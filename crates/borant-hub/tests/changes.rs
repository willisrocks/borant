//! The PR watch: the hub polls the change request of a task and tells the
//! worker and the owner what changed (spec phase 6, "PR watch").

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use borant_core::events::{EventFilter, SwarmEvent};
use borant_core::model::{ChannelKind, Envelope, Harness, NewTask, Task};
use borant_core::ports::changes::{
    ChangeRequests, ChangeState, ChangeStatus, CheckState, ReviewState,
};
use borant_core::ports::{EventLog, MessageBus, PortResult, StateStore};
use borant_core::proto::{Registered, ToolCall};
use borant_core::{ChannelName, TaskId, subjects};
use borant_hub::ChangeWatch;
use borant_memory::MemoryChanges;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

use common::Fx;

const URL: &str = "https://example.test/borant/pull/7";

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn status(state: ChangeState, head: &str, checks: CheckState, review: ReviewState) -> ChangeStatus {
    ChangeStatus {
        state,
        head: head.to_owned(),
        checks,
        review,
    }
}

fn watch(changes: Arc<MemoryChanges>) -> ChangeWatch {
    ChangeWatch {
        changes,
        // The tests run the pass themselves.
        every: Duration::from_secs(3600),
    }
}

/// A host, the owner of a channel, a task with a `pr_url`, and its worker.
async fn setup(fx: &Fx) -> (Registered, Registered, TaskId) {
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
        },
    )
    .await
    .unwrap();
    let task: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "watch the PR".into(),
                    properties: json!({"pr_url": URL})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let worker = fx.register("h1", Some("w1"), Some("api")).await;
    fx.call(
        Some(&worker),
        ToolCall::TaskClaim {
            channel: api(),
            id: task.id,
        },
    )
    .await
    .expect("the worker claims the task");
    (owner, worker, task.id)
}

/// The next direct message on a subject, or `None` when none comes.
async fn next_message(stream: &mut BoxStream<'static, Envelope>) -> Option<Envelope> {
    tokio::time::timeout(Duration::from_millis(200), stream.next())
        .await
        .ok()?
}

async fn no_message(stream: &mut BoxStream<'static, Envelope>, what: &str) {
    if let Some(env) = next_message(stream).await {
        panic!("{what}: {}", env.text);
    }
}

/// Each head tells the worker once about a failure, the owner once about green
/// and once about the merge; no check reported is not a pass.
#[tokio::test]
async fn changes_tell_once_per_head() {
    let fx = Fx::new();
    let (owner, worker, _task) = setup(&fx).await;
    let mut to_worker = fx
        .bus
        .subscribe(&subjects::agent(&worker.agent))
        .await
        .unwrap();
    let mut to_owner = fx
        .bus
        .subscribe(&subjects::agent(&owner.agent))
        .await
        .unwrap();
    let changes = Arc::new(MemoryChanges::new());
    let watch = watch(changes.clone());

    // No check reported for the head is not a pass.
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::None {},
            ReviewState::None,
        ),
    );
    fx.hub.poll_changes(&watch).await.unwrap();
    no_message(&mut to_owner, "a change with no check told the owner green").await;

    // A failed check tells the worker, once per head.
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::Fail {
                failed: vec!["clippy".into()],
            },
            ReviewState::None,
        ),
    );
    fx.hub.poll_changes(&watch).await.unwrap();
    let env = next_message(&mut to_worker)
        .await
        .expect("the worker hears about the failed check");
    assert!(env.text.contains("clippy"), "{}", env.text);
    assert!(env.text.contains("aaaa1111"), "{}", env.text);
    fx.hub.poll_changes(&watch).await.unwrap();
    no_message(&mut to_worker, "the same head told the worker twice").await;

    // A green head tells the owner, once per head.
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::Pass {},
            ReviewState::Approved,
        ),
    );
    fx.hub.poll_changes(&watch).await.unwrap();
    let env = next_message(&mut to_owner)
        .await
        .expect("the owner hears about green checks");
    assert!(env.text.contains("green"), "{}", env.text);
    fx.hub.poll_changes(&watch).await.unwrap();
    no_message(&mut to_owner, "green was told twice for one head").await;

    // A new head starts its own round of messages.
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "bbbb2222",
            CheckState::Fail {
                failed: vec!["test".into()],
            },
            ReviewState::None,
        ),
    );
    fx.hub.poll_changes(&watch).await.unwrap();
    let env = next_message(&mut to_worker)
        .await
        .expect("the worker hears about the new head");
    assert!(env.text.contains("bbbb2222"), "{}", env.text);
    assert!(env.text.contains("test"), "{}", env.text);

    // The merge tells the owner once.
    changes.set(
        URL,
        status(
            ChangeState::Merged,
            "bbbb2222",
            CheckState::Pass {},
            ReviewState::Approved,
        ),
    );
    fx.hub.poll_changes(&watch).await.unwrap();
    let env = next_message(&mut to_owner)
        .await
        .expect("the owner hears about the merge");
    assert!(env.text.contains("merged"), "{}", env.text);
    fx.hub.poll_changes(&watch).await.unwrap();
    no_message(&mut to_owner, "the merge was told twice").await;
}

/// A change request that is gone, or a task that is closed, is not polled.
#[tokio::test]
async fn changes_skip_what_is_not_open() {
    let fx = Fx::new();
    let (owner, _worker, task) = setup(&fx).await;
    let mut to_owner = fx
        .bus
        .subscribe(&subjects::agent(&owner.agent))
        .await
        .unwrap();
    let changes = Arc::new(MemoryChanges::new());
    let watch = watch(changes.clone());
    // The adapter knows no URL at all: NotFound is not a failure of the watch.
    assert_eq!(fx.hub.poll_changes(&watch).await.unwrap(), 0);
    // A closed task is not watched, even with a green change.
    fx.call(
        Some(&owner),
        ToolCall::TaskClose {
            channel: api(),
            id: task,
            reason: borant_core::model::CloseReason::Done,
            message: "done".into(),
        },
    )
    .await
    .unwrap();
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::Pass {},
            ReviewState::Approved,
        ),
    );
    assert_eq!(fx.hub.poll_changes(&watch).await.unwrap(), 0);
    no_message(&mut to_owner, "a closed task told the owner").await;
}

/// A change-request source that answers after a delay, as a forge over a slow
/// link does.
#[derive(Debug)]
struct Slow {
    inner: MemoryChanges,
    delay: Duration,
}

#[async_trait::async_trait]
impl ChangeRequests for Slow {
    async fn status(&self, url: &str) -> PortResult<ChangeStatus> {
        tokio::time::sleep(self.delay).await;
        self.inner.status(url).await
    }
}

/// The watch runs outside the request lock: a poll that waits on the forge
/// does not delay a heartbeat, so no lease expires.
#[tokio::test]
async fn changes_a_slow_adapter_does_not_delay_a_lease() {
    let fx = Fx::new();
    let (_owner, _worker, _task) = setup(&fx).await;
    let inner = MemoryChanges::new();
    inner.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::Pass {},
            ReviewState::Approved,
        ),
    );
    let watch = ChangeWatch {
        changes: Arc::new(Slow {
            inner,
            delay: Duration::from_secs(5),
        }),
        every: Duration::from_secs(3600),
    };
    let hub = fx.hub.clone();
    let polling = tokio::spawn(async move { hub.poll_changes(&watch).await });
    // The poll is now inside the forge call.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    fx.heartbeat("h1", &[]).await;
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(500),
        "a heartbeat waited {took:?} for the PR watch"
    );
    polling.abort();
}

/// The watch key holds the status it saw, at the key of the spec.
#[tokio::test]
async fn changes_keep_the_last_state_of_each_change() {
    let fx = Fx::new();
    let (_owner, _worker, task) = setup(&fx).await;
    let changes = Arc::new(MemoryChanges::new());
    let watch = watch(changes.clone());
    let expected = status(
        ChangeState::Open,
        "aaaa1111",
        CheckState::Pass {},
        ReviewState::Approved,
    );
    changes.set(URL, expected.clone());
    fx.hub.poll_changes(&watch).await.unwrap();
    let entry = fx
        .state
        .get(&format!("pr.api.{task}"))
        .await
        .unwrap()
        .expect("the key of the change");
    let stored: Value = serde_json::from_slice(&entry.value).expect("json");
    assert_eq!(stored["last"]["head"], "aaaa1111", "{stored}");
    assert_eq!(stored["last"]["state"], "open", "{stored}");
    assert_eq!(stored["told_green"], true, "{stored}");
}

/// A log that keeps every event the hub publishes.
#[derive(Debug, Default)]
struct Captured(std::sync::Mutex<Vec<SwarmEvent>>);

#[async_trait::async_trait]
impl EventLog for Captured {
    async fn publish(&self, event: &SwarmEvent) -> PortResult<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
        Ok(())
    }

    async fn read(
        &self,
        _filter: &EventFilter,
        _follow: bool,
    ) -> PortResult<BoxStream<'static, SwarmEvent>> {
        Ok(futures::stream::empty().boxed())
    }
}

/// Each change of a change request publishes one `change.updated` event.
#[tokio::test]
async fn changes_publish_one_change_updated_event() {
    use borant_core::model::HostInfo;
    use borant_core::proto::{CtlRequest, RegisterSession};
    use borant_core::{ManualClock, SessionId, Timestamp};
    use borant_hub::Hub;
    use borant_memory::{MemoryBus, MemorySession, MemoryState};

    let bus = MemoryBus::new();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let captured = Arc::new(Captured::default());
    let hub = Arc::new(
        Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(bus.clone()),
            Arc::new(MemorySession::new()),
            clock,
            borant_hub::HubConfig::default(),
        )
        .with_events(captured.clone()),
    );
    hub.handle(CtlRequest::RegisterHost {
        info: HostInfo {
            name: "h1".parse().unwrap(),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 4,
            ..Default::default()
        },
    })
    .await;
    let owner: Registered = serde_json::from_value(
        hub.handle(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: "h1".parse().unwrap(),
            harness: Harness::Fake,
            agent: Some("boss".parse().unwrap()),
            ..Default::default()
        }))
        .await
        .into_result()
        .unwrap(),
    )
    .unwrap();
    let actor = borant_core::proto::Actor {
        agent: owner.agent.clone(),
        session: owner.session,
        token: owner.token,
    };
    let call = |call: ToolCall| {
        let hub = hub.clone();
        let actor = actor.clone();
        async move {
            hub.handle(CtlRequest::Call {
                actor: Some(actor),
                key: None,
                call,
            })
            .await
            .into_result()
            .unwrap()
        }
    };
    call(ToolCall::ChannelCreate {
        name: api(),
        schema: None,
        project: None,
        branch: None,
        kind: ChannelKind::Project,
        title: None,
        issue: None,
        issue_target: None,
    })
    .await;
    call(ToolCall::TaskCreate {
        channel: api(),
        task: NewTask {
            title: "watch the PR".into(),
            properties: json!({"pr_url": URL})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        },
    })
    .await;
    let changes = Arc::new(MemoryChanges::new());
    let watch = watch(changes.clone());
    changes.set(
        URL,
        status(
            ChangeState::Open,
            "aaaa1111",
            CheckState::Pass {},
            ReviewState::Approved,
        ),
    );
    hub.poll_changes(&watch).await.unwrap();
    // The same status again is no change, so no second event.
    hub.poll_changes(&watch).await.unwrap();
    let events = captured.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let changed: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e.body,
                borant_core::events::SwarmEventBody::ChangeUpdated { .. }
            )
        })
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].channel, Some(api()), "{:?}", changed[0]);
}
