//! The hub tick on a large store: one record list per tick, a lease pass that
//! costs the leases that need it instead of every lease the store holds, an
//! alert pass that reads no inbox counts, a registration that reads no lease,
//! and a tick that keeps no memory it did not keep before.

mod common;

use std::time::Duration;

use borant_core::events::{EventFilter, SwarmEventBody};
use borant_core::model::{
    Agent, ChannelKind, Harness, HostStats, Lease, LeaseState, SessionStats, Target,
};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, SessionLeaseRef};
use borant_core::{AgentId, Clock, FencingToken, SessionId, keys};
use common::{Fx, name};
use futures::StreamExt;

fn api() -> borant_core::ChannelName {
    "api".parse().unwrap()
}

fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// An agent and its lease, written straight into the store as an old version
/// or another hub left them: `count` agents with an hour-old released lease,
/// retired or not.
async fn seed_released(fx: &Fx, count: usize, parked: bool, retired: bool) {
    let now = fx.clock.now();
    for i in 0..count {
        let agent: AgentId = format!("old-{i}").parse().unwrap();
        let record = Agent {
            id: agent.clone(),
            retired,
            movable: true,
            channels: vec![api()],
            host: Some(name("h1")),
            ..Default::default()
        };
        fx.state
            .create(
                &keys::agent(&agent),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .expect("seed the agent");
        let lease = Lease {
            agent,
            token: FencingToken(1),
            state: LeaseState::Released {
                since: now.minus(seconds(3_600)),
                parked,
            },
        };
        fx.state
            .create(
                &keys::lease_agent(&lease.agent),
                serde_json::to_vec(&lease).unwrap().into(),
            )
            .await
            .expect("seed the lease");
    }
}

/// `count` live agents that hold their lease, as a working swarm does.
async fn seed_held(fx: &Fx, count: usize) {
    let now = fx.clock.now();
    for i in 0..count {
        let agent: AgentId = format!("live-{i}").parse().unwrap();
        let record = Agent {
            id: agent.clone(),
            movable: true,
            channels: vec![api()],
            host: Some(name("h1")),
            session: Some(SessionId::new()),
            ..Default::default()
        };
        fx.state
            .create(
                &keys::agent(&agent),
                serde_json::to_vec(&record).unwrap().into(),
            )
            .await
            .expect("seed the agent");
        let lease = Lease {
            agent,
            token: FencingToken(1),
            state: LeaseState::Held {
                session: SessionId::new(),
                host: name("h1"),
                expires: now.plus(seconds(30)),
            },
        };
        fx.state
            .create(
                &keys::lease_agent(&lease.agent),
                serde_json::to_vec(&lease).unwrap().into(),
            )
            .await
            .expect("seed the lease");
    }
}

/// Runs `ticks` ticks of one second each and returns the store calls of the
/// hub after the first one: the first pass reads what the store holds, and
/// every later tick must not.
async fn cost_per_tick(fx: &Fx, ticks: usize) -> u64 {
    fx.hub.tick().await.expect("the first tick");
    let mark = fx.count_from();
    for _ in 1..ticks {
        fx.advance(1);
        fx.hub.tick().await.expect("a tick");
    }
    fx.count_since(mark)
}

/// The released leases of retired agents cost nothing after the first pass:
/// a tick of a hub with a thousand of them costs what a tick of the live
/// leases costs.
#[tokio::test]
async fn released_leases_cost_no_store_call_per_tick() {
    let small = Fx::new();
    small.host("h1", &[Harness::Fake]).await;
    seed_held(&small, 20).await;
    let without = cost_per_tick(&small, 10).await;

    let big = Fx::new();
    big.host("h1", &[Harness::Fake]).await;
    seed_held(&big, 20).await;
    seed_released(&big, 1_000, false, true).await;
    let with = cost_per_tick(&big, 10).await;

    // Nine ticks of a thousand released leases beyond the twenty live ones.
    // The bound is flat in the released leases: before this pass, each of the
    // thousand cost a read of its lease and of its agent, every tick.
    println!(
        "nine ticks: {with} store calls with a thousand released leases, {without} without them"
    );
    assert!(
        with < without + 100,
        "a thousand released leases cost {with} store calls over nine ticks, \
         against {without} without them"
    );
}

/// The gate lets go when a message waits for the agent: a released agent is
/// woken by the next tick, not by the poll.
#[tokio::test]
async fn a_message_wakes_a_settled_release() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let boss = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&boss),
        borant_core::proto::ToolCall::ChannelCreate {
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
    .expect("a channel");
    // A parked agent that the hub already released and settled.
    seed_released(&fx, 1, true, false).await;
    let agent: AgentId = "old-0".parse().unwrap();
    // The pass settles the release: nothing waits for it.
    fx.hub.tick().await.expect("the settling tick");
    fx.advance(1);

    // The host daemon puts the message in the inbox, so the hub learns of it
    // from the count and not from a call.
    let envelope = borant_core::testing::envelope(Target::Agent(agent.clone()), "come back");
    fx.bus
        .ensure_inbox(&agent, &borant_core::subjects::inbox(&agent, &[api()]))
        .await
        .expect("the inbox");
    fx.bus
        .publish(&borant_core::subjects::agent(&agent), &envelope)
        .await
        .expect("the message");
    let mut commands = fx.commands("h1").await;
    // The wake check of a released agent.
    fx.advance(10);
    fx.hub.tick().await.expect("the waking tick");
    assert!(
        common::next_cmd(&mut commands)
            .await
            .is_some_and(|c| matches!(c, borant_core::proto::HostCommand::Launch { .. })),
        "the message wakes the released agent at the wake check"
    );
}

/// The alert pass reads the hosts and the agents of the tick and no inbox
/// count, and a live agent still fires its alert.
#[tokio::test]
async fn alerts_fire_without_reading_every_inbox() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let w = fx.register("h1", Some("w"), None).await;
    let now = fx.clock.now();
    let last = now.minus(seconds(45 * 60));
    // A tool in flight: the hung rule stays quiet and the stall rule speaks.
    let activity = borant_core::model::Activity {
        last_tool_at: Some(now),
        last_tool: Some("Bash".into()),
        tool_running_since: Some(now),
        ..Default::default()
    };
    fx.heartbeat("h1", &[&w]).await;
    fx.req(CtlRequest::HostHeartbeat {
        host: name("h1"),
        sessions: vec![SessionLeaseRef {
            session: w.session,
            agent: w.agent.clone(),
            token: w.token,
            idle_since: None,
            last_event_at: Some(last),
            pinned: false,
            activity: Some(activity.clone()),
        }],
        launching: Vec::new(),
        stats: Some(HostStats {
            version: "test".into(),
            sessions: vec![SessionStats {
                agent: w.agent.clone(),
                busy: true,
                paused: false,
                turn: 1,
                last_event_at: Some(last),
                wakes: 0,
                wake_limited: 0,
                ack_failed: 0,
                append_failed: 0,
                redelivered: 0,
                bad_envelope: 0,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
            }],
            ..Default::default()
        }),
    })
    .await
    .expect("the heartbeat");

    let stats = fx.stats_from();
    fx.advance(1);
    fx.hub.tick().await.expect("the alert pass");
    assert_eq!(
        fx.stats_from(),
        stats,
        "the alert pass reads no inbox count"
    );
    assert!(
        fx.recorder
            .notices()
            .iter()
            .any(|n| n.title.contains("stalled")),
        "the stalled agent of a live session still fires: {:?}",
        fx.recorder.notices()
    );
}

/// A host registration reads the agents of its own host, from the list of the
/// last tick, and not every lease in the store.
#[tokio::test]
async fn registration_sees_its_host_agents() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let _a = fx.register("h1", Some("a"), None).await;
    let _b = fx.register("h1", Some("b"), None).await;
    let _c = fx.register("h1", Some("c"), None).await;
    let _d = fx.register("h2", Some("d"), None).await;
    fx.advance(1);
    fx.hub.tick().await.expect("the tick");

    let mark = fx.count_from();
    fx.host("h1", &[Harness::Fake]).await;
    let cost = fx.count_since(mark);
    assert!(
        cost <= 4,
        "a registration took {cost} store calls: it reads no lease"
    );

    let events: Vec<_> = fx
        .events
        .read(
            &EventFilter {
                host: Some(name("h1")),
                kinds: vec!["host.registered".into()],
                ..Default::default()
            },
            false,
        )
        .await
        .expect("the events")
        .collect()
        .await;
    let agents: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.body {
            SwarmEventBody::HostRegistered { agents, .. } => Some(*agents),
            _ => None,
        })
        .collect();
    assert_eq!(
        agents.last().copied(),
        Some(3),
        "h1 holds three agents, not four: {agents:?}"
    );
}

/// The resident size of the process does not grow with the ticks: a hub with
/// a thousand released leases runs two hundred ticks and keeps what it holds.
#[tokio::test]
async fn many_ticks_keep_the_memory() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    seed_held(&fx, 20).await;
    seed_released(&fx, 1_000, false, true).await;
    // The first pass gates the thousand releases.
    fx.hub.tick().await.expect("the first pass");
    let before = resident_kb();
    let mark = fx.count_from();
    for _ in 0..200 {
        fx.advance(1);
        fx.hub.tick().await.expect("a tick");
    }
    let grew = resident_kb().saturating_sub(before);
    let cost = fx.count_since(mark);
    println!("200 ticks: {cost} store calls, {grew} KiB of resident growth");
    assert!(
        grew < 32 * 1024,
        "two hundred ticks grew the process by {grew} KiB"
    );
    assert!(
        cost < 200 * 40,
        "two hundred ticks of a settled store took {cost} store calls"
    );
}

/// The resident set of this process, in KiB, from `/proc`. Zero where the
/// platform has no such file, so the test holds on its other bounds.
fn resident_kb() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    pages * 4
}
