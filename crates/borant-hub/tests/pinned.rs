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
            key: None,
            task: None,
            stop_on_close: None,
            priority: None,
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
                member: None,
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

/// Reads the agent record.
async fn agent_doc(fx: &Fx, id: &AgentId) -> Agent {
    let e = fx
        .state
        .get(&keys::agent(id))
        .await
        .unwrap()
        .expect("the agent");
    serde_json::from_slice(&e.value).unwrap()
}

/// Rewrites the agent record with `pinned` set as given.
async fn set_stored_pin(fx: &Fx, id: &AgentId, pinned: bool) {
    let key = keys::agent(id);
    let e = fx.state.get(&key).await.unwrap().expect("the agent");
    let mut a: Agent = serde_json::from_slice(&e.value).unwrap();
    a.pinned = pinned;
    let bytes = serde_json::to_vec(&a).unwrap();
    fx.state
        .update(&key, bytes.into(), e.revision)
        .await
        .unwrap();
}

/// A full `RegisterSession` for `who` over the host of the fixture.
fn session_req(who: &borant_core::proto::Registered, role: Option<&str>) -> RegisterSession {
    RegisterSession {
        session: who.session,
        host: host(),
        harness: Harness::Fake,
        harness_session_id: None,
        pid: Some(1),
        cwd: "/work".into(),
        repo: None,
        agent: Some(who.agent.clone()),
        channel: None,
        shared_dir: false,
        role: role.map(|r| r.parse().unwrap()),
        collection: None,
    }
}

#[tokio::test]
async fn pinned_survives_a_registration_with_an_unpinned_role() {
    let roles = std::collections::BTreeMap::from([(
        "orchestrator".parse().unwrap(),
        borant_core::rules::roles::RoleConfig::default(),
    )]);
    let fx = Fx::with(borant_hub::HubConfig {
        roles,
        ..Default::default()
    });
    let who = pinned_agent(&fx).await;
    // The old session ends, and a new one registers with an unpinned role,
    // as a host that lost its table re-registers an agent.
    fx.req(CtlRequest::EndSession {
        session: who.session,
        agent: who.agent.clone(),
        token: who.token,
        reason: "restart".into(),
    })
    .await
    .unwrap();

    let v = fx
        .req(CtlRequest::RegisterSession({
            let mut r = session_req(&who, Some("orchestrator"));
            r.session = SessionId::new();
            r
        }))
        .await
        .unwrap();
    let reg: borant_core::proto::Registered = serde_json::from_value(v).unwrap();

    assert!(reg.pinned, "the pin must survive the registration");
    assert!(
        agent_doc(&fx, &who.agent).await.pinned,
        "an unpinned role must not clear a person's pin"
    );
}

#[tokio::test]
async fn pinned_heartbeat_restores_a_lost_pin() {
    let fx = Fx::new();
    let who = pinned_agent(&fx).await;
    // The hub record lost the pin, for example with the state of a restart.
    set_stored_pin(&fx, &who.agent, false).await;
    assert!(!agent_doc(&fx, &who.agent).await.pinned);

    // The host remembered the pin and reports it on the heartbeat.
    fx.req(CtlRequest::HostHeartbeat {
        host: host(),
        sessions: vec![borant_core::proto::SessionLeaseRef {
            session: who.session,
            agent: who.agent.clone(),
            token: who.token,
            idle_since: None,
            last_event_at: None,
            pinned: true,
        }],
        stats: None,
    })
    .await
    .unwrap();

    assert!(
        agent_doc(&fx, &who.agent).await.pinned,
        "a pinned agent stays pinned"
    );
}
