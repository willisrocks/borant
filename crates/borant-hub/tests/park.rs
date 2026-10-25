//! Parking idle agents on pool hosts (spec "Parking").
//!
//! The hub parks only agents on pool hosts, and only ones that can wake: the
//! idle signal is older than the pool timeout, the agent holds no task, has
//! no unacked message, is movable, and has channels. A parked agent wakes on
//! a wake message; a plain released agent stays asleep.

mod common;

use std::time::Duration;

use borant_core::model::{Harness, Lease, LeaseState, NewTask, Target, Task};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, HostCommand, Registered, ToolCall};
use borant_core::{AgentId, ChannelName, Clock, Timestamp, keys, subjects, testing};
use common::{Fx, next_cmd};
use futures::stream::BoxStream;

const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// The heartbeat interval of the hub, so a beat every step stays fresh.
const BEAT: u64 = 10;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// An owner on static host `s1` with channel `api`, and a worker on pool host
/// `p1`.
async fn setup(fx: &Fx) -> (Registered, Registered) {
    fx.host("s1", &[Harness::Fake]).await;
    let owner = fx.register("s1", Some("boss"), None).await;
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
    .unwrap();
    fx.pool_host("p1", &[Harness::Fake], Some("workers")).await;
    let w = fx.register("p1", Some("w"), Some("api")).await;
    (owner, w)
}

/// Heartbeats for `secs` with the idle signal of `w`, and none for the owner
/// on `s1`.
async fn beat_worker(fx: &Fx, w: &Registered, idle: Option<Timestamp>, secs: u64) {
    let mut left = secs;
    loop {
        let step = left.min(BEAT);
        fx.advance(step);
        fx.heartbeat_idle("p1", &[(w, idle)]).await;
        fx.heartbeat("s1", &[]).await;
        left -= step;
        if left == 0 {
            return;
        }
    }
}

/// The lease of an agent, from the hub state.
async fn lease(fx: &Fx, agent: &AgentId) -> Lease {
    let e = fx
        .state
        .get(&keys::lease_agent(agent))
        .await
        .unwrap()
        .expect("the agent has a lease");
    serde_json::from_slice(&e.value).unwrap()
}

async fn claim_a_task(fx: &Fx, owner: &Registered, w: &Registered) {
    let t: Task = serde_json::from_value(
        fx.call(
            Some(owner),
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
    fx.call(
        Some(w),
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn parks_an_idle_agent_on_a_pool_host() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut p1 = fx.commands("p1").await;
    let idle = fx.clock.now();
    beat_worker(&fx, &w, Some(idle), IDLE_TIMEOUT.as_secs() + BEAT).await;
    let parked = fx.hub.park_idle(None, IDLE_TIMEOUT).await.unwrap();
    assert_eq!(parked, vec![w.agent.clone()]);
    match next_cmd(&mut p1).await {
        Some(HostCommand::Park { session }) => assert_eq!(session, w.session),
        other => panic!("expected a park, got {other:?}"),
    }
}

#[tokio::test]
async fn does_not_park_a_session_in_a_turn() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut p1 = fx.commands("p1").await;
    beat_worker(&fx, &w, None, IDLE_TIMEOUT.as_secs() + BEAT).await;
    assert!(
        fx.hub
            .park_idle(None, IDLE_TIMEOUT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(next_cmd(&mut p1).await.is_none());
}

#[tokio::test]
async fn does_not_park_an_agent_with_a_claim() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim_a_task(&fx, &owner, &w).await;
    let mut p1 = fx.commands("p1").await;
    let idle = fx.clock.now();
    beat_worker(&fx, &w, Some(idle), IDLE_TIMEOUT.as_secs() + BEAT).await;
    assert!(
        fx.hub
            .park_idle(None, IDLE_TIMEOUT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(next_cmd(&mut p1).await.is_none());
}

#[tokio::test]
async fn does_not_park_an_agent_with_an_unacked_message() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let env = testing::envelope(Target::Agent(w.agent.clone()), "stays here");
    fx.bus
        .publish(&subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    let mut p1 = fx.commands("p1").await;
    let idle = fx.clock.now();
    beat_worker(&fx, &w, Some(idle), IDLE_TIMEOUT.as_secs() + BEAT).await;
    assert!(
        fx.hub
            .park_idle(None, IDLE_TIMEOUT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(next_cmd(&mut p1).await.is_none());
}

#[tokio::test]
async fn does_not_park_an_agent_on_a_static_host() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let mut s1 = fx.commands("s1").await;
    let idle = fx.clock.now();
    // The worker is in a turn, so the owner on the static host is the only
    // agent that is idle past the timeout.
    beat_worker(&fx, &w, None, IDLE_TIMEOUT.as_secs() + BEAT).await;
    fx.heartbeat_idle("s1", &[(&owner, Some(idle))]).await;
    assert!(
        fx.hub
            .park_idle(None, IDLE_TIMEOUT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(next_cmd(&mut s1).await.is_none());
}

#[tokio::test]
async fn does_not_park_an_agent_with_a_stale_report() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut p1 = fx.commands("p1").await;
    // One report, then the host stops reporting the session. The hub cannot
    // tell whether the agent started a turn, so it does not park it.
    fx.heartbeat_idle("p1", &[(&w, Some(fx.clock.now()))]).await;
    fx.advance(IDLE_TIMEOUT.as_secs() + BEAT);
    assert!(
        fx.hub
            .park_idle(None, IDLE_TIMEOUT)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(next_cmd(&mut p1).await.is_none());
}

/// Parks `w`, then ends its session the way its host would with `reason`.
async fn park_and_end(fx: &Fx, w: &Registered, reason: &str) -> Lease {
    let idle = fx.clock.now();
    beat_worker(fx, w, Some(idle), IDLE_TIMEOUT.as_secs() + BEAT).await;
    assert_eq!(
        fx.hub.park_idle(None, IDLE_TIMEOUT).await.unwrap(),
        vec![w.agent.clone()]
    );
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: reason.into(),
    })
    .await
    .unwrap();
    lease(fx, &w.agent).await
}

async fn next_launch(
    a: &mut BoxStream<'static, HostCommand>,
    b: &mut BoxStream<'static, HostCommand>,
) -> Option<HostCommand> {
    tokio::select! {
        x = next_cmd(a) => x,
        x = next_cmd(b) => x,
    }
}

/// Ticks the hub with both hosts alive until a launch order arrives or `secs`
/// pass.
async fn wake(fx: &Fx, owner: &Registered, secs: u64) -> Option<HostCommand> {
    let mut s1 = fx.commands("s1").await;
    let mut p1 = fx.commands("p1").await;
    let mut left = secs;
    while left > 0 {
        let step = left.min(BEAT);
        fx.advance(step);
        fx.heartbeat_idle("p1", &[]).await;
        fx.heartbeat_idle("s1", &[(owner, None)]).await;
        fx.hub.tick().await.unwrap();
        if let Some(cmd) = next_launch(&mut s1, &mut p1).await {
            return Some(cmd);
        }
        left -= step;
    }
    None
}

#[tokio::test]
async fn a_parked_agent_wakes_on_a_message() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let lease = park_and_end(&fx, &w, "park").await;
    assert!(
        matches!(lease.state, LeaseState::Released { parked: true, .. }),
        "{lease:?}"
    );
    let env = testing::envelope(Target::Agent(w.agent.clone()), "back to work");
    fx.bus
        .publish(&subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    match wake(&fx, &owner, 200).await {
        Some(HostCommand::Launch(o)) => assert_eq!(o.agent, w.agent),
        other => panic!("expected a launch of the parked agent, got {other:?}"),
    }
}

#[tokio::test]
async fn a_plain_release_does_not_wake_like_a_park() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let lease = park_and_end(&fx, &w, "session ended").await;
    assert!(
        matches!(lease.state, LeaseState::Released { parked: false, .. }),
        "{lease:?}"
    );
    let env = testing::envelope(Target::Agent(w.agent.clone()), "anyone there");
    fx.bus
        .publish(&subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    assert!(
        wake(&fx, &owner, 200).await.is_none(),
        "a plain release stays asleep"
    );
}
