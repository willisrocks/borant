//! Durable waits (phase 6 spec, "Wait tool"): an agent asks the hub to wake it
//! when a task reaches a state. The tick reads the task itself, so a wait
//! survives a hub restart and a lost event, and it wakes the agent once.

mod common;

use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{ChannelKind, CloseReason, Envelope, Harness, NewTask, Task, TaskStatus};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{Registered, ToolCall};
use borant_core::{ChannelName, TaskId, keys};
use borant_hub::{Hub, HubConfig};
use common::Fx;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// A channel owned by `boss` on h1, a worker in it, and an open task.
async fn setup(fx: &Fx) -> (Registered, Registered, TaskId) {
    fx.host("h1", &[Harness::Fake]).await;
    let boss = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&boss),
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
    let w = fx.register("h1", Some("w"), Some("api")).await;
    let task: Task = serde_json::from_value(
        fx.call(
            Some(&boss),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "t".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    (boss, w, task.id)
}

/// Closes the task as its channel owner.
async fn close(fx: &Fx, boss: &Registered, task: TaskId) {
    fx.call(
        Some(boss),
        ToolCall::TaskClose {
            channel: api(),
            id: task,
            reason: CloseReason::Done,
            message: "done".into(),
        },
    )
    .await
    .unwrap();
}

/// The next direct message of the worker's inbox, acked so it does not come
/// back. The channel task events sit in the same inbox and are skipped.
async fn next_message(fx: &Fx, w: &Registered) -> Option<Envelope> {
    let got = fx
        .bus
        .pull(&w.agent, 10, Duration::from_millis(200))
        .await
        .unwrap();
    let mut first = None;
    for d in got {
        if first.is_none() && matches!(d.envelope.kind, borant_core::model::EnvelopeKind::Direct) {
            first = Some(d.envelope.clone());
        }
        let _ = d.acker.ack().await;
    }
    first
}

async fn wait(
    fx: &Fx,
    w: &Registered,
    task: TaskId,
    until: &[TaskStatus],
    timeout_secs: u64,
) -> serde_json::Value {
    fx.call(
        Some(w),
        ToolCall::Wait {
            channel: api(),
            task,
            until: until.to_vec(),
            timeout_secs,
        },
    )
    .await
    .unwrap()
}

/// The stored waits, read back from the store.
async fn stored(fx: &Fx) -> Vec<String> {
    fx.state
        .list(keys::WAIT_PREFIX)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect()
}

#[tokio::test]
async fn waits_answer_at_once_when_the_task_already_matches() {
    let fx = Fx::new();
    let (boss, w, task) = setup(&fx).await;
    close(&fx, &boss, task).await;

    let v = wait(&fx, &w, task, &[TaskStatus::Closed], 600).await;
    assert_eq!(v["satisfied"], true, "{v}");
    assert!(v["wait"].is_string(), "{v}");
    assert!(
        stored(&fx).await.is_empty(),
        "a satisfied wait is not stored"
    );

    fx.hub.tick().await.unwrap();
    assert!(
        next_message(&fx, &w).await.is_none(),
        "the answer is the reply, not a message"
    );
}

#[tokio::test]
async fn waits_wake_the_agent_once_when_the_task_matches_later() {
    let fx = Fx::new();
    let (boss, w, task) = setup(&fx).await;

    let v = wait(&fx, &w, task, &[TaskStatus::Closed], 600).await;
    assert_eq!(v["satisfied"], false, "{v}");
    let id = v["wait"].as_str().unwrap().to_owned();
    assert_eq!(stored(&fx).await, vec![keys::wait(&w.agent, &id)]);

    fx.hub.tick().await.unwrap();
    assert!(
        next_message(&fx, &w).await.is_none(),
        "the task has not changed"
    );

    close(&fx, &boss, task).await;
    fx.hub.tick().await.unwrap();
    let msg = next_message(&fx, &w).await.expect("a wake");
    assert_eq!(msg.to.to_string(), w.agent.to_string());
    assert!(
        msg.text.contains("is now done"),
        "the message names the state: {}",
        msg.text
    );
    assert!(stored(&fx).await.is_empty(), "the wait goes after the wake");

    // One message only, however many ticks follow.
    fx.hub.tick().await.unwrap();
    fx.hub.tick().await.unwrap();
    assert!(next_message(&fx, &w).await.is_none(), "no second wake");
}

#[tokio::test]
async fn waits_wake_after_a_hub_restart_that_lost_the_event() {
    let fx = Fx::new();
    let (boss, w, task) = setup(&fx).await;
    wait(&fx, &w, task, &[TaskStatus::Closed], 600).await;

    // The task closes, and the hub restarts before its tick sees the change.
    close(&fx, &boss, task).await;
    let restarted = Hub::new(
        Arc::new(fx.state.clone()),
        Arc::new(fx.bus.clone()),
        Arc::new(fx.sessions.clone()),
        fx.clock.clone(),
        HubConfig::default(),
    );

    restarted.tick().await.unwrap();
    let msg = next_message(&fx, &w)
        .await
        .expect("a wake after the restart");
    assert!(msg.text.contains("is now done"), "{}", msg.text);
    assert!(stored(&fx).await.is_empty(), "the wait goes after the wake");
}

#[tokio::test]
async fn waits_time_out_with_one_message() {
    let fx = Fx::new();
    let (_boss, w, task) = setup(&fx).await;

    let v = wait(&fx, &w, task, &[TaskStatus::Closed], 60).await;
    assert_eq!(v["satisfied"], false, "{v}");
    let id = v["wait"].as_str().unwrap().to_owned();

    fx.advance(61);
    fx.hub.tick().await.unwrap();
    let msg = next_message(&fx, &w).await.expect("a timeout wake");
    assert!(
        msg.text.contains("timed out") && msg.text.contains(&id),
        "the message names the wait: {}",
        msg.text
    );
    assert!(
        stored(&fx).await.is_empty(),
        "the wait goes after the timeout"
    );

    fx.hub.tick().await.unwrap();
    assert!(next_message(&fx, &w).await.is_none(), "no second wake");
}

#[tokio::test]
async fn waits_refuse_more_than_a_day() {
    let fx = Fx::new();
    let (_boss, w, task) = setup(&fx).await;
    let err = fx
        .call(
            Some(&w),
            ToolCall::Wait {
                channel: api(),
                task,
                until: vec![TaskStatus::Closed],
                timeout_secs: 86_401,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Invalid, "{err}");
    assert!(stored(&fx).await.is_empty(), "nothing stored");
}
