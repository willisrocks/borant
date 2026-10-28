//! An in-memory issue tracker, for tests. It makes the key `<team>-<n>` and
//! the URL `https://issues.example.test/<key>`, it can fail the next call
//! once, and it can run a hook inside a create.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use borant_core::ports::{IssueRef, IssueState, IssueTracker, IssueWrite, PortError, PortResult};

use crate::Availability;

/// The host of the URLs that this adapter makes.
const URL: &str = "https://issues.example.test";

/// A hook that runs inside `create`, before it returns.
type CreateHook = Arc<dyn Fn(&IssueWrite) + Send + Sync>;

/// The calls that a test made, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueCall {
    Create(IssueWrite),
    Update {
        issue: String,
        write: IssueWrite,
        full: bool,
    },
}

#[derive(Clone)]
pub struct MemoryIssues {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MemoryIssues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryIssues")
            .field("issues", &self.count())
            .finish_non_exhaustive()
    }
}

struct Inner {
    state: Mutex<State>,
    /// The error that the next call returns, once.
    fail: Mutex<Option<PortError>>,
    /// Runs inside `create`, before it returns.
    on_create: Mutex<Option<CreateHook>>,
    available: Availability,
}

#[derive(Debug, Default)]
struct State {
    next: u64,
    /// Every issue, by key.
    issues: BTreeMap<String, Issue>,
    /// The create key of a write, to the key of the issue it made.
    created: BTreeMap<String, String>,
    calls: Vec<IssueCall>,
}

#[derive(Debug, Clone)]
struct Issue {
    reference: IssueRef,
    team: Option<String>,
    write: IssueWrite,
}

impl Default for MemoryIssues {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryIssues {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                fail: Mutex::new(None),
                on_create: Mutex::new(None),
                available: Availability::default(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    /// An issue that exists before the mirror runs, for an adoption.
    pub fn seed(&self, key: &str, target: &str) -> IssueRef {
        let mut state = self.lock();
        let reference = IssueRef {
            key: key.to_owned(),
            url: format!("{URL}/{key}"),
        };
        state.issues.insert(
            key.to_owned(),
            Issue {
                reference: reference.clone(),
                team: Some(target.to_owned()),
                write: IssueWrite {
                    key: key.to_owned(),
                    target: Some(target.to_owned()),
                    parent: None,
                    title: String::new(),
                    body: String::new(),
                    state: IssueState::Todo,
                },
            },
        );
        // A later create takes a number past every seeded one.
        if let Some(n) = key.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()) {
            state.next = state.next.max(n);
        }
        reference
    }

    /// An issue that is gone from the tracker.
    pub fn remove(&self, key: &str) {
        self.lock().issues.remove(key);
    }

    /// Fails the next call once, with this error.
    pub fn fail_next(&self, error: PortError) {
        *self.inner.fail.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
    }

    /// Runs `hook` inside every create, before it returns.
    pub fn on_create(&self, hook: impl Fn(&IssueWrite) + Send + Sync + 'static) {
        *self
            .inner
            .on_create
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hook));
    }

    pub fn calls(&self) -> Vec<IssueCall> {
        self.lock().calls.clone()
    }

    /// The issue, its team, and the write that last reached it.
    pub fn issue(&self, key: &str) -> Option<(IssueRef, Option<String>, IssueWrite)> {
        self.lock().issues.get(&issue_key_of(key)).map(|issue| {
            (
                issue.reference.clone(),
                issue.team.clone(),
                issue.write.clone(),
            )
        })
    }

    pub fn count(&self) -> usize {
        self.lock().issues.len()
    }

    fn take_failure(&self) -> PortResult<()> {
        match self
            .inner
            .fail
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn hook(&self) -> Option<CreateHook> {
        self.inner
            .on_create
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl IssueTracker for MemoryIssues {
    async fn create(&self, w: &IssueWrite) -> PortResult<IssueRef> {
        self.inner.available.check()?;
        let hook = self.hook();
        let reference = {
            let mut state = self.lock();
            state.calls.push(IssueCall::Create(w.clone()));
            self.take_failure()?;
            // A create with the same key is the same issue.
            if let Some(key) = state.created.get(&w.key) {
                return match state.issues.get(key) {
                    Some(issue) => Ok(issue.reference.clone()),
                    None => Err(PortError::not_found(format!("issue {key}"))),
                };
            }
            let team = match (&w.target, &w.parent) {
                (Some(target), _) if !target.is_empty() => Some(target.clone()),
                (_, Some(parent)) => {
                    let key = issue_key_of(parent);
                    Some(
                        state
                            .issues
                            .get(&key)
                            .and_then(|issue| issue.team.clone())
                            .ok_or_else(|| PortError::invalid(format!("no issue {key}")))?,
                    )
                }
                _ => {
                    return Err(PortError::invalid(
                        "a create needs a team or a parent issue",
                    ));
                }
            };
            let team = team.unwrap_or_default();
            state.next += 1;
            let key = format!("{team}-{}", state.next);
            let reference = IssueRef {
                url: format!("{URL}/{key}"),
                key: key.clone(),
            };
            state.issues.insert(
                key.clone(),
                Issue {
                    reference: reference.clone(),
                    team: Some(team),
                    write: w.clone(),
                },
            );
            state.created.insert(w.key.clone(), key);
            reference
        };
        if let Some(hook) = hook {
            hook(w);
        }
        Ok(reference)
    }

    async fn update(&self, issue: &str, w: &IssueWrite, full: bool) -> PortResult<IssueRef> {
        self.inner.available.check()?;
        let mut state = self.lock();
        state.calls.push(IssueCall::Update {
            issue: issue.to_owned(),
            write: w.clone(),
            full,
        });
        self.take_failure()?;
        let key = issue_key_of(issue);
        let Some(issue) = state.issues.get_mut(&key) else {
            return Err(PortError::not_found(format!("issue {key}")));
        };
        // An adopted issue gets the state only, and an owned one every field.
        if full {
            issue.write.title = w.title.clone();
            issue.write.body = w.body.clone();
            issue.write.parent = w.parent.clone();
            issue.write.target = w.target.clone();
        }
        issue.write.state = w.state;
        Ok(issue.reference.clone())
    }
}

/// The key of an issue that the caller names by a key or a URL.
fn issue_key_of(issue: &str) -> String {
    issue.rsplit('/').next().unwrap_or(issue).trim().to_owned()
}
