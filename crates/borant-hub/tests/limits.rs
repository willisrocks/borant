//! Task limits: the close rule, the claim guard, attempts, and deadlines
//! (phase 6 spec, "Close rule" and "Tasks").

use std::time::Duration;

mod common;

use borant_core::model::{ChannelKind, CloseReason, Harness, NewTask, Task, TaskPatch, TaskStatus};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{ApiError, ApiErrorCode, Registered, ToolCall};
use borant_core::{ChannelName, Clock, TaskId, keys};
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
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
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

async fn task_of(fx: &Fx, id: TaskId) -> Task {
    let doc = fx
        .state
        .get(&keys::task(&api(), &id))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&doc.value).unwrap()
}

async fn create_with(fx: &Fx, who: &Registered, new: NewTask) -> Task {
    serde_json::from_value(
        fx.call(
            Some(who),
            ToolCall::TaskCreate {
                channel: api(),
                task: new,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

/// A claim that times out counts one attempt, once.
#[tokio::test]
async fn attempts_a_claim_timeout_counts_one() {
    let r = rig().await;
    let task = create(&r.fx, &r.boss, "long job").await;
    claim(&r.fx, &r.w1, task.id).await.unwrap();
    // The host goes silent: the lease expires and the claim timeout returns
    // the claim.
    for _ in 0..190 {
        r.fx.advance(10);
        r.fx.hub.tick().await.unwrap();
    }
    let task = task_of(&r.fx, task.id).await;
    assert_eq!(task.status, TaskStatus::Open);
    assert_eq!(task.claimed_by, None);
    assert_eq!(task.attempts, 1, "the timeout is one attempt");

    // The reopen that follows the timeout is the same claim. The owner's old
    // session went stale with the clock, so it registers again.
    let boss = r.fx.register("h1", Some("boss"), None).await;
    update(
        &r.fx,
        &boss,
        task.id,
        TaskPatch {
            status: Some(TaskStatus::Open),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(task_of(&r.fx, task.id).await.attempts, 1);
}

/// At the retry limit the task waits for its owner: no claim until the owner
/// reopens it, and that reopen starts the count again.
#[tokio::test]
async fn attempts_the_retry_limit_holds_until_the_owner_reopens() {
    let r = rig().await;
    let task = create_with(
        &r.fx,
        &r.boss,
        NewTask {
            title: "tries".into(),
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await;
    claim(&r.fx, &r.w1, task.id).await.unwrap();
    // The worker gives the task back: the claim ends without a done close.
    let limited = update(
        &r.fx,
        &r.w1,
        task.id,
        TaskPatch {
            status: Some(TaskStatus::Open),
            ..Default::default()
        },
    )
    .await
    .expect("the worker reopens its task");
    assert_eq!(limited.attempts, 1);
    assert_eq!(limited.status, TaskStatus::NeedsHuman);
    assert_eq!(
        limited.attention_msg.as_deref(),
        Some("retry limit reached (1 tries)")
    );

    let err = claim(&r.fx, &r.w2, task.id)
        .await
        .expect_err("a task at its limit is not claimable");
    assert_eq!(err.code, ApiErrorCode::Conflict);
    assert!(err.message.contains("retry limit"), "{}", err.message);

    // Only the owner lifts the limit.
    let err = update(
        &r.fx,
        &r.w2,
        task.id,
        TaskPatch {
            status: Some(TaskStatus::Open),
            ..Default::default()
        },
    )
    .await
    .expect_err("a member cannot lift the retry limit");
    assert_eq!(err.code, ApiErrorCode::Conflict);

    let reopened = update(
        &r.fx,
        &r.boss,
        task.id,
        TaskPatch {
            status: Some(TaskStatus::Open),
            ..Default::default()
        },
    )
    .await
    .expect("the owner reopens the task");
    assert_eq!(reopened.attempts, 0, "the owner's reopen resets the count");
    assert_eq!(reopened.status, TaskStatus::Open);

    let again = claim(&r.fx, &r.w2, task.id).await.expect("claimable again");
    assert_eq!(again.status, TaskStatus::InProgress);
}

/// A passed deadline puts the task in front of its owner once, and reassigns
/// nothing.
#[tokio::test]
async fn deadline_a_passed_deadline_sticks_once() {
    let r = rig().await;
    let deadline = r.fx.clock.now().plus(std::time::Duration::from_secs(60));
    let task = create_with(
        &r.fx,
        &r.boss,
        NewTask {
            title: "due".into(),
            deadline: Some(deadline),
            ..Default::default()
        },
    )
    .await;
    let inbox = borant_core::subjects::agent(&r.boss.agent);
    r.fx.bus
        .ensure_inbox(&r.boss.agent, &[inbox])
        .await
        .unwrap();

    r.fx.advance(30);
    r.fx.hub.tick().await.unwrap();
    assert_eq!(
        task_of(&r.fx, task.id).await.status,
        TaskStatus::Open,
        "before the deadline nothing happens"
    );
    assert_eq!(
        r.fx.bus.inbox_pending(&r.boss.agent).await.unwrap(),
        0,
        "no message yet"
    );

    r.fx.advance(60);
    r.fx.hub.tick().await.unwrap();
    let stuck = task_of(&r.fx, task.id).await;
    assert_eq!(stuck.status, TaskStatus::Stuck);
    assert_eq!(stuck.attention_msg.as_deref(), Some("deadline passed"));
    assert_eq!(stuck.claimed_by, None, "it reassigns nothing");
    assert_eq!(
        r.fx.bus.inbox_pending(&r.boss.agent).await.unwrap(),
        1,
        "the owner hears once"
    );
    let heard =
        r.fx.bus
            .pull(&r.boss.agent, 10, Duration::ZERO)
            .await
            .unwrap();
    let msg = &heard[0].envelope;
    assert_eq!(
        msg.to,
        borant_core::model::Target::Agent(r.boss.agent.clone())
    );
    assert!(msg.text.contains("passed its deadline"), "{}", msg.text);
    for d in heard {
        d.acker.ack().await.unwrap();
    }

    r.fx.hub.tick().await.unwrap();
    assert_eq!(
        r.fx.bus.inbox_pending(&r.boss.agent).await.unwrap(),
        0,
        "the miss is reported once"
    );
}
