//! Task limits, part one: the close rule and the claim guard (phase 6 spec,
//! "Close rule" and "Tasks").

mod common;

use borant_core::model::{CloseReason, Harness, NewTask, Task, TaskPatch, TaskStatus};
use borant_core::ports::StateStore;
use borant_core::proto::{ApiError, ApiErrorCode, Registered, ToolCall};
use borant_core::{ChannelName, TaskId, keys};
use common::Fx;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

struct Rig {
    fx: Fx,
    /// The owner of the channel.
    boss: Registered,
    /// Two member workers.
    w1: Registered,
    w2: Registered,
}

async fn rig() -> Rig {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let boss = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&boss),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let w2 = fx.register("h1", Some("w2"), Some("api")).await;
    Rig { fx, boss, w1, w2 }
}

async fn create(fx: &Fx, who: &Registered, title: &str) -> Task {
    serde_json::from_value(
        fx.call(
            Some(who),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

async fn close(
    fx: &Fx,
    who: Option<&Registered>,
    id: TaskId,
    reason: CloseReason,
) -> Result<Task, ApiError> {
    fx.call(
        who,
        ToolCall::TaskClose {
            channel: api(),
            id,
            reason,
            message: "closed".into(),
        },
    )
    .await
    .map(|v| serde_json::from_value(v).unwrap())
}

async fn claim(fx: &Fx, who: &Registered, id: TaskId) -> Result<Task, ApiError> {
    fx.call(Some(who), ToolCall::TaskClaim { channel: api(), id })
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

async fn status(fx: &Fx, id: TaskId) -> TaskStatus {
    let doc = fx
        .state
        .get(&keys::task(&api(), &id))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice::<Task>(&doc.value).unwrap().status
}

async fn update(fx: &Fx, who: &Registered, id: TaskId, patch: TaskPatch) -> Result<Task, ApiError> {
    fx.call(
        Some(who),
        ToolCall::TaskUpdate {
            channel: api(),
            id,
            patch,
        },
    )
    .await
    .map(|v| serde_json::from_value(v).unwrap())
}

fn needs_human() -> TaskPatch {
    TaskPatch {
        status: Some(TaskStatus::NeedsHuman),
        attention_msg: Some("a question".into()),
        ..Default::default()
    }
}

/// A worker may not close a task as done: the owner of the channel does.
#[tokio::test]
async fn close_rule_only_the_owner_closes_a_task_as_done() {
    let r = rig().await;
    let task = create(&r.fx, &r.boss, "ship it").await;
    let err = close(&r.fx, Some(&r.w1), task.id, CloseReason::Done)
        .await
        .expect_err("a worker cannot close as done");
    assert_eq!(err.code, ApiErrorCode::Denied);
    assert_eq!(err.message, "only the owner closes a task as done");
    assert_eq!(
        status(&r.fx, task.id).await,
        TaskStatus::Open,
        "the refused close writes nothing"
    );

    let closed = close(&r.fx, Some(&r.boss), task.id, CloseReason::Done)
        .await
        .expect("the owner closes as done");
    assert_eq!(closed.status, TaskStatus::Closed);
    assert_eq!(closed.close_reason, Some(CloseReason::Done));
}

/// A person closes a task as done, and other reasons stay open to members.
#[tokio::test]
async fn close_rule_a_person_and_member_reasons() {
    let r = rig().await;
    let mine = create(&r.fx, &r.boss, "by a person").await;
    let by_person = close(&r.fx, None, mine.id, CloseReason::Done)
        .await
        .expect("a person closes as done");
    assert_eq!(by_person.status, TaskStatus::Closed);

    let other = create(&r.fx, &r.boss, "wontfix").await;
    let by_member = close(&r.fx, Some(&r.w1), other.id, CloseReason::Wontfix)
        .await
        .expect("a member closes with another reason");
    assert_eq!(by_member.close_reason, Some(CloseReason::Wontfix));
}

/// A question that no one has claimed cannot be claimed over.
#[tokio::test]
async fn claim_guard_an_unclaimed_needs_human_task_is_closed_to_agents() {
    let r = rig().await;
    let task = create(&r.fx, &r.boss, "ask").await;
    let asked = update(&r.fx, &r.boss, task.id, needs_human())
        .await
        .unwrap();
    assert_eq!(asked.claimed_by, None);

    let err = claim(&r.fx, &r.w1, task.id)
        .await
        .expect_err("an agent cannot claim over a question");
    assert_eq!(err.code, ApiErrorCode::Conflict);
    assert!(err.message.contains("needs_human"), "{}", err.message);
    assert_eq!(status(&r.fx, task.id).await, TaskStatus::NeedsHuman);
}

/// A stuck task belongs to its claimer; another agent cannot take it.
#[tokio::test]
async fn claim_guard_a_stuck_task_belongs_to_its_claimer() {
    let r = rig().await;
    let task = create(&r.fx, &r.boss, "stalled").await;
    claim(&r.fx, &r.w1, task.id).await.unwrap();
    let stuck = TaskPatch {
        status: Some(TaskStatus::Stuck),
        attention_msg: Some("no progress".into()),
        ..Default::default()
    };
    update(&r.fx, &r.w1, task.id, stuck).await.unwrap();

    let err = claim(&r.fx, &r.w2, task.id)
        .await
        .expect_err("another agent cannot take a stuck task");
    assert_eq!(err.code, ApiErrorCode::Conflict);

    let again = claim(&r.fx, &r.w1, task.id)
        .await
        .expect("the claimer takes its own task again");
    assert_eq!(again.status, TaskStatus::InProgress);
}
