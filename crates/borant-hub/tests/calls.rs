mod common;

use std::time::Duration;

use borant_core::model::{
    ChannelKind, CloseReason, Envelope, Harness, NewTask, Target, Task, TaskStatus,
};
use borant_core::ports::MessageBus;
use borant_core::proto::{ApiErrorCode, ToolCall};
use borant_core::rules::schema;
use borant_core::{ChannelName, FencingToken, TaskId, subjects, testing};
use common::Fx;
use futures::StreamExt;
use serde_json::json;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

async fn create_task(
    fx: &Fx,
    who: &borant_core::proto::Registered,
    title: &str,
    blocked_by: Vec<TaskId>,
) -> Task {
    let v = fx
        .call(
            Some(who),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    blocked_by,
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn ready(fx: &Fx) -> Vec<TaskId> {
    let v = fx
        .call(
            None,
            ToolCall::TaskList {
                channel: api(),
                ready_only: true,
            },
        )
        .await
        .unwrap();
    serde_json::from_value::<Vec<Task>>(v)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect()
}

async fn with_channel(fx: &Fx) -> borant_core::proto::Registered {
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

#[tokio::test]
async fn register_creates_agent_and_inbox() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let r = fx.register("h1", None, None).await;
    assert!(r.agent.as_str().starts_with("fake-"), "{}", r.agent);
    assert_eq!(r.token, FencingToken(1));
    assert!(r.movable);
    assert_eq!(fx.bus.inbox_pending(&r.agent).await.unwrap(), 0);
}

#[tokio::test]
async fn second_session_for_held_agent_gets_lease_held() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.register("h1", Some("a"), None).await;
    let err = fx
        .try_register("h2", Some("a"), None, true)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
    assert!(err.message.contains("h1"), "{}", err.message);
}

#[tokio::test]
async fn stale_token_is_rejected() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let mut r = fx.register("h1", Some("a"), None).await;
    r.token = FencingToken(r.token.0 + 1);
    let err = fx.call(Some(&r), ToolCall::Whoami).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::StaleToken);
}

#[tokio::test]
async fn channel_create_sets_owner_and_default_schema() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let schema = fx
        .call(None, ToolCall::SchemaGet { channel: api() })
        .await
        .unwrap();
    assert_eq!(schema, schema::default_schema());
    let members = fx
        .call(None, ToolCall::Members { name: api() })
        .await
        .unwrap();
    assert_eq!(members[0]["agent"], "boss");
    assert_eq!(members[0]["online"], true);
    let who = fx.call(Some(&owner), ToolCall::Whoami).await.unwrap();
    assert_eq!(who["owner_of"], json!(["api"]));
    let err = fx
        .call(
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
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
}

#[tokio::test]
async fn claim_conflict_between_agents() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w = fx.register("h1", Some("w"), Some("api")).await;
    let t = create_task(&fx, &owner, "build", vec![]).await;
    let claimed = fx
        .call(
            Some(&w),
            ToolCall::TaskClaim {
                channel: api(),
                id: t.id,
            },
        )
        .await
        .unwrap();
    assert_eq!(claimed["status"], "in_progress");
    let err = fx
        .call(
            Some(&owner),
            ToolCall::TaskClaim {
                channel: api(),
                id: t.id,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
    assert!(err.message.contains('w'));
}

#[tokio::test]
async fn human_cannot_claim() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let t = create_task(&fx, &owner, "build", vec![]).await;
    let err = fx
        .call(
            None,
            ToolCall::TaskClaim {
                channel: api(),
                id: t.id,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid);
}

#[tokio::test]
async fn close_publishes_event_and_unblocks() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let mut events = fx.bus.subscribe("borant.chan.*.tasks").await.unwrap();
    let a = create_task(&fx, &owner, "a", vec![]).await;
    let b = create_task(&fx, &owner, "b", vec![a.id]).await;
    assert_eq!(ready(&fx).await, vec![a.id]);
    fx.call(
        Some(&owner),
        ToolCall::TaskClose {
            channel: api(),
            id: a.id,
            reason: CloseReason::Done,
            message: "done and tested".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(ready(&fx).await, vec![b.id]);
    let mut seen: Vec<Envelope> = Vec::new();
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(100), events.next()).await {
        seen.push(e);
    }
    let closed = seen
        .iter()
        .filter_map(|e| e.event.as_ref())
        .find(|ev| ev.status == TaskStatus::Closed)
        .expect("a closed event");
    assert_eq!(closed.task, a.id);
    assert_eq!(closed.owner.as_ref().map(|o| o.as_str()), Some("boss"));
}

#[tokio::test]
async fn cycle_edge_is_rejected() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let a = create_task(&fx, &owner, "a", vec![]).await;
    let b = create_task(&fx, &owner, "b", vec![a.id]).await;
    let err = fx
        .call(
            Some(&owner),
            ToolCall::TaskUpdate {
                channel: api(),
                id: a.id,
                patch: borant_core::model::TaskPatch {
                    blocked_by: Some(vec![b.id]),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid);
    assert!(err.message.contains("cycle"), "{}", err.message);
}

#[tokio::test]
async fn invalid_property_is_rejected_with_reason() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let err = fx
        .call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "x".into(),
                    properties: json!({"files": "not-a-list"}).as_object().unwrap().clone(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid);
    assert!(err.message.contains("/files"), "{}", err.message);
}

#[tokio::test]
async fn join_updates_inbox_subjects() {
    let fx = Fx::new();
    with_channel(&fx).await;
    let w = fx.register("h1", Some("w"), None).await;
    fx.call(Some(&w), ToolCall::ChannelJoin { name: api() })
        .await
        .unwrap();
    let env = testing::envelope(Target::Channel(api()), "hello team");
    fx.bus
        .publish(&subjects::broadcast(&api()), &env)
        .await
        .unwrap();
    assert_eq!(fx.bus.inbox_pending(&w.agent).await.unwrap(), 1);
}

#[tokio::test]
async fn heartbeat_reports_lost_sessions() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let r = fx.register("h1", Some("a"), None).await;
    assert!(fx.heartbeat("h1", &[&r]).await.lost.is_empty());
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    // A new session takes the expired agent, so the old one cannot reclaim.
    fx.register("h1", Some("a"), None).await;
    assert_eq!(fx.heartbeat("h1", &[&r]).await.lost, vec![r.session]);
}

#[tokio::test]
async fn parent_cycle_is_rejected() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let a = create_task(&fx, &owner, "a", vec![]).await;
    let b = create_task(&fx, &owner, "b", vec![]).await;
    let set_parent = |id, parent| ToolCall::TaskUpdate {
        channel: api(),
        id,
        patch: borant_core::model::TaskPatch {
            parent: Some(parent),
            ..Default::default()
        },
    };
    fx.call(Some(&owner), set_parent(b.id, a.id)).await.unwrap();
    let err = fx
        .call(Some(&owner), set_parent(a.id, b.id))
        .await
        .unwrap_err();
    assert!(err.message.contains("cycle"), "{}", err.message);
}
