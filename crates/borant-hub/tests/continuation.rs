//! Bounded task continuation (phase 11 spec), the part that the core task
//! owns: the `continues` field of a spawn, its refusal without a bound task,
//! and the policy that the agent record keeps. The idle path and the send are
//! in `crates/borant-hub/src/continuation.rs`.

mod common;

use borant_core::model::{
    Agent, ContinuePolicy, ContinueState, Harness, NewTask, Task, TaskBinding,
};
use borant_core::ports::StateStore;
use borant_core::proto::{ApiErrorCode, ToolCall};
use borant_core::{AgentId, ChannelName, keys};
use common::Fx;
use serde_json::Value;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn policy() -> ContinuePolicy {
    ContinuePolicy {
        max: 2,
        max_hours: Some(4),
    }
}

fn spawn_call(continues: Option<ContinuePolicy>, task: Option<TaskBinding>) -> ToolCall {
    ToolCall::Spawn {
        channel: api(),
        prompt: "do the work".into(),
        harness: Some(Harness::Fake),
        host: None,
        name: None,
        model: None,
        role: None,
        key: None,
        task,
        stop_on_close: None,
        continues,
        priority: None,
        bundles: Vec::new(),
    }
}

/// A hub with host `h1`, an owner, a channel `api`, and one task in it.
async fn bound(fx: &Fx) -> (borant_core::proto::Registered, Task) {
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .expect("create the channel");
    let v = fx
        .call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "do the work".into(),
                    ..NewTask::default()
                },
            },
        )
        .await
        .expect("create the task");
    (owner, serde_json::from_value(v).expect("a task"))
}

#[tokio::test]
async fn a_spawn_with_continues_and_a_task_keeps_the_policy() {
    let fx = Fx::new();
    let (owner, task) = bound(&fx).await;
    let v = fx
        .call(
            Some(&owner),
            spawn_call(
                Some(policy()),
                Some(TaskBinding::Existing {
                    channel: api(),
                    id: task.id,
                }),
            ),
        )
        .await
        .expect("spawn a bound worker");
    let id: AgentId = serde_json::from_value(agent_id(&v)).expect("an agent ID");
    let doc = fx
        .state
        .get(&keys::agent(&id))
        .await
        .expect("read the agent")
        .expect("a stored agent");
    let agent: Agent = serde_json::from_slice(&doc.value).expect("an agent");
    assert_eq!(agent.continues, Some(ContinueState::new(policy())));
    assert_eq!(agent.task.map(|t| t.id), Some(task.id));
}

#[tokio::test]
async fn a_spawn_with_continues_and_no_task_is_invalid() {
    let fx = Fx::new();
    let (owner, _task) = bound(&fx).await;
    let err = fx
        .call(Some(&owner), spawn_call(Some(policy()), None))
        .await
        .expect_err("continues need a bound task");
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
    assert!(err.message.contains("binds no task"), "{err:?}");
}

/// The agent of a spawn reply, `{"agent": {"agent": "<id>", ...}}`.
fn agent_id(v: &Value) -> Value {
    v["agent"]["agent"].clone()
}
