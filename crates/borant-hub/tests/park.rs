//! Parking idle agents on pool hosts (spec "Parking").
//!
//! The hub parks only agents on pool hosts, and only ones that can wake: the
//! idle signal is older than the pool timeout, the agent holds no task, has
//! no unacked message, is movable, and has channels. A parked agent wakes on
//! a wake message; a plain released agent stays asleep.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::config::Span;
use borant_core::model::{
    Activity, Agent, ChannelKind, Harness, Lease, LeaseState, NewTask, Target, Task, TaskRef,
};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, HostCommand, Registered, SessionLeaseRef, ToolCall};
use borant_core::rules::roles::RoleConfig;
use borant_core::{AgentId, ChannelName, Clock, Timestamp, keys, subjects, testing};
use borant_hub::HubConfig;
use common::{Fx, name, next_cmd};
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
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
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

// The idle release of a helper or a worker (spec "Parking"): a role that
// names `idle_release_after` gives the slot back on any host, pool or not,
// once the agent has no work in progress.

/// A `hub.toml` whose `helper` and `worker` release after `secs`, or never
/// when it is `None`.
fn roles(secs: Option<u64>) -> HubConfig {
    let release = || RoleConfig {
        idle_release_after: secs.map(|s| Span(Duration::from_secs(s))),
        ..RoleConfig::default()
    };
    HubConfig {
        roles: BTreeMap::from([
            ("helper".parse().unwrap(), release()),
            ("worker".parse().unwrap(), release()),
        ]),
        ..HubConfig::default()
    }
}

/// An owner on static host `s1` with channel `api`, and an agent of `role` on
/// the static host `s2`, which belongs to no pool.
async fn on_a_static_host(fx: &Fx, role: &str) -> (Registered, Registered) {
    fx.host("s1", &[Harness::Fake]).await;
    fx.host("s2", &[Harness::Fake]).await;
    let owner = fx.register("s1", Some("boss"), None).await;
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
    let h = fx.register_roled("s2", "h1", None, role).await;
    fx.call(Some(&h), ToolCall::ChannelJoin { name: api() })
        .await
        .unwrap();
    (owner, h)
}

/// Heartbeats of `host` for `secs` that report the end of the agent's last
/// turn, or a turn in flight with `None`.
async fn beat(fx: &Fx, host: &str, h: &Registered, idle: Option<Timestamp>, secs: u64) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(BEAT);
        fx.advance(step);
        fx.heartbeat_idle(host, &[(h, idle)]).await;
        left -= step;
    }
}

#[tokio::test]
async fn idle_release_parks_an_idle_helper_on_a_static_host() {
    let fx = Fx::with(roles(Some(600)));
    let (_owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    let idle = fx.clock.now();
    beat(&fx, "s2", &h, Some(idle), 600).await;
    assert!(
        fx.hub.park_idle_roles().await.unwrap().is_empty(),
        "at the timeout the slot stays"
    );
    beat(&fx, "s2", &h, Some(idle), BEAT).await;
    assert_eq!(
        fx.hub.park_idle_roles().await.unwrap(),
        vec![h.agent.clone()]
    );
    match next_cmd(&mut s2).await {
        Some(HostCommand::Park { session }) => assert_eq!(session, h.session),
        other => panic!("expected a park, got {other:?}"),
    }
}

#[tokio::test]
async fn idle_release_parks_an_unbound_worker_on_a_static_host() {
    let fx = Fx::with(roles(Some(600)));
    let (_owner, w) = on_a_static_host(&fx, "worker").await;
    let mut s2 = fx.commands("s2").await;
    let doc = fx.state.get(&keys::agent(&w.agent)).await.unwrap().unwrap();
    let agent: Agent = serde_json::from_slice(&doc.value).unwrap();
    assert!(agent.task.is_none(), "an unbound worker");
    let idle = fx.clock.now();
    beat(&fx, "s2", &w, Some(idle), 600 + BEAT).await;
    assert_eq!(
        fx.hub.park_idle_roles().await.unwrap(),
        vec![w.agent.clone()],
        "a worker that holds no task gives its slot back"
    );
    match next_cmd(&mut s2).await {
        Some(HostCommand::Park { session }) => assert_eq!(session, w.session),
        other => panic!("expected a park, got {other:?}"),
    }
}

#[tokio::test]
async fn idle_release_keeps_a_helper_with_an_open_claim() {
    let fx = Fx::with(roles(Some(600)));
    let (owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    claim_a_task(&fx, &owner, &h).await;
    let idle = fx.clock.now();
    beat(&fx, "s2", &h, Some(idle), 600 + BEAT).await;
    assert!(fx.hub.park_idle_roles().await.unwrap().is_empty());
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_keeps_a_helper_with_an_unacked_message() {
    let fx = Fx::with(roles(Some(600)));
    let (_owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    let env = testing::envelope(Target::Agent(h.agent.clone()), "one more thing");
    fx.bus
        .publish(&subjects::agent(&h.agent), &env)
        .await
        .unwrap();
    let idle = fx.clock.now();
    beat(&fx, "s2", &h, Some(idle), 600 + BEAT).await;
    assert!(fx.hub.park_idle_roles().await.unwrap().is_empty());
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_keeps_a_busy_helper() {
    let fx = Fx::with(roles(Some(600)));
    let (_owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    beat(&fx, "s2", &h, None, 600 + BEAT).await;
    assert!(
        fx.hub.park_idle_roles().await.unwrap().is_empty(),
        "a session in a turn keeps its slot"
    );
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_keeps_a_session_with_a_running_tool() {
    let fx = Fx::with(roles(Some(600)));
    let (_owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    let idle = fx.clock.now();
    fx.advance(600 + BEAT);
    // The turn ended, and the host still reports a tool in flight.
    fx.req(CtlRequest::HostHeartbeat {
        host: name("s2"),
        sessions: vec![SessionLeaseRef {
            session: h.session,
            agent: h.agent.clone(),
            token: h.token,
            idle_since: Some(idle),
            last_event_at: None,
            pinned: false,
            activity: Some(Activity {
                tool_running_since: Some(idle),
                ..Activity::default()
            }),
        }],
        stats: None,
        launching: Vec::new(),
    })
    .await
    .unwrap();
    assert!(fx.hub.park_idle_roles().await.unwrap().is_empty());
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_keeps_a_helper_bound_to_an_open_task() {
    let fx = Fx::with(roles(Some(600)));
    let (owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    let t: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "still mine".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    // Bound the way a spawn binds, without a claim: the task is still open.
    let key = keys::agent(&h.agent);
    let doc = fx.state.get(&key).await.unwrap().unwrap();
    let mut bound: Agent = serde_json::from_slice(&doc.value).unwrap();
    bound.task = Some(TaskRef {
        channel: api(),
        id: t.id,
    });
    fx.state
        .update(
            &key,
            serde_json::to_vec(&bound).unwrap().into(),
            doc.revision,
        )
        .await
        .unwrap();
    let idle = fx.clock.now();
    beat(&fx, "s2", &h, Some(idle), 600 + BEAT).await;
    assert!(fx.hub.park_idle_roles().await.unwrap().is_empty());
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_keeps_a_pinned_or_approving_role() {
    let pinned = RoleConfig {
        pinned: true,
        idle_release_after: Some(Span(Duration::from_secs(600))),
        ..RoleConfig::default()
    };
    let approving = RoleConfig {
        approves: true,
        idle_release_after: Some(Span(Duration::from_secs(600))),
        ..RoleConfig::default()
    };
    for (role, config) in [("helper", &pinned), ("chief", &approving)] {
        let fx = Fx::with(HubConfig {
            roles: BTreeMap::from([(role.parse().unwrap(), (*config).clone())]),
            ..HubConfig::default()
        });
        let (_owner, h) = on_a_static_host(&fx, role).await;
        let mut s2 = fx.commands("s2").await;
        let idle = fx.clock.now();
        beat(&fx, "s2", &h, Some(idle), 600 + BEAT).await;
        assert!(
            fx.hub.park_idle_roles().await.unwrap().is_empty(),
            "a {role} role keeps its slot"
        );
        assert!(next_cmd(&mut s2).await.is_none());
    }
}

#[tokio::test]
async fn idle_release_is_off_without_the_key() {
    let fx = Fx::with(roles(None));
    let (_owner, h) = on_a_static_host(&fx, "helper").await;
    let mut s2 = fx.commands("s2").await;
    let idle = fx.clock.now();
    beat(&fx, "s2", &h, Some(idle), 2 * 60 * 60).await;
    assert!(
        fx.hub.park_idle_roles().await.unwrap().is_empty(),
        "unset releases nobody"
    );
    assert!(next_cmd(&mut s2).await.is_none());
}

#[tokio::test]
async fn idle_release_runs_in_the_hub_tick() {
    let fx = Fx::with(roles(Some(600)));
    let (owner, w) = on_a_static_host(&fx, "worker").await;
    let mut s2 = fx.commands("s2").await;
    let idle = fx.clock.now();
    for _ in 0..=(600 + BEAT) / BEAT {
        fx.advance(BEAT);
        fx.heartbeat_idle("s2", &[(&w, Some(idle))]).await;
        fx.heartbeat_idle("s1", &[(&owner, None)]).await;
        fx.hub.tick().await.unwrap();
    }
    match next_cmd(&mut s2).await {
        Some(HostCommand::Park { session }) => assert_eq!(session, w.session),
        other => panic!("the tick never released the idle worker: {other:?}"),
    }
}
