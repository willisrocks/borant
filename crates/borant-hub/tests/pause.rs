//! Stop, resume, and caps (phase 6 spec, "Stop, resume, caps"): a stop waits
//! for the turn end and fences after the grace, a paused agent is skipped by
//! every placement path and by the claim timeout, a channel pause blocks a
//! spawn, and a role cap stops an agent with `needs_human` on its claim.

mod common;

use std::collections::BTreeMap;

use borant_core::model::{
    Agent, Harness, HostStats, Lease, LeaseState, MoveReason, NewTask, SessionStats, Task,
    TaskStatus,
};
use borant_core::ports::StateStore;
use borant_core::proto::{
    CtlRequest, HostCommand, MoveSelector, MoveTarget, PauseSelector, Registered, SessionLeaseRef,
    ToolCall,
};
use borant_core::rules::members::MemberRules;
use borant_core::rules::roles::RoleConfig;
use borant_core::{AgentId, ChannelName, Clock, RoleName, Timestamp, keys};
use borant_hub::HubConfig;
use common::{Fx, name, next_cmd};
use serde_json::json;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn agent(name: &str) -> AgentId {
    name.parse().unwrap()
}

fn paused_selector(who: &str) -> PauseSelector {
    PauseSelector::Agent(agent(who))
}

/// Two hosts, an owner with a channel, and a worker spawned into it. The
/// worker's session is registered on h1.
async fn setup(fx: &Fx, roles: &[(&str, RoleConfig)]) -> (Registered, Registered) {
    let _ = roles;
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
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
    .unwrap();
    let role = roles.first().map(|(n, _)| (*n).to_owned());
    fx.call(
        Some(&owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            priority: None,
            bundles: Vec::new(),
            channel: api(),
            prompt: "work".into(),
            harness: Some(Harness::Fake),
            host: Some(name("h1")),
            name: Some(agent("w1")),
            model: None,
            role: role.map(|r| r.parse::<RoleName>().unwrap()),
        },
    )
    .await
    .expect("spawn");
    let w = fx.register("h1", Some("w1"), None).await;
    (owner, w)
}

/// A hub with the given roles, so a cap applies.
fn hub(roles: &[(&str, RoleConfig)]) -> HubConfig {
    HubConfig {
        roles: roles
            .iter()
            .map(|(n, r)| (n.parse::<RoleName>().unwrap(), r.clone()))
            .collect::<BTreeMap<_, _>>(),
        member_rules: MemberRules::default(),
        ..HubConfig::default()
    }
}

fn worker(max_turns_per_day: Option<u32>) -> RoleConfig {
    RoleConfig {
        harness: Some(Harness::Fake),
        max_turns_per_day,
        ..RoleConfig::default()
    }
}

async fn agent_of(fx: &Fx, id: &str) -> Agent {
    let doc = fx
        .state
        .get(&keys::agent(&agent(id)))
        .await
        .unwrap()
        .expect("the agent");
    serde_json::from_slice(&doc.value).unwrap()
}

async fn lease_of(fx: &Fx, id: &str) -> Lease {
    let doc = fx
        .state
        .get(&keys::lease_agent(&agent(id)))
        .await
        .unwrap()
        .expect("the lease");
    serde_json::from_slice(&doc.value).unwrap()
}

/// A heartbeat with the stats of each session.
async fn stats_heartbeat(
    fx: &Fx,
    host: &str,
    refs: &[(&Registered, Option<Timestamp>)],
    stats: Vec<SessionStats>,
) -> borant_core::proto::HeartbeatReply {
    let sessions = refs
        .iter()
        .map(|(r, since)| SessionLeaseRef {
            session: r.session,
            agent: r.agent.clone(),
            token: r.token,
            idle_since: *since,
            last_event_at: None,
            pinned: false,
        })
        .collect();
    let v = fx
        .req(CtlRequest::HostHeartbeat {
            host: name(host),
            sessions,
            stats: Some(HostStats {
                sessions: stats,
                ..HostStats::default()
            }),
        })
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

fn session_stats(who: &str, turn: u64) -> SessionStats {
    SessionStats {
        agent: agent(who),
        busy: true,
        paused: false,
        turn,
        last_event_at: None,
        wakes: 0,
        wake_limited: 0,
        ack_failed: 0,
        append_failed: 0,
        redelivered: 0,
        bad_envelope: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
    }
}

/// One task claimed by `who`, with a claim.
async fn claim(fx: &Fx, owner: &Registered, who: &Registered) -> Task {
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
        Some(who),
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await
    .unwrap();
    t
}

async fn task_of(fx: &Fx, id: borant_core::TaskId) -> Task {
    let doc = fx
        .state
        .get(&keys::task(&api(), &id))
        .await
        .unwrap()
        .expect("the task");
    serde_json::from_slice(&doc.value).unwrap()
}

#[tokio::test]
async fn pause_stops_at_the_turn_end() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx, &[]).await;
    let mut h1 = fx.commands("h1").await;
    // The session is in a turn, so the stop waits for its end.
    fx.heartbeat_idle("h1", &[(&w, None)]).await;
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: paused_selector("w1"),
            grace_secs: Some(600),
        },
    )
    .await
    .expect("the owner stops its member");
    assert!(agent_of(&fx, "w1").await.paused.is_some());
    assert!(
        next_cmd(&mut h1).await.is_none(),
        "a stop in a turn waits for the turn end"
    );
    // The turn ends: the session gets its stop order.
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    match next_cmd(&mut h1).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected stop, got {other:?}"),
    }
    // The end of the session releases the lease, and does not park it.
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "stopped".into(),
    })
    .await
    .unwrap();
    match lease_of(&fx, "w1").await.state {
        LeaseState::Released { parked, .. } => assert!(!parked, "a stopped agent is not parked"),
        other => panic!("expected a release, got {other:?}"),
    }
    // A retry of the harness does not bring the session back while paused.
    let err = fx
        .try_register("h1", Some("w1"), None, true)
        .await
        .expect_err("a paused agent takes no session");
    assert!(err.message.contains("paused"), "{err}");
}

#[tokio::test]
async fn pause_fences_after_the_grace() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx, &[]).await;
    let mut h1 = fx.commands("h1").await;
    fx.heartbeat_idle("h1", &[(&w, None)]).await;
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: paused_selector("w1"),
            grace_secs: Some(60),
        },
    )
    .await
    .unwrap();
    assert!(
        next_cmd(&mut h1).await.is_none(),
        "the turn is still running"
    );
    fx.advance(61);
    fx.hub.tick().await.unwrap();
    match next_cmd(&mut h1).await {
        Some(HostCommand::Fence { session, reason }) => {
            assert_eq!(session, w.session);
            assert!(reason.contains("grace"), "{reason}");
        }
        other => panic!("expected a fence, got {other:?}"),
    }
}

#[tokio::test]
async fn pause_keeps_the_agent_out_of_moves_and_the_claim_timeout() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx, &[]).await;
    let task = claim(&fx, &owner, &w).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: paused_selector("w1"),
            grace_secs: Some(60),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        next_cmd(&mut h1).await,
        Some(HostCommand::Stop { .. })
    ));
    // A move of a paused agent does nothing.
    fx.call(
        Some(&owner),
        ToolCall::Move {
            selector: MoveSelector::Agent(w.agent.clone()),
            to: MoveTarget {
                host: Some(name("h2")),
                harness: None,
            },
            member: None,
        },
    )
    .await
    .ok();
    assert!(
        next_cmd(&mut h1).await.is_none(),
        "no move of a paused agent"
    );
    assert!(next_cmd(&mut h2).await.is_none());
    // Claim expiry skips it: the claim stays after the timeout.
    fx.advance(10_000);
    fx.hub.tick().await.unwrap();
    assert_eq!(
        task_of(&fx, task.id).await.status,
        TaskStatus::InProgress,
        "a paused agent keeps its claim"
    );
}

#[tokio::test]
async fn pause_resume_places_the_agent_again() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx, &[]).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: paused_selector("w1"),
            grace_secs: Some(60),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        next_cmd(&mut h1).await,
        Some(HostCommand::Stop { .. })
    ));
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "stopped".into(),
    })
    .await
    .unwrap();
    let reply = fx
        .call(
            Some(&owner),
            ToolCall::Resume {
                selector: paused_selector("w1"),
            },
        )
        .await
        .expect("the owner resumes its member");
    assert_eq!(reply["paused"], false, "{reply}");
    assert_eq!(reply["placed"], true, "{reply}");
    assert!(agent_of(&fx, "w1").await.paused.is_none());
    // The resume places the agent again with a planned move: the host is
    // whatever qualifies now.
    let launched = match next_cmd(&mut h1).await {
        Some(HostCommand::Launch(o)) => Some(o),
        Some(other) => panic!("expected a launch, got {other:?}"),
        None => None,
    };
    let launched = match launched {
        Some(o) => o,
        None => match next_cmd(&mut h2).await {
            Some(HostCommand::Launch(o)) => o,
            other => panic!("expected a launch, got {other:?}"),
        },
    };
    assert_eq!(launched.reason, MoveReason::Resume);
}

#[tokio::test]
async fn pause_of_a_channel_blocks_a_spawn() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx, &[]).await;
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: PauseSelector::Channel(api()),
            grace_secs: Some(60),
        },
    )
    .await
    .expect("the owner stops its channel");
    // Its only member is stopped.
    assert!(agent_of(&fx, "boss").await.paused.is_some());
    // A spawn into the paused channel is a conflict.
    let err = fx
        .call(
            Some(&owner),
            ToolCall::Spawn {
                key: None,
                task: None,
                stop_on_close: None,
                priority: None,
                bundles: Vec::new(),
                channel: api(),
                prompt: "more".into(),
                harness: Some(Harness::Fake),
                host: None,
                name: Some(agent("w2")),
                model: None,
                role: None,
            },
        )
        .await
        .expect_err("a paused channel takes no spawn");
    assert_eq!(err.message, format!("channel {} is paused", api()), "{err}");
}

#[tokio::test]
async fn pause_cap_stops_the_agent_and_marks_the_claim() {
    let fx = Fx::with(hub(&[("worker", worker(Some(1)))]));
    let (owner, w) = setup(&fx, &[("worker", worker(Some(1)))]).await;
    let task = claim(&fx, &owner, &w).await;
    let mut h1 = fx.commands("h1").await;
    fx.heartbeat_idle("h1", &[(&w, None)]).await;
    // One turn of the day reaches the cap of one.
    stats_heartbeat(&fx, "h1", &[(&w, None)], vec![session_stats("w1", 1)]).await;
    let agent = agent_of(&fx, "w1").await;
    let paused = agent.paused.expect("the cap pauses the agent");
    assert_eq!(paused.why, "cap: 1 turns today");
    // Its claim goes to a person, with the reason.
    let saved = task_of(&fx, task.id).await;
    assert_eq!(saved.status, TaskStatus::NeedsHuman);
    assert_eq!(saved.attention_msg.as_deref(), Some("cap: 1 turns today"));
    // The session stops at the turn end.
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(
        matches!(next_cmd(&mut h1).await, Some(HostCommand::Stop { .. })),
        "the capped agent stops"
    );
}

#[tokio::test]
async fn pause_turn_counter_survives_a_move() {
    let fx = Fx::with(hub(&[("worker", worker(Some(5)))]));
    let (_owner, w) = setup(&fx, &[("worker", worker(Some(5)))]).await;
    stats_heartbeat(&fx, "h1", &[(&w, None)], vec![session_stats("w1", 1)]).await;
    assert_eq!(turns_of(&fx).await, 1, "one turn counted");

    // A move starts a new session whose turns count from zero.
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    fx.call(
        None,
        ToolCall::Move {
            selector: MoveSelector::Agent(agent("w1")),
            to: MoveTarget {
                host: Some(name("h2")),
                harness: None,
            },
            member: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        next_cmd(&mut h1).await,
        Some(HostCommand::PrepareMove { .. })
    ));
    fx.req(CtlRequest::MoveReady {
        agent: agent("w1"),
        session: w.session,
        token: w.token,
    })
    .await
    .unwrap();
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
    let w2 = fx.register("h2", Some("w1"), None).await;
    stats_heartbeat(&fx, "h2", &[(&w2, None)], vec![session_stats("w1", 1)]).await;
    assert_eq!(
        turns_of(&fx).await,
        2,
        "the counter kept the turn of the first session"
    );
}

/// The turn counter of `w1` today, from KV.
async fn turns_of(fx: &Fx) -> u64 {
    let doc = fx
        .state
        .list("turns.")
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.key.starts_with("turns.w1."))
        .expect("the turn counter");
    serde_json::from_slice(&doc.value).unwrap()
}

#[tokio::test]
async fn pause_channel_cap_queues_the_spawn() {
    use borant_core::config::ChannelConfig;

    let cfg = HubConfig {
        channels: BTreeMap::from([(
            api(),
            ChannelConfig {
                max_agents: Some(2),
                bundles: Vec::new(),
            },
        )]),
        ..hub(&[])
    };
    let fx = Fx::with(cfg);
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
    .unwrap();
    let reply = fx
        .call(
            Some(&owner),
            ToolCall::Spawn {
                key: None,
                task: None,
                stop_on_close: None,
                priority: None,
                bundles: Vec::new(),
                channel: api(),
                prompt: "work".into(),
                harness: Some(Harness::Fake),
                host: None,
                name: Some(agent("w1")),
                model: None,
                role: None,
            },
        )
        .await
        .expect("the spawn queues");
    assert_eq!(reply["agent"]["placed"], false, "{reply}");
    let queue = fx.call(Some(&owner), ToolCall::SpawnQueue).await.unwrap();
    let row = queue
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == json!(agent("w1")))
        .expect("the queued spawn");
    assert_eq!(row["queue_reason"], json!("channel full (2/2)"), "{row}");
}
