//! The person's reads of the log: the changes feed and a task's history
//! (person changes feed spec).

mod common;

use borant_core::Clock;
use borant_core::changes::ChangeMark;
use borant_core::events::{Severity, SwarmEvent, SwarmEventBody};
use borant_core::model::{ChannelKind, CloseReason, Harness, NewTask, Sender, Target, TaskStatus};
use borant_core::ports::EventLog;
use borant_core::proto::{Registered, ToolCall};
use borant_core::{ChannelName, TaskId};
use common::Fx;
use serde_json::{Value, json};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// A host, a channel owner, and the channel.
async fn with_channel(fx: &Fx) -> Registered {
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
    owner
}

async fn create(fx: &Fx, who: &Registered, title: &str) -> TaskId {
    let v = fx
        .call(
            Some(who),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    ..NewTask::default()
                },
            },
        )
        .await
        .unwrap();
    v["id"].as_str().unwrap().parse().expect("a task id")
}

async fn get_task(fx: &Fx, id: TaskId) -> Value {
    fx.call(None, ToolCall::TaskGet { channel: api(), id })
        .await
        .unwrap()
}

/// A message event of the host, as `send --task` writes one.
async fn message(fx: &Fx, id: TaskId, text: &str) {
    let event = SwarmEvent::host(
        fx.clock.now(),
        &"h1".parse().unwrap(),
        Severity::Info,
        SwarmEventBody::MessageSent {
            from: Sender::Agent("boss".parse().unwrap()),
            to: Target::Agent("w1".parse().unwrap()),
            text: text.into(),
        },
    )
    .about_channel(api())
    .about_task(id);
    fx.events
        .publish(&event)
        .await
        .expect("the event is stored");
}

#[tokio::test]
async fn a_task_history_shows_its_claims_answers_and_named_messages() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    fx.host("h2", &[Harness::Fake]).await;
    let worker = fx.register("h2", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "Add the export button").await;
    let other = create(&fx, &owner, "Another task").await;
    fx.call(
        Some(&worker),
        ToolCall::TaskClaim {
            channel: api(),
            id: task,
        },
    )
    .await
    .unwrap();
    let _seen = get_task(&fx, task).await;
    fx.call(
        Some(&worker),
        ToolCall::TaskUpdate {
            channel: api(),
            id: task,
            patch: borant_core::model::TaskPatch {
                status: Some(TaskStatus::NeedsHuman),
                attention_msg: Some("which format?".into()),
                ..borant_core::model::TaskPatch::default()
            },
        },
    )
    .await
    .unwrap();
    let seen = get_task(&fx, task).await;
    assert_eq!(seen["status"], json!("needs_human"));
    fx.call(
        None,
        ToolCall::Answer {
            channel: api(),
            task,
            text: "CSV".into(),
            expect_updated: serde_json::from_value(seen["updated"].clone()).unwrap(),
            key: Some("q1".into()),
        },
    )
    .await
    .unwrap();
    message(&fx, task, "Use RFC 4180 for the header.").await;
    // A message of another task is not part of this history.
    message(&fx, other, "not this task").await;

    let v = fx
        .call(
            None,
            ToolCall::TaskHistory {
                channel: api(),
                id: task,
            },
        )
        .await
        .unwrap();
    let texts: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        vec![
            "created",
            "claimed by w1",
            "in progress -> needs human: which format?",
            "needs human -> in progress",
            "answered by a person (key q1)",
            "boss to w1: Use RFC 4180 for the header.",
        ],
        "oldest first, and the other task's message is left out"
    );
    let kinds: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "created", "claimed", "status", "status", "answered", "message"
        ]
    );
    assert_eq!(v["channel"], json!("api"));
}

#[tokio::test]
async fn a_changes_read_shows_new_tasks_and_a_done_task_after_a_mark() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let first = create(&fx, &owner, "Add the export button").await;
    create(&fx, &owner, "Fix the CSV header").await;
    fx.call(
        Some(&owner),
        ToolCall::TaskClose {
            channel: api(),
            id: first,
            reason: CloseReason::Done,
            message: "shipped".into(),
        },
    )
    .await
    .unwrap();

    let v = fx
        .call(None, ToolCall::Changes { since: None })
        .await
        .unwrap();
    let changes = v["changes"].as_array().expect("the changes");
    let verbs: Vec<&str> = changes
        .iter()
        .map(|c| c["verb"].as_str().unwrap())
        .collect();
    assert_eq!(
        verbs,
        vec!["done", "new_tasks"],
        "newest first: the close, then the two creates in one group"
    );
    assert_eq!(
        changes[1]["text"],
        json!("2 new tasks: Add the export button, Fix the CSV header")
    );
    assert!(changes[0]["text"].as_str().unwrap().starts_with("done: "));
    assert_eq!(changes[0]["channel"], json!("api"));

    // The mark is the sequence number of the newest change, and nothing was
    // written after it.
    let mark = changes[0]["seq"].as_u64().expect("a sequence");
    let v = fx
        .call(
            None,
            ToolCall::Changes {
                since: Some(ChangeMark::Seq(mark)),
            },
        )
        .await
        .unwrap();
    assert_eq!(v["changes"], json!([]), "a read after the mark is empty");
}
