//! The issue mirror: the hub sweeps the sagas, epics, and tasks of a
//! mirrored project out to an issue tracker, one way, and writes the key
//! back (spec: issue mirror). The tracker is the in-memory one, and the
//! fixture seeds every record: the create rules of a channel belong to
//! another task.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use borant_core::model::{
    Channel, ChannelKind, CloseReason, Project, Task, TaskKind, TaskPatch, TaskStatus,
};
use borant_core::ports::{IssueState, MessageBus, PortError, PortErrorKind, StateStore};
use borant_core::proto::ToolCall;
use borant_core::rules::mirror::no_target_message;
use borant_core::rules::roles::CollectionConfig;
use borant_core::{AgentId, ChannelName, CollectionName, ProjectName, TaskId, Timestamp, keys};
use borant_hub::mirror::MirrorRecord;
use borant_hub::{HubConfig, IssueMirror};
use borant_memory::{IssueCall, MemoryIssues};
use bytes::Bytes;
use common::Fx;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::json;

const PR: &str = "https://example.test/borant/pull/7";

fn id(n: u32) -> TaskId {
    format!("01J{n:023}").parse().unwrap()
}

fn chan(s: &str) -> ChannelName {
    s.parse().unwrap()
}

fn proj(s: &str) -> ProjectName {
    s.parse().unwrap()
}

fn col(s: &str) -> CollectionName {
    s.parse().unwrap()
}

fn boss() -> AgentId {
    "boss".parse().unwrap()
}

fn bytes<T: serde::Serialize>(value: &T) -> Bytes {
    Bytes::from(serde_json::to_vec(value).expect("a record"))
}

fn channel(name: &str, kind: ChannelKind) -> Channel {
    Channel {
        name: chan(name),
        owner: Some(boss()),
        schema: json!({}),
        created: Timestamp(1_000),
        closed: None,
        project: Some(proj("web")),
        branch: None,
        collection: Some(col("work")),
        paused: false,
        kind,
        title: None,
        issue: None,
        gate: None,
        root: None,
    }
}

fn task(n: u32, title: &str, status: TaskStatus) -> Task {
    Task {
        id: id(n),
        title: title.into(),
        body: format!("The body of {title}."),
        status,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        issue_target: None,
        properties: Default::default(),
        created: Timestamp(1_000 + i64::from(n)),
        updated: Timestamp(1_000 + i64::from(n)),
        create_key: None,
    }
}

/// A task that the mirror copies, with a change request and no team.
fn with_pr(n: u32, title: &str, status: TaskStatus) -> Task {
    Task {
        properties: json!({"pr_url": PR})
            .as_object()
            .cloned()
            .unwrap_or_default(),
        ..task(n, title, status)
    }
}

/// A task that the mirror copies: it has a change request and a team.
fn mirrored_task(n: u32, title: &str) -> Task {
    Task {
        issue_target: Some("T".into()),
        ..with_pr(n, title, TaskStatus::Open)
    }
}

struct Mirror {
    fx: Fx,
    issues: MemoryIssues,
    cfg: IssueMirror,
}

/// A hub whose `work` collection turns the mirror on, and the tracker that
/// the tests read.
fn mirror_with(
    collections: BTreeMap<CollectionName, CollectionConfig>,
    every: Duration,
    max_writes: usize,
) -> Mirror {
    let issues = MemoryIssues::new();
    let cfg = IssueMirror {
        tracker: Arc::new(issues.clone()),
        every,
        max_writes,
    };
    let fx = Fx::with(HubConfig {
        collections,
        mirror: Some(cfg.clone()),
        ..HubConfig::default()
    });
    Mirror { fx, issues, cfg }
}

fn mirror(max_writes: usize) -> Mirror {
    // The tests run the sweep themselves.
    mirror_every(Duration::from_secs(3600), max_writes)
}

fn mirror_every(every: Duration, max_writes: usize) -> Mirror {
    mirror_with(
        BTreeMap::from([(
            col("work"),
            CollectionConfig {
                mirror_issues: true,
                ..CollectionConfig::default()
            },
        )]),
        every,
        max_writes,
    )
}

impl Mirror {
    async fn sweep(&self) -> usize {
        self.fx
            .hub
            .sweep_mirror(&self.cfg)
            .await
            .expect("the sweep")
    }

    fn seed_project(&self, name: &str, collection: Option<&str>, mirror_issues: Option<bool>) {
        let project = Project {
            name: proj(name),
            remote_url: "git://git/demo.git".into(),
            default_branch: "main".into(),
            checkpoint_remote: None,
            setup: None,
            test: None,
            collection: collection.map(col),
            mirror_issues,
        };
        self.fx
            .state
            .seed(&keys::project(&project.name), bytes(&project));
    }

    fn seed_channel(&self, c: &Channel) {
        self.fx.state.seed(&keys::channel(&c.name), bytes(c));
    }

    fn seed_task(&self, channel: &ChannelName, t: &Task) {
        self.fx.state.seed(&keys::task(channel, &t.id), bytes(t));
    }

    fn seed_record(&self, id: &str, rec: &MirrorRecord) {
        self.fx.state.seed(id, bytes(rec));
    }

    async fn task(&self, channel: &ChannelName, id: &TaskId) -> Task {
        let doc = self
            .fx
            .state
            .get(&keys::task(channel, id))
            .await
            .unwrap()
            .expect("the task");
        serde_json::from_slice(&doc.value).expect("a task")
    }

    async fn channel(&self, name: &ChannelName) -> Channel {
        let doc = self
            .fx
            .state
            .get(&keys::channel(name))
            .await
            .unwrap()
            .expect("the channel");
        serde_json::from_slice(&doc.value).expect("a channel")
    }

    async fn record(&self, id: &str) -> Option<MirrorRecord> {
        let doc = self.fx.state.get(id).await.unwrap()?;
        Some(serde_json::from_slice(&doc.value).expect("a record"))
    }

    /// Changes a task as a person, which a record never sees as a race of its
    /// own.
    async fn update(&self, channel: &ChannelName, id: &TaskId, patch: TaskPatch) -> Task {
        let value = self
            .fx
            .call(
                None,
                ToolCall::TaskUpdate {
                    channel: channel.clone(),
                    id: *id,
                    patch,
                },
            )
            .await
            .expect("the update");
        serde_json::from_value(value).expect("a task")
    }

    async fn close_task(&self, channel: &ChannelName, id: &TaskId, reason: CloseReason) {
        self.fx
            .call(
                None,
                ToolCall::TaskClose {
                    channel: channel.clone(),
                    id: *id,
                    reason,
                    message: "done".into(),
                },
            )
            .await
            .expect("the close");
    }

    async fn close_channel(&self, name: &ChannelName) {
        self.fx
            .call(None, ToolCall::ChannelClose { name: name.clone() })
            .await
            .expect("the channel close");
    }
}

/// The KV key of an item: `mirror.<channel>.<task>`, or `mirror.<channel>`.
fn record_key(channel: &ChannelName, id: Option<&TaskId>) -> String {
    match id {
        Some(id) => format!("mirror.{channel}.{id}"),
        None => format!("mirror.{channel}"),
    }
}

/// The `work` collection's project `web`, which the mirror copies.
fn web(m: &Mirror) {
    m.seed_project("web", Some("work"), None);
}

fn creates(calls: &[IssueCall]) -> Vec<&borant_core::ports::IssueWrite> {
    calls
        .iter()
        .filter_map(|c| match c {
            IssueCall::Create(w) => Some(w),
            IssueCall::Update { .. } => None,
        })
        .collect()
}

/// The states of the updates that reached an issue, in order.
fn states(calls: &[IssueCall], key: &str) -> Vec<IssueState> {
    calls
        .iter()
        .filter_map(|c| match c {
            IssueCall::Update { issue, write, .. } if issue.as_str() == key => Some(write.state),
            _ => None,
        })
        .collect()
}

/// The mirror creates one issue, then writes only when the item changes.
#[tokio::test]
async fn mirror_creates_once_then_updates_on_change() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(1, "the task");
    m.seed_task(&c.name, &t);

    assert_eq!(m.sweep().await, 1);
    let calls = m.issues.calls();
    let IssueCall::Create(w) = &calls[0] else {
        panic!("a create")
    };
    assert_eq!(w.target.as_deref(), Some("T"));
    assert_eq!(w.parent, None);
    assert_eq!(w.state, IssueState::Todo);
    assert_eq!(w.title, "the task");
    assert!(
        w.body
            .contains(&format!("Mirrored from Borant (`work-web/{}`).", t.id))
    );
    assert!(w.body.ends_with(&format!("PR: {PR}")));

    let linked = m.task(&c.name, &t.id).await;
    assert_eq!(linked.issue.as_deref(), Some("T-1"));
    let rec = m
        .record(&record_key(&c.name, Some(&t.id)))
        .await
        .expect("the record");
    assert_eq!(rec.issue.as_deref(), Some("T-1"));
    assert!(rec.owned);
    assert!(rec.linked);
    assert_eq!(m.issues.count(), 1);

    // An unchanged item costs no call.
    assert_eq!(m.sweep().await, 0);
    assert_eq!(m.issues.calls().len(), 1);

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("a new title".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1);
    let calls = m.issues.calls();
    let IssueCall::Update { issue, write, full } = calls.last().expect("a call") else {
        panic!("an update")
    };
    assert_eq!(issue, "T-1");
    assert_eq!(write.title, "a new title");
    assert!(*full, "an owned issue gets every field");
}

/// An adopted issue gets the state only, and a title change costs no call.
#[tokio::test]
async fn mirror_adopted_issue_gets_state_only() {
    let m = mirror(50);
    web(&m);
    m.issues.seed("T-9", "T");
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = Task {
        issue: Some("T-9".into()),
        ..task(2, "adopted", TaskStatus::Open)
    };
    m.seed_task(&c.name, &t);

    assert_eq!(m.sweep().await, 1);
    let calls = m.issues.calls();
    let IssueCall::Update { issue, full, .. } = &calls[0] else {
        panic!("an update")
    };
    assert_eq!(issue, "T-9");
    assert!(!full, "an adopted issue gets the state only");
    assert!(creates(&calls).is_empty());
    let rec = m
        .record(&record_key(&c.name, Some(&t.id)))
        .await
        .expect("the record");
    assert!(!rec.owned);
    assert!(rec.linked);

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("a new title".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 0, "a title edit is none of its business");

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            status: Some(TaskStatus::InProgress),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1);
    let calls = m.issues.calls();
    let IssueCall::Update { full, write, .. } = calls.last().expect("a call") else {
        panic!("an update")
    };
    assert!(!full);
    assert_eq!(write.state, IssueState::Started);
}

/// A crash between the create and the record makes no second issue: the
/// create key is the same one.
#[tokio::test]
async fn mirror_crash_between_create_and_link_makes_no_duplicate() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(3, "the task");
    m.seed_task(&c.name, &t);

    // The create reaches the tracker, then the store goes away.
    let state = m.fx.state.clone();
    let once = Arc::new(AtomicBool::new(false));
    m.issues.on_create(move |_| {
        if !once.swap(true, Ordering::SeqCst) {
            state.set_available(false);
        }
    });
    let _ = m.fx.hub.sweep_mirror(&m.cfg).await;
    m.fx.state.set_available(true);

    assert_eq!(m.sweep().await, 1);
    let calls = m.issues.calls();
    let keys: Vec<&str> = calls
        .iter()
        .filter_map(|c| match c {
            IssueCall::Create(w) => Some(w.key.as_str()),
            IssueCall::Update { .. } => None,
        })
        .collect();
    assert_eq!(keys.len(), 2, "the create is retried");
    assert_eq!(keys[0], keys[1], "with the same key");
    assert_eq!(m.issues.count(), 1, "and makes one issue");
    assert!(m.task(&c.name, &t.id).await.issue.is_some());
}

/// A record that says `linked: false` finishes its link with no tracker call.
#[tokio::test]
async fn mirror_record_without_link_relinks() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(4, "the task");
    m.seed_task(&c.name, &t);
    assert_eq!(m.sweep().await, 1);

    // The crash happened after the record and before the link.
    let rk = record_key(&c.name, Some(&t.id));
    let mut rec = m.record(&rk).await.expect("the record");
    rec.linked = false;
    m.seed_record(&rk, &rec);
    let key = m.task(&c.name, &t.id).await.issue.expect("the link");
    let mut unlinked = m.task(&c.name, &t.id).await;
    unlinked.issue = None;
    m.seed_task(&c.name, &unlinked);

    let before = m.issues.calls().len();
    assert_eq!(m.sweep().await, 0, "no tracker call");
    assert_eq!(m.issues.calls().len(), before);
    assert_eq!(
        m.task(&c.name, &t.id).await.issue.as_deref(),
        Some(key.as_str())
    );
    assert!(m.record(&rk).await.expect("the record").linked);
}

/// A task with no team goes `needs_human` once, and gets its status back when
/// the team is set.
#[tokio::test]
async fn mirror_no_team_raises_needs_human_once_and_restores() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = with_pr(5, "needs a team", TaskStatus::InProgress);
    m.seed_task(&c.name, &t);

    assert_eq!(m.sweep().await, 0);
    let asked = m.task(&c.name, &t.id).await;
    assert_eq!(asked.status, TaskStatus::NeedsHuman);
    assert_eq!(
        asked.attention_msg.as_deref(),
        Some(no_target_message(&c.name, &t.id).as_str())
    );
    let rk = record_key(&c.name, Some(&t.id));
    let rec = m.record(&rk).await.expect("the record");
    assert_eq!(rec.stopped, Some(borant_hub::mirror::Stopped::NoTarget));
    assert_eq!(rec.was, Some(TaskStatus::InProgress));
    assert_eq!(rec.issue, None);

    // The same question is not asked twice.
    let revision =
        m.fx.state
            .get(&keys::task(&c.name, &t.id))
            .await
            .unwrap()
            .unwrap()
            .revision;
    assert_eq!(m.sweep().await, 0);
    assert_eq!(
        m.fx.state
            .get(&keys::task(&c.name, &t.id))
            .await
            .unwrap()
            .unwrap()
            .revision,
        revision
    );

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            issue_target: Some("T".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1);
    let restored = m.task(&c.name, &t.id).await;
    assert_eq!(restored.status, TaskStatus::InProgress);
    assert_eq!(restored.attention_msg, None);
    assert_eq!(creates(&m.issues.calls()).len(), 1);
}

/// A gate that already waits for approval keeps its question; the team
/// question is appended, and goes again when the team is set.
#[tokio::test]
async fn mirror_no_team_on_a_waiting_gate_keeps_its_question() {
    let m = mirror(50);
    web(&m);
    let mut c = channel("work-saga", ChannelKind::Saga);
    let gate = Task {
        status: TaskStatus::NeedsHuman,
        attention_msg: Some("Which scope?".into()),
        ..task(6, "scope the saga", TaskStatus::NeedsHuman)
    };
    c.gate = Some(gate.id);
    m.seed_channel(&c);
    m.seed_task(&c.name, &gate);

    assert_eq!(m.sweep().await, 0);
    let asked = m.task(&c.name, &gate.id).await;
    let message = no_target_message(&c.name, &gate.id);
    assert_eq!(
        asked.attention_msg.as_deref(),
        Some(format!("Which scope?\n\n{message}").as_str())
    );
    assert_eq!(asked.status, TaskStatus::NeedsHuman);
    let rec = m
        .record(&record_key(&c.name, None))
        .await
        .expect("the record");
    assert_eq!(rec.was, Some(TaskStatus::NeedsHuman));

    m.update(
        &c.name,
        &gate.id,
        TaskPatch {
            issue_target: Some("T".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1, "the saga gets its issue");
    let back = m.task(&c.name, &gate.id).await;
    assert_eq!(back.attention_msg.as_deref(), Some("Which scope?"));
    assert_eq!(back.status, TaskStatus::NeedsHuman);
}

/// A child with no team of its own takes the team of its parent issue.
#[tokio::test]
async fn mirror_sub_issue_without_target_uses_parent_team() {
    let m = mirror(50);
    web(&m);
    m.issues.seed("T-1", "T");
    let mut c = channel("work-saga", ChannelKind::Saga);
    let gate = task(7, "scope the saga", TaskStatus::Open);
    let epic = Task {
        kind: TaskKind::Epic,
        ..task(8, "the epic", TaskStatus::Open)
    };
    c.gate = Some(gate.id);
    c.issue = Some("T-1".into());
    m.seed_channel(&c);
    m.seed_task(&c.name, &gate);
    m.seed_task(&c.name, &epic);

    assert_eq!(m.sweep().await, 2);
    let calls = m.issues.calls();
    let created = creates(&calls);
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].title, "the epic");
    assert_eq!(created[0].target, None, "the parent issue's team answers");
    assert_eq!(created[0].parent.as_deref(), Some("T-1"));

    let key = m.task(&c.name, &epic.id).await.issue.expect("the link");
    assert_eq!(
        m.issues.issue(&key).expect("the issue").1.as_deref(),
        Some("T")
    );
}

/// A task with no change request is out, and comes in when its PR opens.
#[tokio::test]
async fn mirror_task_gets_issue_when_pr_url_appears() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = Task {
        issue_target: Some("T".into()),
        ..task(9, "the task", TaskStatus::Open)
    };
    m.seed_task(&c.name, &t);

    assert_eq!(m.sweep().await, 0);
    assert!(m.issues.calls().is_empty());

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            properties: json!({"pr_url": PR}).as_object().cloned(),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1);
    assert_eq!(creates(&m.issues.calls()).len(), 1);
}

/// A `not_found` stops the item and tells the owner once.
#[tokio::test]
async fn mirror_not_found_is_told_once() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(10, "the task");
    m.seed_task(&c.name, &t);
    let mut inbox: BoxStream<'static, borant_core::model::Envelope> =
        m.fx.bus
            .subscribe(&borant_core::subjects::agent(&boss()))
            .await
            .unwrap();
    assert_eq!(m.sweep().await, 1);

    m.issues.remove("T-1");
    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("renamed".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1, "the update finds the issue gone");
    let rec = m
        .record(&record_key(&c.name, Some(&t.id)))
        .await
        .expect("the record");
    assert_eq!(rec.stopped, Some(borant_hub::mirror::Stopped::NotFound));
    let told = tokio::time::timeout(Duration::from_millis(200), inbox.next())
        .await
        .ok()
        .flatten()
        .expect("one message to the owner");
    assert!(told.text.contains("gone from the tracker"), "{}", told.text);

    let calls = m.issues.calls().len();
    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("renamed again".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 0, "a stopped item gets no write");
    assert_eq!(m.issues.calls().len(), calls);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), inbox.next())
            .await
            .is_err(),
        "the owner hears it once"
    );
}

/// A refused create is told once, and retried when the item changes.
#[tokio::test]
async fn mirror_invalid_is_told_once_until_the_hash_changes() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(11, "the task");
    m.seed_task(&c.name, &t);
    let mut inbox: BoxStream<'static, borant_core::model::Envelope> =
        m.fx.bus
            .subscribe(&borant_core::subjects::agent(&boss()))
            .await
            .unwrap();

    m.issues
        .fail_next(PortError::new(PortErrorKind::Invalid, "unknown team"));
    assert_eq!(m.sweep().await, 1);
    let rk = record_key(&c.name, Some(&t.id));
    let rec = m.record(&rk).await.expect("the record");
    assert_eq!(rec.issue, None);
    assert!(rec.refused.is_some(), "the refused hash is kept");
    let told = tokio::time::timeout(Duration::from_millis(200), inbox.next())
        .await
        .ok()
        .flatten()
        .expect("one message to the owner");
    assert!(told.text.contains("refused"), "{}", told.text);

    assert_eq!(m.sweep().await, 0, "the same record is not sent again");

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("a new title".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 1, "a changed record is retried");
    assert_eq!(creates(&m.issues.calls()).len(), 2);
    assert!(m.task(&c.name, &t.id).await.issue.is_some());
}

/// A `denied` pauses the mirror until the next hub start, and the alert says
/// so once.
#[tokio::test]
async fn mirror_denied_pauses_until_restart() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(12, "the task");
    m.seed_task(&c.name, &t);

    m.issues
        .fail_next(PortError::new(PortErrorKind::Denied, "the key was revoked"));
    assert_eq!(m.sweep().await, 1, "the call that was refused");
    assert_eq!(m.sweep().await, 0, "the pause holds");

    m.fx.advance(60);
    m.fx.hub.tick().await.expect("the tick");
    assert!(
        m.fx.state
            .get("alert.mirror_refused.tracker")
            .await
            .unwrap()
            .is_some(),
        "the alert fires for the person"
    );
}

/// An unreachable tracker is a warning, and the next sweep retries.
#[tokio::test]
async fn mirror_unavailable_retries_next_sweep() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(13, "the task");
    m.seed_task(&c.name, &t);

    m.issues.set_available(false);
    assert_eq!(m.sweep().await, 1);
    let rk = record_key(&c.name, Some(&t.id));
    assert!(m.record(&rk).await.is_none(), "nothing is recorded");

    m.issues.set_available(true);
    assert_eq!(m.sweep().await, 1);
    assert_eq!(
        creates(&m.issues.calls()).len(),
        1,
        "the failed call never reached the tracker"
    );
    assert_eq!(m.issues.count(), 1);
    assert!(m.task(&c.name, &t.id).await.issue.is_some());
}

/// Clearing the link stops the mirror for the item.
#[tokio::test]
async fn mirror_unlinked_stops() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(14, "the task");
    m.seed_task(&c.name, &t);
    assert_eq!(m.sweep().await, 1);

    let cleared = m
        .update(
            &c.name,
            &t.id,
            TaskPatch {
                issue: Some(String::new()),
                ..TaskPatch::default()
            },
        )
        .await;
    assert_eq!(cleared.issue, None);
    assert_eq!(m.sweep().await, 0);
    let rec = m
        .record(&record_key(&c.name, Some(&t.id)))
        .await
        .expect("the record");
    assert_eq!(rec.stopped, Some(borant_hub::mirror::Stopped::Unlinked));

    m.update(
        &c.name,
        &t.id,
        TaskPatch {
            title: Some("a new title".into()),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 0);
    assert_eq!(m.issues.calls().len(), 1, "no further call");
}

/// The cap counts calls, and parents go first, so a child waits for its
/// parent instead of making its own call.
#[tokio::test]
async fn mirror_max_writes_caps_parents_first() {
    let m = mirror(2);
    web(&m);
    let mut saga = channel("work-saga", ChannelKind::Saga);
    let gate = Task {
        issue_target: Some("T".into()),
        ..task(15, "scope the saga", TaskStatus::Open)
    };
    saga.gate = Some(gate.id);
    m.seed_channel(&saga);
    m.seed_task(&saga.name, &gate);
    let one = Task {
        kind: TaskKind::Epic,
        updated: Timestamp(1_010),
        ..task(16, "epic one", TaskStatus::Open)
    };
    let two = Task {
        kind: TaskKind::Epic,
        updated: Timestamp(1_020),
        ..task(17, "epic two", TaskStatus::Open)
    };
    m.seed_task(&saga.name, &one);
    m.seed_task(&saga.name, &two);
    let child = Task {
        parent: Some(two.id),
        ..mirrored_task(18, "the task")
    };
    m.seed_task(&saga.name, &child);

    assert_eq!(m.sweep().await, 2);
    let titles: Vec<String> = creates(&m.issues.calls())
        .iter()
        .map(|w| w.title.clone())
        .collect();
    assert_eq!(titles, vec!["work-saga", "epic one"]);

    assert_eq!(m.sweep().await, 2);
    let titles: Vec<String> = creates(&m.issues.calls())
        .iter()
        .skip(2)
        .map(|w| w.title.clone())
        .collect();
    assert_eq!(titles, vec!["epic two", "the task"]);
}

/// Closed history is not backfilled.
#[tokio::test]
async fn mirror_closed_item_without_record_makes_no_call() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let closed = Task {
        status: TaskStatus::Closed,
        close_reason: Some(CloseReason::Done),
        ..mirrored_task(19, "the old task")
    };
    m.seed_task(&c.name, &closed);

    let mut saga = channel("work-saga", ChannelKind::Saga);
    let gate = Task {
        status: TaskStatus::Closed,
        close_reason: Some(CloseReason::Done),
        ..task(20, "scope the saga", TaskStatus::Closed)
    };
    saga.gate = Some(gate.id);
    saga.closed = Some(Timestamp(2_000));
    m.seed_channel(&saga);
    m.seed_task(&saga.name, &gate);

    assert_eq!(m.sweep().await, 0);
    assert!(m.issues.calls().is_empty());
    assert!(
        m.record(&record_key(&c.name, Some(&id(19))))
            .await
            .is_none()
    );
    assert!(m.record(&record_key(&saga.name, None)).await.is_none());
}

/// A project with the mirror off makes no call, whether its collection has
/// the mirror on or not.
#[tokio::test]
async fn mirror_off_project_makes_no_calls() {
    let m = mirror(50);
    // The collection is on, the project turns it off.
    m.seed_project("web", Some("work"), Some(false));
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    m.seed_task(&c.name, &mirrored_task(21, "the task"));

    // A collection that never turns the mirror on.
    let other = Channel {
        project: Some(proj("other")),
        collection: Some(col("personal")),
        ..channel("personal-other", ChannelKind::Project)
    };
    m.seed_project("other", Some("personal"), None);
    m.seed_channel(&other);
    m.seed_task(&other.name, &mirrored_task(22, "another task"));

    assert_eq!(m.sweep().await, 0);
    assert!(m.issues.calls().is_empty());
    assert!(
        m.record(&record_key(&c.name, Some(&id(21))))
            .await
            .is_none()
    );
}

/// The link write reads the task again after it loses a race, so a concurrent
/// update is kept and the task still ends linked.
#[tokio::test]
async fn mirror_link_write_retries_a_lost_race() {
    let m = mirror(50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let t = mirrored_task(23, "the task");
    m.seed_task(&c.name, &t);

    // Another writer renames the task while the create runs: its revision
    // moves, so the link must not write over it with a stale one.
    let state = m.fx.state.clone();
    let key = keys::task(&c.name, &t.id);
    let renamed = Task {
        title: "the newer title".into(),
        ..t.clone()
    };
    let once = Arc::new(AtomicBool::new(false));
    m.issues.on_create(move |_| {
        if !once.swap(true, Ordering::SeqCst) {
            state.seed(&key, bytes(&renamed));
        }
    });

    assert_eq!(m.sweep().await, 1);
    let linked = m.task(&c.name, &t.id).await;
    assert_eq!(linked.issue.as_deref(), Some("T-1"));
    assert_eq!(
        linked.title, "the newer title",
        "the concurrent write stays"
    );
    assert_eq!(m.issues.count(), 1);
}

/// A saga is `backlog` until its gate closes as done, `started` once a child
/// moves, and `done` after the channel closes.
#[tokio::test]
async fn mirror_saga_state_follows_the_gate_and_children() {
    let m = mirror(50);
    web(&m);
    let mut saga = channel("work-saga", ChannelKind::Saga);
    let gate = Task {
        issue_target: Some("T".into()),
        ..task(24, "scope the saga", TaskStatus::Open)
    };
    saga.gate = Some(gate.id);
    m.seed_channel(&saga);
    m.seed_task(&saga.name, &gate);
    let epic = Task {
        kind: TaskKind::Epic,
        ..task(25, "the epic", TaskStatus::Open)
    };
    m.seed_task(&saga.name, &epic);

    assert_eq!(m.sweep().await, 2);
    let channel_key = m.channel(&saga.name).await.issue.expect("the saga issue");
    let created = m.issues.calls();
    assert_eq!(
        creates(&created)
            .iter()
            .find(|w| w.title == "work-saga")
            .map(|w| w.state),
        Some(IssueState::Backlog)
    );

    // The gate closes as done: the saga is approved.
    m.close_task(&saga.name, &gate.id, CloseReason::Done).await;
    assert_eq!(m.sweep().await, 1, "the saga changes state");
    assert_eq!(
        states(&m.issues.calls(), &channel_key),
        vec![IssueState::Todo]
    );

    // A child of the epic moves: the work has begun.
    let child = Task {
        parent: Some(epic.id),
        ..mirrored_task(26, "the child")
    };
    m.seed_task(&saga.name, &child);
    m.update(
        &saga.name,
        &child.id,
        TaskPatch {
            status: Some(TaskStatus::InProgress),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 3, "the child, the epic, and the saga");
    assert!(states(&m.issues.calls(), &channel_key).contains(&IssueState::Started));

    // Everything closes, then the channel.
    m.close_task(&saga.name, &child.id, CloseReason::Done).await;
    m.close_task(&saga.name, &epic.id, CloseReason::Done).await;
    m.close_channel(&saga.name).await;
    m.sweep().await;
    assert_eq!(
        *states(&m.issues.calls(), &channel_key)
            .last()
            .expect("a state"),
        IssueState::Done
    );
}

/// The loop of the hub sweeps on its own task, and its shutdown ends it.
#[tokio::test]
async fn mirror_loop_sweeps_on_its_tick() {
    let m = mirror_every(Duration::from_millis(10), 50);
    web(&m);
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    m.seed_task(&c.name, &mirrored_task(28, "the task"));

    let shutdown = tokio_util::sync::CancellationToken::new();
    let hub = m.fx.hub.clone();
    let token = shutdown.clone();
    let handle = tokio::spawn(async move { hub.mirror_loop(token).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        while m.issues.calls().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the loop swept");
    shutdown.cancel();
    handle.await.expect("the loop ends");
    assert_eq!(creates(&m.issues.calls()).len(), 1);
}

/// Two items that link the same issue: only the owner writes its state, so
/// the issue never takes turns between their states. The other rolls up.
#[tokio::test]
async fn mirror_shared_link_writes_from_one_owner() {
    let m = mirror(50);
    web(&m);
    m.issues.seed("T-9", "T");
    let c = channel("work-web", ChannelKind::Project);
    m.seed_channel(&c);
    let owner = Task {
        issue: Some("T-9".into()),
        ..task(2, "the issue itself", TaskStatus::InProgress)
    };
    let sharer = Task {
        issue: Some("T-9".into()),
        ..task(3, "a part of it", TaskStatus::Open)
    };
    m.seed_task(&c.name, &owner);
    m.seed_task(&c.name, &sharer);

    assert_eq!(m.sweep().await, 1, "one write for one issue");
    assert_eq!(states(&m.issues.calls(), "T-9"), vec![IssueState::Started]);
    assert!(
        m.record(&record_key(&c.name, Some(&owner.id)))
            .await
            .is_some()
    );
    assert!(
        m.record(&record_key(&c.name, Some(&sharer.id)))
            .await
            .is_none(),
        "the sharer keeps no record"
    );

    m.update(
        &c.name,
        &sharer.id,
        TaskPatch {
            status: Some(TaskStatus::InProgress),
            ..TaskPatch::default()
        },
    )
    .await;
    assert_eq!(m.sweep().await, 0, "the sharer's change writes nothing");
}
