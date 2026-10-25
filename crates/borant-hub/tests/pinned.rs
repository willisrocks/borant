//! Pinned agents: the hub never replaces one while its session may still run,
//! and a pinned agent keeps its claims.

mod common;

use std::time::Duration;

use borant_core::model::{Agent, Harness, Lease, LeaseState, NewTask, Task, TaskStatus};
use borant_core::ports::StateStore;
use borant_core::proto::{CtlRequest, MoveSelector, MoveTarget, RegisterSession, ToolCall};
use borant_core::{AgentId, ChannelName, SessionId, keys};
use common::Fx;
use serde_json::{Value, json};

/// The default lease of a hub config.
const LEASE: Duration = Duration::from_secs(30);
/// The default wait before a takeover.
const GRACE: Duration = Duration::from_secs(120);
/// The default time after which claims return to open.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(1800);

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn host() -> borant_core::HostName {
    "h1".parse().unwrap()
}

/// Registers `w1` on `h1` with a channel, then pins it.
async fn pinned_agent(fx: &Fx) -> borant_core::proto::Registered {
    fx.host("h1", &[Harness::Fake]).await;
    let who = fx.register("h1", Some("w1"), None).await;
    fx.call(
        Some(&who),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    let v = fx
        .call(
            None,
            ToolCall::Pin {
                agent: who.agent.clone(),
                on: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(v["pinned"], json!(true));
    who
}

/// Creates a task in `#api` and claims it for `who`.
async fn claim_task(fx: &Fx, who: &borant_core::proto::Registered) -> Task {
    let task: Task = serde_json::from_value(
        fx.call(
            Some(who),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "the work".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value(
        fx.call(
            Some(who),
            ToolCall::TaskClaim {
                channel: api(),
                id: task.id,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

/// Expires the lease of the agent and moves past the takeover grace.
async fn expire(fx: &Fx) {
    fx.clock.advance(LEASE * 2);
    fx.hub.tick().await.unwrap();
    fx.clock.advance(GRACE * 2);
    // The clock moved past the online window of the host.
    fx.host("h1", &[Harness::Fake]).await;
}

async fn lease(fx: &Fx, id: &AgentId) -> Lease {
    let e = fx
        .state
        .get(&keys::lease_agent(id))
        .await
        .unwrap()
        .expect("a lease");
    serde_json::from_slice(&e.value).unwrap()
}

async fn agent(fx: &Fx, id: &AgentId) -> Agent {
    let e = fx
        .state
        .get(&keys::agent(id))
        .await
        .unwrap()
        .expect("an agent");
    serde_json::from_slice(&e.value).unwrap()
}

#[tokio::test]
async fn pinned_takeover_is_skipped() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    // A claim makes an expired movable agent worth a takeover.
    claim_task(&fx, &who).await;
    expire(&fx).await;
    fx.hub.tick().await.unwrap();

    assert!(
        matches!(
            lease(&fx, &who.agent).await.state,
            LeaseState::Expired { .. }
        ),
        "a pinned agent keeps its expired lease"
    );
    assert!(
        agent(&fx, &who.agent).await.session.is_none(),
        "no second session starts"
    );

    // Unpinning it lets the same tick take it over.
    fx.call(
        None,
        ToolCall::Pin {
            agent: who.agent.clone(),
            on: false,
        },
    )
    .await
    .unwrap();
    // The clock moved past the host's online window.
    fx.host("h1", &[Harness::Fake]).await;
    fx.hub.tick().await.unwrap();
    assert!(
        matches!(
            lease(&fx, &who.agent).await.state,
            LeaseState::Reserved { .. }
        ),
        "an unpinned agent is placed again"
    );
}

#[tokio::test]
async fn pinned_pending_move_is_not_placed() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let parent = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&parent),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    let child: AgentId = "child".parse().unwrap();
    // No host offers Claude Code yet, so the spawn stays a pending move.
    fx.call(
        Some(&parent),
        ToolCall::Spawn {
            channel: api(),
            prompt: "go".into(),
            harness: Some(Harness::ClaudeCode),
            host: None,
            name: Some(child.clone()),
            model: None,
            role: None,
        },
    )
    .await
    .unwrap();
    fx.call(
        None,
        ToolCall::Pin {
            agent: child.clone(),
            on: true,
        },
    )
    .await
    .unwrap();

    // Past the grace, the tick must not place the pending move.
    fx.clock.advance(GRACE * 2);
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::ClaudeCode]).await;
    fx.hub.tick().await.unwrap();
    assert!(
        matches!(lease(&fx, &child).await.state, LeaseState::Expired { .. }),
        "a pending move does not start a session of a pinned agent"
    );

    // Unpinning it lets the same tick place it on h2.
    fx.call(
        None,
        ToolCall::Pin {
            agent: child.clone(),
            on: false,
        },
    )
    .await
    .unwrap();
    fx.hub.tick().await.unwrap();
    assert!(
        matches!(lease(&fx, &child).await.state, LeaseState::Reserved { .. }),
        "an unpinned pending move is placed"
    );
}

#[tokio::test]
async fn pinned_move_is_skipped() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    expire(&fx).await;

    let v: Value = fx
        .call(
            None,
            ToolCall::Move {
                selector: MoveSelector::Agent(who.agent.clone()),
                to: MoveTarget {
                    host: Some(host()),
                    harness: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(v, json!({ "moving": [] }));
    assert!(matches!(
        lease(&fx, &who.agent).await.state,
        LeaseState::Expired { .. }
    ));
}

#[tokio::test]
async fn pinned_new_session_is_refused() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    expire(&fx).await;

    let err = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: host(),
            harness: Harness::Fake,
            pid: Some(2),
            cwd: "/work".into(),
            repo: Some(common::repo()),
            agent: Some(who.agent.clone()),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert!(err.message.contains("pinned"), "{err}");
}

#[tokio::test]
async fn pinned_same_session_reclaims() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    expire(&fx).await;

    let reg = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: who.session,
            host: host(),
            harness: Harness::Fake,
            pid: Some(1),
            cwd: "/work".into(),
            repo: Some(common::repo()),
            agent: Some(who.agent.clone()),
            ..Default::default()
        }))
        .await
        .unwrap();
    let reg: borant_core::proto::Registered = serde_json::from_value(reg).unwrap();
    assert_eq!(reg.session, who.session);
    assert!(reg.pinned);
    assert!(matches!(
        lease(&fx, &who.agent).await.state,
        LeaseState::Held { .. }
    ));
}

#[tokio::test]
async fn pinned_claims_survive_the_claim_timeout() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    let task = claim_task(&fx, &who).await;
    assert_eq!(task.claimed_by, Some(who.agent.clone()));

    expire(&fx).await;
    fx.clock.advance(CLAIM_TIMEOUT * 2);
    fx.hub.tick().await.unwrap();

    let e = fx
        .state
        .get(&keys::task(&api(), &task.id))
        .await
        .unwrap()
        .expect("the task");
    let task: Task = serde_json::from_slice(&e.value).unwrap();
    assert_eq!(
        task.claimed_by,
        Some(who.agent.clone()),
        "a pinned agent keeps its claims"
    );
    assert_eq!(task.status, TaskStatus::InProgress);
}
