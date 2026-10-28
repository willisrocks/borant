//! The board (spec: sagas and epics, "API, MCP, and CLI"). It reads the
//! records, so every test seeds them rather than creating them through a
//! call: the create rules of the channels belong to another task.

mod common;

use std::collections::BTreeMap;

use borant_core::board::{BoardView, Counts, SagaState};
use borant_core::model::{
    Channel, ChannelKind, CloseReason, Harness, Project, Task, TaskKind, TaskStatus,
};
use borant_core::ports::StateStore;
use borant_core::proto::{ApiErrorCode, Registered, ToolCall};
use borant_core::rules::roles::CollectionConfig;
use borant_core::{ChannelName, CollectionName, ProjectName, TaskId, Timestamp, keys};
use borant_hub::HubConfig;
use bytes::Bytes;
use common::Fx;
use serde_json::json;

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
        .call(None, ToolCall::Board { collection: None })
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
        .call(None, ToolCall::Board { collection: None })
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
        .call(Some(&me), ToolCall::Board { collection: None })
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
        .call(None, ToolCall::Board { collection: None })
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
