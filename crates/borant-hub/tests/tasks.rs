//! Idempotent task calls: a create with a key, and a repeated close.

mod common;

use borant_core::ChannelName;
use borant_core::model::{ChannelKind, NewTask, Task, TaskPatch};
use borant_core::ports::StateStore;
use borant_core::proto::{CtlRequest, ToolCall};
use common::Fx;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

async fn fixture() -> (Fx, borant_core::proto::Registered) {
    let fx = Fx::new();
    fx.host("h1", &[borant_core::model::Harness::Fake]).await;
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
    (fx, owner)
}

async fn create(fx: &Fx, who: &borant_core::proto::Registered, title: &str) -> Task {
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
    who: &borant_core::proto::Registered,
    id: borant_core::TaskId,
    reason: borant_core::model::CloseReason,
) -> Result<Task, borant_core::proto::ApiError> {
    fx.call(
        Some(who),
        ToolCall::TaskClose {
            channel: api(),
            id,
            reason,
            message: "done".into(),
        },
    )
    .await
    .map(|v| serde_json::from_value(v).unwrap())
}

/// A retried close with the same reason returns the task; another reason is a
/// conflict.
#[tokio::test]
async fn tasks_a_repeated_close_is_idempotent() {
    let (fx, owner) = fixture().await;
    let task = create(&fx, &owner, "ship it").await;

    let done = close(&fx, &owner, task.id, borant_core::model::CloseReason::Done)
        .await
        .expect("the first close");
    assert_eq!(done.status, borant_core::model::TaskStatus::Closed);
    // The same close again returns the task as it is.
    let again = close(&fx, &owner, task.id, borant_core::model::CloseReason::Done)
        .await
        .expect("a repeated close");
    assert_eq!(again, done);
    // Another reason does not rewrite the close.
    let err = close(
        &fx,
        &owner,
        task.id,
        borant_core::model::CloseReason::Wontfix,
    )
    .await
    .expect_err("another reason conflicts");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Conflict);
    let stored: Task = serde_json::from_slice(
        &fx.state
            .get(&borant_core::keys::task(&api(), &task.id))
            .await
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(
        stored.close_reason,
        Some(borant_core::model::CloseReason::Done)
    );
}

/// A create with a key stores it; a repeat with the same key and title returns
/// the first task, and another title under the key is a conflict.
#[tokio::test]
async fn tasks_a_create_key_is_idempotent() {
    let (fx, owner) = fixture().await;
    let call = |title: &str| {
        let actor = borant_core::proto::Actor {
            agent: owner.agent.clone(),
            session: owner.session,
            token: owner.token,
        };
        CtlRequest::Call {
            actor: Some(actor),
            key: Some("create-1".into()),
            call: ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    ..Default::default()
                },
            },
        }
    };
    let first: Task = serde_json::from_value(fx.req(call("ship it")).await.unwrap()).unwrap();
    let again: Task = serde_json::from_value(fx.req(call("ship it")).await.unwrap()).unwrap();
    assert_eq!(again.id, first.id, "the same key returns the first task");
    let stored: Task = serde_json::from_slice(
        &fx.state
            .get(&borant_core::keys::task(&api(), &first.id))
            .await
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(stored.create_key.as_deref(), Some("create-1"));

    let err = fx
        .req(call("another title"))
        .await
        .expect_err("another title under the key conflicts");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Conflict);
    // Only one task exists.
    let all = fx
        .call(
            None,
            ToolCall::TaskList {
                channel: api(),
                ready_only: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(all.as_array().map(Vec::len), Some(1));
}

/// The issue target is stored, changed, and cleared by an empty string; an
/// empty issue link clears the link.
#[tokio::test]
async fn tasks_issue_target_is_stored_and_cleared() {
    let (fx, owner) = fixture().await;
    let created: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "ship it".into(),
                    issue: Some("ENG-1".into()),
                    issue_target: Some("ENG".into()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(created.issue_target.as_deref(), Some("ENG"));

    let fetched: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskGet {
                channel: api(),
                id: created.id,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(fetched.issue_target.as_deref(), Some("ENG"));

    let changed: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskUpdate {
                channel: api(),
                id: created.id,
                patch: TaskPatch {
                    issue_target: Some("OPS".into()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(changed.issue_target.as_deref(), Some("OPS"));

    let cleared: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskUpdate {
                channel: api(),
                id: created.id,
                patch: TaskPatch {
                    issue_target: Some(String::new()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(cleared.issue_target, None);

    let unlinked: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskUpdate {
                channel: api(),
                id: created.id,
                patch: TaskPatch {
                    issue: Some(String::new()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(unlinked.issue, None);
}
