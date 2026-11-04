//! The board (spec: sagas and epics, "API, MCP, and CLI"). It reads the
//! records, so every test seeds them rather than creating them through a
//! call: the create rules of the channels belong to another task.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::board::{BoardView, Counts, SagaState};
use borant_core::events::{EventFilter, Severity, SwarmEvent, SwarmEventBody};
use borant_core::model::{
    Channel, ChannelKind, CloseReason, Harness, Project, Task, TaskKind, TaskStatus,
};
use borant_core::ports::{EventLog, PortResult, StateStore};
use borant_core::proto::{ApiErrorCode, CtlRequest, Registered, ToolCall};
use borant_core::rules::roles::CollectionConfig;
use borant_core::{ChannelName, CollectionName, ProjectName, TaskId, Timestamp, keys};
use borant_hub::{Hub, HubConfig};
use borant_memory::MemoryEventLog;
use bytes::Bytes;
use common::Fx;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

fn id(n: u8) -> TaskId {
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

fn bytes<T: serde::Serialize>(value: &T) -> Bytes {
    Bytes::from(serde_json::to_vec(value).expect("a record"))
}

fn channel(
    name: &str,
    kind: ChannelKind,
    project: Option<&str>,
    collection: Option<&str>,
) -> Channel {
    Channel {
        name: chan(name),
        owner: None,
        schema: json!({}),
        created: Timestamp(1_000),
        closed: None,
        project: project.map(proj),
        branch: None,
        collection: collection.map(col),
        paused: false,
        kind,
        title: None,
        issue: None,
        gate: None,
        root: None,
    }
}

fn task(n: u8, title: &str, status: TaskStatus, created: i64) -> Task {
    Task {
        id: id(n),
        title: title.into(),
        body: String::new(),
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
        created: Timestamp(created),
        updated: Timestamp(created),
        create_key: None,
    }
}

fn epic(n: u8, title: &str, created: i64) -> Task {
    Task {
        kind: TaskKind::Epic,
        ..task(n, title, TaskStatus::Open, created)
    }
}

fn closed(n: u8, title: &str, reason: CloseReason) -> Task {
    Task {
        status: TaskStatus::Closed,
        close_reason: Some(reason),
        ..task(n, title, TaskStatus::Closed, 1_100)
    }
}

fn child(n: u8, title: &str, parent: TaskId, status: TaskStatus) -> Task {
    Task {
        parent: Some(parent),
        ..task(n, title, status, 1_200)
    }
}

async fn put_channel(fx: &Fx, channel: &Channel) {
    fx.state.seed(&keys::channel(&channel.name), bytes(channel));
}

async fn put_task(fx: &Fx, channel: &str, task: &Task) {
    fx.state
        .seed(&keys::task(&chan(channel), &task.id), bytes(task));
}

async fn put_project(fx: &Fx, project: &Project) {
    fx.state.seed(&keys::project(&project.name), bytes(project));
}

fn project(name: &str, collection: Option<&str>) -> Project {
    Project {
        name: proj(name),
        remote_url: "git://git/demo.git".into(),
        default_branch: "main".into(),
        checkpoint_remote: None,
        setup: None,
        test: None,
        collection: collection.map(col),
        mirror_issues: None,
    }
}

/// A hub with the `personal` collection, its project, a saga of two epics, a
/// standalone epic channel, and a project channel whose task waits for a
/// person. The orchestrator owns the three channels and holds its lease.
async fn one_collection() -> (Fx, Registered) {
    let fx = Fx::with(HubConfig {
        collections: BTreeMap::from([(
            col("personal"),
            CollectionConfig {
                mirror_issues: true,
                ..CollectionConfig::default()
            },
        )]),
        ..HubConfig::default()
    });
    fx.host("h1", &[Harness::Fake]).await;
    let me = fx
        .register_in("h1", Some("orchestrator"), None, Some("personal"))
        .await;
    put_project(&fx, &project("borant", Some("personal"))).await;

    // The saga: the gate closed as done, so it is approved.
    let mut saga = channel(
        "elastic",
        ChannelKind::Saga,
        Some("borant"),
        Some("personal"),
    );
    saga.owner = Some(me.agent.clone());
    saga.title = Some("Elastic compute".into());
    saga.issue = Some("ENG-1".into());
    saga.gate = Some(id(1));
    put_channel(&fx, &saga).await;
    put_task(
        &fx,
        "elastic",
        &closed(1, "Approve the scope and the epics", CloseReason::Done),
    )
    .await;
    put_task(
        &fx,
        "elastic",
        &Task {
            issue: Some("ENG-2".into()),
            properties: serde_json::from_value(json!({
                "branch": "stack/elastic",
                "pr_url": "https://github.com/x/y/pull/7",
            }))
            .unwrap(),
            ..epic(2, "the provisioner", 1_100)
        },
    )
    .await;
    put_task(
        &fx,
        "elastic",
        &child(3, "the adapter", id(2), TaskStatus::Open),
    )
    .await;
    put_task(&fx, "elastic", &epic(4, "the client", 1_200)).await;

    // A standalone epic channel.
    let mut metrics = channel(
        "metrics",
        ChannelKind::Epic,
        Some("borant"),
        Some("personal"),
    );
    metrics.owner = Some(me.agent.clone());
    metrics.root = Some(id(5));
    put_channel(&fx, &metrics).await;
    put_task(&fx, "metrics", &epic(5, "Metrics", 1_100)).await;
    put_task(
        &fx,
        "metrics",
        &child(6, "the gauge", id(5), TaskStatus::Open),
    )
    .await;

    // The plain tasks of the project channel.
    let mut roadmap = channel(
        "roadmap",
        ChannelKind::Project,
        Some("borant"),
        Some("personal"),
    );
    roadmap.owner = Some(me.agent.clone());
    put_channel(&fx, &roadmap).await;
    put_task(
        &fx,
        "roadmap",
        &task(7, "the docs", TaskStatus::Open, 1_100),
    )
    .await;
    put_task(
        &fx,
        "roadmap",
        &Task {
            blocked_by: vec![id(7)],
            ..task(8, "the guide", TaskStatus::Open, 1_200)
        },
    )
    .await;
    put_task(
        &fx,
        "roadmap",
        &Task {
            status: TaskStatus::Stuck,
            updated: Timestamp(1_500),
            ..task(9, "the migration", TaskStatus::Stuck, 1_300)
        },
    )
    .await;
    (fx, me)
}

#[tokio::test]
async fn board_shows_a_collection() {
    let (fx, _me) = one_collection().await;
    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();

    // The whole view, so the JSON that the mod reads is pinned.
    assert_eq!(
        v,
        json!({
            "at": 1_000_000,
            "collections": [{
                "collection": "personal",
                "projects": [{
                    "project": "borant",
                    "mirror_issues": true,
                    "orchestrators": [{
                        "agent": "orchestrator",
                        "online": true,
                        "busy": false,
                        "hung": false,
                        "host": "h1",
                    }],
                    "sagas": [{
                        "channel": "elastic",
                        "title": "Elastic compute",
                        "issue": "ENG-1",
                        "state": "approved",
                        "gate": id(1),
                        "epics": [{
                            "channel": "elastic",
                            "task": id(2),
                            "title": "the provisioner",
                            "status": "open",
                            "issue": "ENG-2",
                            "branch": "stack/elastic",
                            "pr_url": "https://github.com/x/y/pull/7",
                            "counts": {
                                "open": 1,
                                "ready": 1,
                                "running": 0,
                                "blocked": 0,
                                "stuck": 0,
                                "needs_human": 0,
                                "done": 0,
                            },
                        }, {
                            "channel": "elastic",
                            "task": id(4),
                            "title": "the client",
                            "status": "open",
                            "issue": null,
                            "branch": null,
                            "pr_url": null,
                            "counts": {
                                "open": 0,
                                "ready": 0,
                                "running": 0,
                                "blocked": 0,
                                "stuck": 0,
                                "needs_human": 0,
                                "done": 0,
                            },
                        }],
                        "done": 0,
                        "total": 2,
                    }],
                    "epics": [{
                        "channel": "metrics",
                        "task": id(5),
                        "title": "Metrics",
                        "status": "open",
                        "issue": null,
                        "branch": null,
                        "pr_url": null,
                        "counts": {
                            "open": 1,
                            "ready": 1,
                            "running": 0,
                            "blocked": 0,
                            "stuck": 0,
                            "needs_human": 0,
                            "done": 0,
                        },
                    }],
                    "tasks": {
                        "open": 2,
                        "ready": 1,
                        "running": 0,
                        "blocked": 1,
                        "stuck": 1,
                        "needs_human": 0,
                        "done": 0,
                    },
                }],
                "needs_you": [{
                    "project": "borant",
                    "channel": "roadmap",
                    "task": id(9),
                    "title": "the migration",
                    "status": "stuck",
                    "attention_msg": null,
                    "since": 1_500,
                    "gate": false,
                    "owner": "orchestrator",
                }],
            }],
        })
    );

    // And the same view parses back into the types that the CLI renders.
    let board: BoardView = serde_json::from_value(v).unwrap();
    assert_eq!(
        board.collections[0].projects[0].sagas[0].state,
        SagaState::Approved
    );
    assert_eq!(
        board.collections[0].projects[0].tasks,
        Counts {
            open: 2,
            ready: 1,
            blocked: 1,
            stuck: 1,
            ..Counts::default()
        }
    );
}

#[tokio::test]
async fn board_without_a_collection_lists_every_collection() {
    let fx = Fx::with(HubConfig {
        collections: BTreeMap::from([
            (
                col("personal"),
                CollectionConfig {
                    mirror_issues: true,
                    ..CollectionConfig::default()
                },
            ),
            (col("work"), CollectionConfig::default()),
        ]),
        ..HubConfig::default()
    });
    put_project(&fx, &project("borant", Some("personal"))).await;
    put_project(&fx, &project("website", Some("work"))).await;
    // A project with no collection gets its own board, last.
    put_project(&fx, &project("misc", None)).await;
    put_channel(
        &fx,
        &channel("api", ChannelKind::Project, Some("website"), Some("work")),
    )
    .await;

    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let board: BoardView = serde_json::from_value(v).unwrap();
    let names: Vec<Option<&str>> = board
        .collections
        .iter()
        .map(|c| c.collection.as_ref().map(CollectionName::as_str))
        .collect();
    assert_eq!(names, [Some("personal"), Some("work"), None]);
    assert_eq!(board.collections[1].projects[0].project, proj("website"));
    assert_eq!(board.collections[2].projects[0].project, proj("misc"));
}

#[tokio::test]
async fn board_agent_sees_only_its_collection() {
    let (fx, me) = one_collection().await;
    let personal = fx
        .call(
            Some(&me),
            ToolCall::Board {
                collection: None,
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let board: BoardView = serde_json::from_value(personal).unwrap();
    assert_eq!(board.collections.len(), 1);
    assert_eq!(board.collections[0].collection, Some(col("personal")));

    // Its own collection, named, is the same board.
    let mine = fx
        .call(
            Some(&me),
            ToolCall::Board {
                collection: Some(col("personal")),
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(mine, serde_json::to_value(&board).unwrap());

    // Another collection is denied.
    let err = fx
        .call(
            Some(&me),
            ToolCall::Board {
                collection: Some(col("work")),
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied);
}

#[tokio::test]
async fn board_writes_nothing() {
    let (fx, _me) = one_collection().await;
    let before: Vec<_> = fx
        .state
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.revision, e.value.len()))
        .collect();

    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(v["collections"].as_array().map(Vec::len), Some(1));

    let after: Vec<_> = fx
        .state
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.revision, e.value.len()))
        .collect();
    assert_eq!(before, after);
}

/// The config of the personal collection.
fn personal_cfg() -> HubConfig {
    HubConfig {
        collections: BTreeMap::from([(
            col("personal"),
            CollectionConfig {
                mirror_issues: true,
                ..CollectionConfig::default()
            },
        )]),
        ..HubConfig::default()
    }
}

/// A hub of the personal collection and no records yet.
fn personal_hub() -> Fx {
    Fx::with(personal_cfg())
}

/// The last read of the PR watch, as it stores it under `pr.<channel>.<task>`.
fn put_change(fx: &Fx, channel: &str, task: TaskId, status: Value) {
    fx.state.seed(
        &format!("pr.{channel}.{task}"),
        bytes(&json!({ "last": status })),
    );
}

/// A task status change in the event log, which is where `since` comes from.
async fn put_status_event(fx: &Fx, channel: &str, task: TaskId, at: i64, to: TaskStatus) {
    let event = SwarmEvent::hub(
        Timestamp(at),
        Severity::Info,
        SwarmEventBody::TaskStatus {
            from: TaskStatus::Open,
            to,
            claimed_by: None,
            attention_msg: None,
        },
    )
    .about_channel(chan(channel))
    .about_task(task);
    fx.events.publish(&event).await.expect("a status event");
}

#[tokio::test]
async fn board_task_lines_carry_the_start_state_the_since_and_the_pr() {
    let fx = Fx::with(personal_cfg());
    fx.host("h1", &[Harness::Fake]).await;
    put_project(&fx, &project("mirror", Some("personal"))).await;
    put_channel(
        &fx,
        &channel(
            "mirror-api",
            ChannelKind::Project,
            Some("mirror"),
            Some("personal"),
        ),
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &task(1, "the guide", TaskStatus::Open, 1_000),
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            blocked_by: vec![id(1)],
            ..task(2, "the skills", TaskStatus::Open, 1_100)
        },
    )
    .await;
    // A task that carries a change request, whose status last changed after
    // its own update.
    put_task(
        &fx,
        "mirror-api",
        &Task {
            status: TaskStatus::InProgress,
            claimed_by: Some("w1".parse().unwrap()),
            properties: serde_json::from_value(json!({
                "pr_url": "https://github.com/x/y/pull/1",
            }))
            .unwrap(),
            updated: Timestamp(3_000),
            ..task(3, "the client", TaskStatus::InProgress, 1_200)
        },
    )
    .await;
    put_status_event(&fx, "mirror-api", id(3), 4_000, TaskStatus::InProgress).await;
    put_change(
        &fx,
        "mirror-api",
        id(3),
        json!({
            "state": "open",
            "head": "abc123",
            "checks": {"fail": {"failed": ["ci"]}},
            "review": "none",
            "checks_passed": 1,
            "checks_total": 3,
            "review_requested": true,
        }),
    );
    // Two tasks of the project closed as done: one inside the default day,
    // one long before it.
    put_task(
        &fx,
        "mirror-api",
        &Task {
            updated: Timestamp(999_000),
            ..closed(4, "the last one", CloseReason::Done)
        },
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            updated: Timestamp(-100_000_000),
            ..closed(5, "the old one", CloseReason::Done)
        },
    )
    .await;

    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let project = &v["collections"][0]["projects"][0];
    let lines = project["task_lines"].as_array().expect("the task lines");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["title"], "the guide");
    assert_eq!(lines[0]["start"], "ready");
    assert!(lines[0]["waits_on"].is_null());
    assert_eq!(lines[1]["title"], "the skills");
    assert_eq!(lines[1]["start"], "later");
    assert_eq!(lines[1]["waits_on"], "the guide");
    assert_eq!(lines[2]["status"], "in_progress");
    assert!(lines[2]["start"].is_null());
    assert_eq!(lines[2]["since"], 4_000);
    assert_eq!(lines[2]["version"], 3_000);
    let pr = &lines[2]["pr"];
    assert_eq!(pr["url"], "https://github.com/x/y/pull/1");
    assert_eq!(pr["state"], "open");
    assert_eq!(pr["checks_passed"], 1);
    assert_eq!(pr["checks_total"], 3);
    assert_eq!(pr["review_requested"], true);

    // The default cutoff is one day before the read.
    assert_eq!(project["done_since"], 1);
    let recent = project["recent_done"].as_array().expect("the done lines");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0]["title"], "the last one");
    assert_eq!(recent[1]["title"], "the old one");
}

#[tokio::test]
async fn board_without_tasks_is_the_counts_alone() {
    let fx = personal_hub();
    put_project(&fx, &project("mirror", Some("personal"))).await;
    put_channel(
        &fx,
        &channel(
            "mirror-api",
            ChannelKind::Project,
            Some("mirror"),
            Some("personal"),
        ),
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &task(1, "the guide", TaskStatus::Open, 1_000),
    )
    .await;

    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: false,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let project = &v["collections"][0]["projects"][0];
    // An older reader gets exactly the keys that it knows.
    assert!(project.get("task_lines").is_none(), "{project}");
    assert!(project.get("done_since").is_none(), "{project}");
    assert!(project.get("recent_done").is_none(), "{project}");
    assert_eq!(project["tasks"]["open"], 1);
}

#[tokio::test]
async fn board_done_since_takes_the_time_of_the_call() {
    let fx = personal_hub();
    put_project(&fx, &project("mirror", Some("personal"))).await;
    put_channel(
        &fx,
        &channel(
            "mirror-api",
            ChannelKind::Project,
            Some("mirror"),
            Some("personal"),
        ),
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            updated: Timestamp(999_000),
            ..closed(1, "the last one", CloseReason::Done)
        },
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            updated: Timestamp(-100_000_000),
            ..closed(2, "the old one", CloseReason::Done)
        },
    )
    .await;

    // A cutoff before both counts both.
    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: Some(Timestamp(-200_000_000)),
            },
        )
        .await
        .unwrap();
    assert_eq!(v["collections"][0]["projects"][0]["done_since"], 2);

    // The default day counts the newer one alone.
    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(v["collections"][0]["projects"][0]["done_since"], 1);
}

/// The event log of a hub that records what each read asked for and returned,
/// so a test sees that a board pass scans only the newer status events.
#[derive(Debug)]
struct CountingLog {
    inner: MemoryEventLog,
    /// The filter of each read, and the number of events that it returned.
    reads: Mutex<Vec<(EventFilter, Arc<AtomicUsize>)>>,
}

impl CountingLog {
    fn new() -> Self {
        Self {
            inner: MemoryEventLog::new(),
            reads: Mutex::new(Vec::new()),
        }
    }

    /// The filter and the event count of each read so far, oldest first.
    fn reads(&self) -> Vec<(EventFilter, usize)> {
        self.reads
            .lock()
            .expect("the reads")
            .iter()
            .map(|(filter, count)| (filter.clone(), count.load(Ordering::SeqCst)))
            .collect()
    }
}

#[async_trait::async_trait]
impl EventLog for CountingLog {
    async fn publish(&self, event: &SwarmEvent) -> PortResult<()> {
        self.inner.publish(event).await
    }

    async fn read(
        &self,
        filter: &EventFilter,
        follow: bool,
    ) -> PortResult<BoxStream<'static, SwarmEvent>> {
        let count = Arc::new(AtomicUsize::new(0));
        self.reads
            .lock()
            .expect("the reads")
            .push((filter.clone(), count.clone()));
        let stream = self.inner.read(filter, follow).await?;
        Ok(stream
            .map(move |event| {
                count.fetch_add(1, Ordering::SeqCst);
                event
            })
            .boxed())
    }
}

#[tokio::test]
async fn board_task_lines_read_only_the_newer_status_events() {
    let log = Arc::new(CountingLog::new());
    let fx = Fx::with_log(personal_cfg(), log.clone());
    put_project(&fx, &project("mirror", Some("personal"))).await;
    put_channel(
        &fx,
        &channel(
            "mirror-api",
            ChannelKind::Project,
            Some("mirror"),
            Some("personal"),
        ),
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            status: TaskStatus::InProgress,
            updated: Timestamp(500_000),
            ..task(1, "the guide", TaskStatus::InProgress, 1_000)
        },
    )
    .await;
    put_task(
        &fx,
        "mirror-api",
        &Task {
            updated: Timestamp(600_000),
            ..task(2, "the skills", TaskStatus::Open, 1_100)
        },
    )
    .await;
    // One status change before the first pass.
    put_status_event(&fx, "mirror-api", id(1), 900_000, TaskStatus::InProgress).await;

    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let lines = v["collections"][0]["projects"][0]["task_lines"]
        .as_array()
        .expect("the task lines")
        .clone();
    assert_eq!(lines[0]["since"], 900_000);
    // The first pass reads the whole log.
    assert_eq!(
        log.reads(),
        [(
            EventFilter {
                kinds: vec!["task.status".to_owned()],
                ..EventFilter::default()
            },
            1
        )]
    );

    // One more status change, stamped after the first pass as a real publish
    // is. The hub's clock moves with it.
    fx.clock.advance(Duration::from_millis(5_000));
    put_status_event(&fx, "mirror-api", id(2), 1_005_000, TaskStatus::Open).await;
    let v = fx
        .call(
            None,
            ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: None,
            },
        )
        .await
        .unwrap();
    let cached = v["collections"][0]["projects"][0]["task_lines"]
        .as_array()
        .expect("the task lines")
        .clone();
    assert_eq!(cached[0]["since"], 900_000);
    assert_eq!(cached[1]["since"], 1_005_000);
    // The second pass asks for the events from a millisecond before the
    // first one ran, and the log holds only the new one of them.
    let reads = log.reads();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[1].0.since, Some(Timestamp(999_999)));
    assert_eq!(reads[1].1, 1);

    // A hub that reads the whole log once answers with the same lines.
    let fresh = Hub::new(
        Arc::new(fx.state.clone()),
        Arc::new(fx.bus.clone()),
        Arc::new(fx.sessions.clone()),
        fx.clock.clone(),
        personal_cfg(),
    )
    .with_events(log.clone());
    let v = fresh
        .handle(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::Board {
                collection: None,
                tasks: true,
                done_since: None,
            },
        })
        .await
        .into_result()
        .unwrap();
    let full = v["collections"][0]["projects"][0]["task_lines"]
        .as_array()
        .expect("the task lines")
        .clone();
    assert_eq!(cached, full);
    // The fresh hub read both events.
    let reads = log.reads();
    assert_eq!(reads.len(), 3);
    assert_eq!(reads[2].0.since, None);
    assert_eq!(reads[2].1, 2);
}
