//! The person changes feed (spec: person changes and task history,
//! 2026-11-02): what changed for the person since they last looked, and the
//! history of one task, both derived from the swarm event log. The hub reads
//! the log and the records and calls [`build`] and [`history`]; nothing here
//! stores anything and nothing here does I/O.

use serde::{Deserialize, Serialize};

use crate::events::{AnswerVia, SwarmEvent, SwarmEventBody};
use crate::ids::{ChannelName, ProjectName, TaskId};
use crate::model::{Channel, ChannelKind, CloseReason, Sender, Task, TaskStatus};
use crate::ports::{CheckState, ReviewState};
use crate::time::Timestamp;

/// The kinds of event that the changes feed reads, for the log filter.
pub const CHANGE_KINDS: &[&str] = &[
    "task.created",
    "task.status",
    "task.answered",
    "change.updated",
    "spawn.blocked",
    "spawn.unblocked",
    "session.started",
    "session.ended",
];

/// The kinds of event that a task's history reads, for the log filter.
pub const HISTORY_KINDS: &[&str] = &[
    "task.created",
    "task.status",
    "task.answered",
    "message.sent",
];

/// How close two new tasks must be to group into one change.
const GROUP_MS: u64 = 5 * 60 * 1000;

/// The most entries that a task's history returns.
pub const HISTORY_LIMIT: usize = 50;

/// The last change that the person saw: the place of its last event in the
/// log, or the time of it. A sequence number never loses a late event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeMark {
    /// Events with a sequence number above this.
    Seq(u64),
    /// Events after this time.
    At(Timestamp),
}

/// The changes that the person may have missed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangesView {
    /// When the hub built the feed.
    pub at: Timestamp,
    /// Newest first.
    pub changes: Vec<Change>,
}

/// One change, in the terms that a person reads it. A group of new tasks is
/// one change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The sequence number of the last event of the change. The mod keeps it
    /// as the seen mark, so a late event is never lost.
    pub seq: u64,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelName>,
    /// The task of the change, when it is about one. A group of several new
    /// tasks has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    pub verb: ChangeVerb,
    /// One line for the person.
    pub text: String,
    /// Who caused it, when the log names one. The field is always present, so
    /// a reader sees `null` and not a missing key.
    #[serde(default)]
    pub by: Option<Sender>,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeVerb {
    /// One or more tasks were created.
    NewTasks,
    /// A task closed as done.
    Done,
    /// A task closed as done with `release: true`.
    Released,
    /// A saga gate closed as done: the saga is approved.
    GateApproved,
    /// A change request was approved.
    PrApproved,
    /// A change request was merged.
    PrMerged,
    /// The checks of a change request's head failed.
    PrChecksFailed,
    /// A person answered a task's question.
    Answered,
    /// A queued spawn waits for something only a person can fix.
    SpawnBlocked,
    /// The blocked spawn places now.
    SpawnUnblocked,
    /// The owner of a channel lost its session.
    OrchestratorOffline,
    /// The owner of a channel has a session again.
    OrchestratorBack,
}

/// What the rules read: the events of the window, and the records that name
/// a task's title, its change request, and its close.
#[derive(Debug)]
pub struct ChangesInput<'a> {
    pub at: Timestamp,
    /// Oldest first, as the log keeps them.
    pub events: &'a [SwarmEvent],
    pub channels: &'a [Channel],
    /// Every task, with its channel, as `Hub::all_tasks` returns them.
    pub tasks: &'a [(ChannelName, Task)],
}

/// The changes of the events, newest first.
///
/// A group of new tasks never spans the mark: the first create of the window
/// starts a new group, so a group that the person saw is not repeated.
pub fn build(input: &ChangesInput<'_>) -> ChangesView {
    let facts = Facts::new(input);
    let mut changes: Vec<Change> = Vec::new();
    let mut group: Option<NewTaskGroup> = None;
    for event in input.events {
        // A group closes at the first event that is not a create of the same
        // creator and channel, so the feed keeps the order of the log.
        let closes = group.as_mut().is_some_and(|open| !open.accepts(event));
        if closes && let Some(done) = group.take() {
            changes.push(done.finish());
        }
        let Some(channel) = &event.channel else {
            // A session event is about an agent, not a channel. Only the
            // owner of a channel is an orchestrator that the person follows.
            if matches!(
                event.body,
                SwarmEventBody::SessionStarted { .. } | SwarmEventBody::SessionEnded { .. }
            ) && let Some(change) = session_change(&facts, event)
            {
                changes.push(change);
            }
            continue;
        };
        match &event.body {
            SwarmEventBody::TaskCreated { title, by } => {
                let Some(id) = &event.task else {
                    continue;
                };
                match &mut group {
                    Some(open) => open.add(&facts, event, id, title),
                    None => {
                        group = Some(NewTaskGroup::start(&facts, event, channel, *id, title, by))
                    }
                }
            }
            SwarmEventBody::TaskStatus { to, .. } if *to == TaskStatus::Closed => {
                if let Some(change) = closed_change(&facts, event, channel) {
                    changes.push(change);
                }
            }
            SwarmEventBody::ChangeUpdated { .. } => {
                if let Some(change) = change_request_change(&facts, event, channel, &event.body) {
                    changes.push(change);
                }
            }
            SwarmEventBody::TaskAnswered { by, .. } => {
                let Some(id) = &event.task else {
                    continue;
                };
                changes.push(Change {
                    seq: event.seq,
                    at: event.at,
                    project: facts.project(channel),
                    channel: Some(channel.clone()),
                    task: Some(*id),
                    verb: ChangeVerb::Answered,
                    text: format!("answered: {}", facts.title(channel, id)),
                    by: Some(by.clone()),
                });
            }
            SwarmEventBody::SpawnBlocked { reason, action } => {
                if let Some(agent) = &event.agent {
                    changes.push(Change {
                        seq: event.seq,
                        at: event.at,
                        project: facts.project(channel),
                        channel: Some(channel.clone()),
                        task: event.task,
                        verb: ChangeVerb::SpawnBlocked,
                        text: format!("spawn {agent} blocked: {reason}. Unblock: {action}"),
                        by: None,
                    });
                }
            }
            SwarmEventBody::SpawnUnblocked => {
                if let Some(agent) = &event.agent {
                    changes.push(Change {
                        seq: event.seq,
                        at: event.at,
                        project: facts.project(channel),
                        channel: Some(channel.clone()),
                        task: event.task,
                        verb: ChangeVerb::SpawnUnblocked,
                        text: format!("spawn {agent} unblocked"),
                        by: None,
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(open) = group {
        changes.push(open.finish());
    }
    // The log is oldest first; the person reads newest first.
    changes.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| b.at.cmp(&a.at)));
    ChangesView {
        at: input.at,
        changes,
    }
}

/// One group of new tasks: one creator and channel within five minutes.
struct NewTaskGroup {
    by: Sender,
    channel: ChannelName,
    project: Option<ProjectName>,
    at: Timestamp,
    seq: u64,
    tasks: Vec<(TaskId, String)>,
}

impl NewTaskGroup {
    fn start(
        facts: &Facts<'_>,
        event: &SwarmEvent,
        channel: &ChannelName,
        id: TaskId,
        title: &str,
        by: &Sender,
    ) -> Self {
        Self {
            by: by.clone(),
            channel: channel.clone(),
            project: facts.project(channel),
            at: event.at,
            seq: event.seq,
            tasks: vec![(id, title.to_owned())],
        }
    }

    /// True when `event` continues this group: a create of the same creator
    /// and channel within the window.
    fn accepts(&self, event: &SwarmEvent) -> bool {
        if event.channel.as_ref() != Some(&self.channel) {
            return false;
        }
        match &event.body {
            SwarmEventBody::TaskCreated { by, .. } => {
                by == &self.by && event.at.since(self.at).as_millis() <= GROUP_MS as u128
            }
            _ => false,
        }
    }

    fn add(&mut self, _facts: &Facts<'_>, event: &SwarmEvent, id: &TaskId, title: &str) {
        self.at = event.at;
        self.seq = event.seq;
        self.tasks.push((*id, title.to_owned()));
    }

    fn finish(self) -> Change {
        let text = match self.tasks.as_slice() {
            [(_, title)] => format!("new task: {title}"),
            many => {
                let titles: Vec<&str> = many.iter().map(|(_, t)| t.as_str()).collect();
                format!("{} new tasks: {}", many.len(), titles.join(", "))
            }
        };
        Change {
            seq: self.seq,
            at: self.at,
            project: self.project,
            channel: Some(self.channel),
            task: match self.tasks.as_slice() {
                [(id, _)] => Some(*id),
                _ => None,
            },
            verb: ChangeVerb::NewTasks,
            text,
            by: Some(self.by),
        }
    }
}

/// The change of a task that closed as done: a gate is approved, a release is
/// released, and any other task is done. One task gives one change.
fn closed_change(facts: &Facts<'_>, event: &SwarmEvent, channel: &ChannelName) -> Option<Change> {
    let id = event.task?;
    let task = facts.task(channel, &id);
    // A close that is not "done" is not a change for the person.
    if task.and_then(|t| t.close_reason) != Some(CloseReason::Done) {
        return None;
    }
    let title = facts.title(channel, &id);
    let gate = facts
        .channel(channel)
        .is_some_and(|c| c.kind == ChannelKind::Saga && c.gate == Some(id));
    let (verb, text) = if gate {
        (ChangeVerb::GateApproved, format!("approved: {title}"))
    } else if facts.release(channel, &id) {
        (ChangeVerb::Released, format!("released: {title}"))
    } else {
        (
            ChangeVerb::Done,
            match facts.pr_url(channel, &id) {
                Some(url) => format!("done: {title} (PR {url})"),
                None => format!("done: {title}"),
            },
        )
    };
    Some(Change {
        seq: event.seq,
        at: event.at,
        project: facts.project(channel),
        channel: Some(channel.clone()),
        task: Some(id),
        verb,
        text,
        by: None,
    })
}

/// The change of one update of a change request: a merge, a failed check, or
/// an approval, in that order of importance. A repeat of a state that the
/// previous status held is not a change.
fn change_request_change(
    facts: &Facts<'_>,
    event: &SwarmEvent,
    channel: &ChannelName,
    body: &SwarmEventBody,
) -> Option<Change> {
    let SwarmEventBody::ChangeUpdated {
        state,
        head,
        checks,
        review,
        previous,
    } = body
    else {
        return None;
    };
    let id = event.task?;
    let was = previous.as_ref();
    let merged = *state == crate::ports::ChangeState::Merged
        && was.is_none_or(|p| p.state != crate::ports::ChangeState::Merged);
    let failed = match checks {
        CheckState::Fail { .. } => {
            was.is_none_or(|p| !matches!(p.checks, CheckState::Fail { .. }) || &p.head != head)
        }
        _ => false,
    };
    let approved = *state == crate::ports::ChangeState::Open
        && *review == ReviewState::Approved
        && was.is_none_or(|p| p.review != ReviewState::Approved);
    let url = facts.pr_url(channel, &id);
    let pr = url.clone().unwrap_or_else(|| format!("{channel}/{}", id));
    let (verb, text) = if merged {
        (ChangeVerb::PrMerged, format!("PR {pr} merged"))
    } else if failed {
        let names = match checks {
            CheckState::Fail { failed } => failed.join(", "),
            _ => String::new(),
        };
        (
            ChangeVerb::PrChecksFailed,
            if names.is_empty() {
                format!("PR {pr}: checks failed")
            } else {
                format!("PR {pr}: checks failed ({names})")
            },
        )
    } else if approved {
        (ChangeVerb::PrApproved, format!("PR {pr} approved"))
    } else {
        return None;
    };
    Some(Change {
        seq: event.seq,
        at: event.at,
        project: facts.project(channel),
        channel: Some(channel.clone()),
        task: Some(id),
        verb,
        text,
        by: None,
    })
}

/// The change of an orchestrator that lost or found its session. The swarm
/// names an orchestrator as the owner of a channel.
fn session_change(facts: &Facts<'_>, event: &SwarmEvent) -> Option<Change> {
    let agent = event.agent.as_ref()?;
    let channel = facts.owned_channel(agent)?;
    let back = matches!(event.body, SwarmEventBody::SessionStarted { .. });
    Some(Change {
        seq: event.seq,
        at: event.at,
        project: channel.project.clone(),
        channel: Some(channel.name.clone()),
        task: None,
        verb: if back {
            ChangeVerb::OrchestratorBack
        } else {
            ChangeVerb::OrchestratorOffline
        },
        text: if back {
            format!("{agent} back")
        } else {
            format!("{agent} offline")
        },
        by: None,
    })
}

/// The history of one task: its status changes, its claims, its answers, and
/// the messages that name it, oldest first and at most [`HISTORY_LIMIT`].
pub fn history(events: &[SwarmEvent], channel: &ChannelName, id: &TaskId) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::new();
    for event in events {
        if event.channel.as_ref() != Some(channel) || event.task.as_ref() != Some(id) {
            continue;
        }
        let entry = match &event.body {
            SwarmEventBody::TaskCreated { .. } => Some(HistoryEntry {
                seq: event.seq,
                at: event.at,
                kind: HistoryKind::Created,
                status: Some(TaskStatus::Open),
                by: None,
                text: "created".to_owned(),
            }),
            SwarmEventBody::TaskStatus {
                from,
                to,
                claimed_by,
                attention_msg,
            } => {
                // A claim is `open -> in progress` with a claimer. The
                // answer of a question also sets `in progress`, with the
                // claim kept, and that is an answer and not a claim.
                let claimed = claimed_by.is_some() && *from == TaskStatus::Open;
                let mut text = if claimed {
                    format!("claimed by {}", claimed_by.as_ref().expect("checked"))
                } else {
                    format!("{} -> {}", word(*from), word(*to))
                };
                if let Some(msg) = attention_msg {
                    text.push_str(&format!(": {msg}"));
                }
                Some(HistoryEntry {
                    seq: event.seq,
                    at: event.at,
                    kind: if claimed {
                        HistoryKind::Claimed
                    } else {
                        HistoryKind::Status
                    },
                    status: Some(*to),
                    by: if claimed {
                        claimed_by.clone().map(Sender::Agent)
                    } else {
                        None
                    },
                    text,
                })
            }
            SwarmEventBody::TaskAnswered { key, by, via } => Some(HistoryEntry {
                seq: event.seq,
                at: event.at,
                kind: HistoryKind::Answered,
                status: None,
                by: Some(by.clone()),
                text: match via {
                    AnswerVia::Chief => "answered by the chief's approval".to_owned(),
                    AnswerVia::Person => match key {
                        Some(key) => format!("answered by a person (key {key})"),
                        None => "answered by a person".to_owned(),
                    },
                },
            }),
            SwarmEventBody::MessageSent { from, to, text } => Some(HistoryEntry {
                seq: event.seq,
                at: event.at,
                kind: HistoryKind::Message,
                status: None,
                by: Some(from.clone()),
                text: format!("{from} to {to}: {text}"),
            }),
            _ => None,
        };
        if let Some(entry) = entry {
            entries.push(entry);
        }
    }
    if entries.len() > HISTORY_LIMIT {
        entries.drain(..entries.len() - HISTORY_LIMIT);
    }
    entries
}

/// One line of a task's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The sequence number of the event.
    pub seq: u64,
    pub at: Timestamp,
    pub kind: HistoryKind,
    /// The status after a status change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    /// The agent that acted, when one did: the claimer or the sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Sender>,
    pub text: String,
}

/// What one history line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryKind {
    Created,
    Claimed,
    Status,
    Answered,
    Message,
}

/// The records that the rules join to the events.
struct Facts<'a> {
    channels: Vec<&'a Channel>,
    tasks: &'a [(ChannelName, Task)],
}

impl<'a> Facts<'a> {
    fn new(input: &ChangesInput<'a>) -> Self {
        Self {
            channels: input.channels.iter().collect(),
            tasks: input.tasks,
        }
    }

    fn channel(&self, name: &ChannelName) -> Option<&'a Channel> {
        self.channels.iter().copied().find(|c| &c.name == name)
    }

    fn project(&self, name: &ChannelName) -> Option<ProjectName> {
        self.channel(name).and_then(|c| c.project.clone())
    }

    fn task(&self, channel: &ChannelName, id: &TaskId) -> Option<&'a Task> {
        self.tasks
            .iter()
            .find(|(c, t)| c == channel && &t.id == id)
            .map(|(_, t)| t)
    }

    fn title(&self, channel: &ChannelName, id: &TaskId) -> String {
        self.task(channel, id)
            .map(|t| t.title.clone())
            .unwrap_or_else(|| format!("task {id}"))
    }

    fn pr_url(&self, channel: &ChannelName, id: &TaskId) -> Option<String> {
        self.task(channel, id)
            .and_then(|t| t.properties.get(PR_URL))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    }

    /// A release is a task closed as done with `release: true`. The title
    /// prefix is only the fallback.
    fn release(&self, channel: &ChannelName, id: &TaskId) -> bool {
        let Some(task) = self.task(channel, id) else {
            return false;
        };
        match task.properties.get(RELEASE) {
            Some(serde_json::Value::Bool(on)) => *on,
            _ => task.title.starts_with("Release"),
        }
    }

    /// The first channel that `agent` owns, which is how the swarm names an
    /// orchestrator.
    fn owned_channel(&self, agent: &crate::AgentId) -> Option<&'a Channel> {
        self.channels
            .iter()
            .copied()
            .find(|c| c.owner.as_ref() == Some(agent))
    }
}

/// The property of a task that names its change request.
const PR_URL: &str = "pr_url";

/// The property of a task that marks a release.
const RELEASE: &str = "release";

/// A status or a target as a person reads it.
fn word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Open => "open",
        TaskStatus::InProgress => "in progress",
        TaskStatus::NeedsHuman => "needs human",
        TaskStatus::Stuck => "stuck",
        TaskStatus::Closed => "closed",
    }
}
