mod common;

use borant_core::model::{Harness, MoveReason, NewTask, Target, Task, TaskStatus};
use borant_core::ports::MessageBus;
use borant_core::proto::{CtlRequest, HostCommand, MoveSelector, MoveTarget, ToolCall};
use borant_core::{ChannelName, subjects, testing};
use common::{Fx, name, next_cmd};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// Two hosts, an owner on h1 with a channel, and a worker on h1.
async fn setup(
    fx: &Fx,
) -> (
    borant_core::proto::Registered,
    borant_core::proto::Registered,
) {
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
    let w = fx.register("h1", Some("w"), Some("api")).await;
    (owner, w)
}

/// Lets h1 go silent while h2 keeps sending heartbeats.
async fn h1_sleeps(fx: &Fx, secs: u64) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(10);
        fx.advance(step);
        fx.heartbeat("h2", &[]).await;
        fx.hub.tick().await.unwrap();
        left -= step;
    }
}

async fn claim(
    fx: &Fx,
    owner: &borant_core::proto::Registered,
    w: &borant_core::proto::Registered,
) -> Task {
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
    t
}

#[tokio::test]
async fn expired_member_with_claim_is_placed_after_grace() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    let mut h2 = fx.commands("h2").await;
    h1_sleeps(&fx, 60).await;
    assert!(
        next_cmd(&mut h2).await.is_none(),
        "no takeover before the grace period"
    );
    h1_sleeps(&fx, 100).await;
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.agent, w.agent);
            assert_eq!(o.reason, MoveReason::Takeover);
            assert!(
                o.prompt.contains("You continue Borant agent w on host h2"),
                "{}",
                o.prompt
            );
            assert_eq!(o.channels, vec![api()]);
        }
        other => panic!("expected a launch, got {other:?}"),
    }
    // The new session on h2 gets the lease with a higher token.
    let r = fx.register("h2", Some("w"), None).await;
    assert!(r.token > w.token);
}

#[tokio::test]
async fn expired_idle_agent_waits_until_message() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut h2 = fx.commands("h2").await;
    h1_sleeps(&fx, 200).await;
    // The owner is idle too, so nothing moves.
    assert!(next_cmd(&mut h2).await.is_none());
    let env = testing::envelope(Target::Agent(w.agent.clone()), "wake up");
    fx.bus
        .publish(&subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    h1_sleeps(&fx, 2).await;
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => assert_eq!(o.agent, w.agent),
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn non_member_never_moves() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let lone = fx.register("h1", Some("lone"), None).await;
    let env = testing::envelope(Target::Agent(lone.agent.clone()), "hi");
    fx.bus
        .publish(&subjects::agent(&lone.agent), &env)
        .await
        .unwrap();
    let mut h2 = fx.commands("h2").await;
    h1_sleeps(&fx, 300).await;
    assert!(next_cmd(&mut h2).await.is_none());
}

#[tokio::test]
async fn reserved_lease_expires_and_replaces() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    let mut h2 = fx.commands("h2").await;
    h1_sleeps(&fx, 160).await;
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
    // No session registers within the reservation time.
    h1_sleeps(&fx, 330).await;
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
}

#[tokio::test]
async fn launch_failed_tries_other_host() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    fx.host("h3", &[Harness::Fake]).await;
    claim(&fx, &owner, &w).await;
    let mut h2 = fx.commands("h2").await;
    let mut h3 = fx.commands("h3").await;
    for _ in 0..16 {
        fx.advance(10);
        fx.heartbeat("h2", &[]).await;
        fx.heartbeat("h3", &[]).await;
        fx.hub.tick().await.unwrap();
    }
    let (first, mut other) = match (next_cmd(&mut h2).await, next_cmd(&mut h3).await) {
        (Some(HostCommand::Launch(_)), None) => ("h2", h3),
        (None, Some(HostCommand::Launch(_))) => ("h3", h2),
        other => panic!("expected one launch, got {other:?}"),
    };
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: name(first),
        error: "no git".into(),
    })
    .await
    .unwrap();
    fx.heartbeat("h2", &[]).await;
    fx.heartbeat("h3", &[]).await;
    fx.hub.tick().await.unwrap();
    assert!(matches!(
        next_cmd(&mut other).await,
        Some(HostCommand::Launch(_))
    ));
}

#[tokio::test]
async fn claims_return_to_open_after_claim_timeout() {
    let fx = Fx::new();
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
    let w = fx.register("h1", Some("w"), Some("api")).await;
    let t = claim(&fx, &owner, &w).await;
    // No other host exists, so nobody can take the worker over.
    for _ in 0..190 {
        fx.advance(10);
        fx.hub.tick().await.unwrap();
    }
    let v = fx
        .call(
            None,
            ToolCall::TaskGet {
                channel: api(),
                id: t.id,
            },
        )
        .await
        .unwrap();
    let t: Task = serde_json::from_value(v).unwrap();
    assert_eq!(t.status, TaskStatus::Open);
    assert_eq!(t.claimed_by, None);
}

#[tokio::test]
async fn planned_move_waits_for_move_ready() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    fx.call(
        None,
        ToolCall::Move {
            selector: MoveSelector::Agent(w.agent.clone()),
            to: MoveTarget {
                host: Some(name("h2")),
                harness: None,
            },
        },
    )
    .await
    .unwrap();
    match next_cmd(&mut h1).await {
        Some(HostCommand::PrepareMove { agent, session }) => {
            assert_eq!(agent, w.agent);
            assert_eq!(session, w.session);
        }
        other => panic!("expected prepare_move, got {other:?}"),
    }
    assert!(next_cmd(&mut h2).await.is_none());
    fx.req(CtlRequest::MoveReady {
        agent: w.agent.clone(),
        session: w.session,
        token: w.token,
    })
    .await
    .unwrap();
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => assert_eq!(o.reason, MoveReason::Planned),
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn planned_move_to_a_full_host_fails_and_keeps_the_session() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    for a in ["x1", "x2", "x3", "x4"] {
        fx.register("h2", Some(a), None).await;
    }
    let mut h1 = fx.commands("h1").await;
    let err = fx
        .call(
            None,
            ToolCall::Move {
                selector: MoveSelector::Agent(w.agent.clone()),
                to: MoveTarget {
                    host: Some(name("h2")),
                    harness: None,
                },
            },
        )
        .await
        .expect_err("h2 is full");
    assert!(
        err.to_string().contains("no host can take agent w"),
        "{err}"
    );
    assert!(next_cmd(&mut h1).await.is_none());
}

#[tokio::test]
async fn drain_moves_all_agents() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let mut h1 = fx.commands("h1").await;
    let v = fx
        .call(None, ToolCall::Drain { host: name("h1") })
        .await
        .unwrap();
    let moving: Vec<String> = serde_json::from_value(v["moving"].clone()).unwrap();
    assert_eq!(moving.len(), 2);
    let mut sessions = vec![];
    while let Some(HostCommand::PrepareMove { session, .. }) = next_cmd(&mut h1).await {
        sessions.push(session);
    }
    sessions.sort();
    let mut want = vec![owner.session, w.session];
    want.sort();
    assert_eq!(sessions, want);
}

#[tokio::test]
async fn restart_grace_prevents_mass_takeover() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    // The hub was down for 10 minutes and nobody renewed leases.
    fx.advance(600);
    fx.hub.restart_grace().await.unwrap();
    fx.hub.tick().await.unwrap();
    assert!(fx.heartbeat("h1", &[&owner, &w]).await.lost.is_empty());
}

#[tokio::test]
async fn spawn_places_new_agent_with_prompt() {
    let fx = Fx::new();
    let (owner, _w) = setup(&fx).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    let v = fx
        .call(
            Some(&owner),
            ToolCall::Spawn {
                channel: api(),
                prompt: "Fix the build".into(),
                harness: None,
                host: Some(name("h2")),
                name: Some("helper".parse().unwrap()),
                model: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(v["agent"]["agent"], "helper");
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.prompt, "Fix the build");
            assert_eq!(o.reason, MoveReason::Spawn);
        }
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn stale_end_session_does_not_take_the_lease() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    h1_sleeps(&fx, 160).await;
    // w now runs on h2.
    let w2 = fx.register("h2", Some("w"), None).await;
    let mut h3 = fx.commands("h2").await;
    // h1 comes back, so it can take w again.
    fx.heartbeat("h1", &[]).await;
    fx.call(
        None,
        ToolCall::Move {
            selector: MoveSelector::Agent(w.agent.clone()),
            to: MoveTarget {
                host: Some(name("h1")),
                harness: None,
            },
        },
    )
    .await
    .unwrap();
    // The old session on h1 ends with its old token.
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "old host".into(),
    })
    .await
    .unwrap();
    while let Some(cmd) = next_cmd(&mut h3).await {
        assert!(
            matches!(cmd, HostCommand::PrepareMove { .. }),
            "unexpected {cmd:?}"
        );
    }
    // The live session on h2 keeps its lease.
    assert!(fx.heartbeat("h2", &[&w2]).await.lost.is_empty());
}

#[tokio::test]
async fn failed_spawn_is_placed_again_with_its_prompt() {
    let fx = Fx::new();
    let (owner, _w) = setup(&fx).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    fx.call(
        Some(&owner),
        ToolCall::Spawn {
            channel: api(),
            prompt: "Fix the build".into(),
            harness: None,
            host: Some(name("h2")),
            name: Some("helper".parse().unwrap()),
            model: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
    fx.req(CtlRequest::LaunchFailed {
        agent: "helper".parse().unwrap(),
        host: name("h2"),
        error: "clone failed".into(),
    })
    .await
    .unwrap();
    // The failed host is skipped until a new host registers.
    fx.host("h3", &[Harness::Fake]).await;
    let mut h3 = fx.commands("h3").await;
    fx.heartbeat("h3", &[]).await;
    fx.hub.tick().await.unwrap();
    match next_cmd(&mut h3).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.prompt, "Fix the build");
            assert_eq!(o.reason, MoveReason::Spawn);
        }
        other => panic!("expected a second launch, got {other:?}"),
    }
}

#[tokio::test]
async fn claims_of_an_unmovable_agent_time_out() {
    let fx = Fx::new();
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
    // No repository, so the worker cannot move.
    let w = fx
        .try_register("h1", Some("w"), Some("api"), false)
        .await
        .unwrap();
    assert!(!w.movable);
    let t = claim(&fx, &owner, &w).await;
    for _ in 0..190 {
        fx.advance(10);
        fx.heartbeat("h1", &[&owner]).await;
        fx.hub.tick().await.unwrap();
    }
    let v = fx
        .call(
            None,
            ToolCall::TaskGet {
                channel: api(),
                id: t.id,
            },
        )
        .await
        .unwrap();
    let t: Task = serde_json::from_value(v).unwrap();
    assert_eq!(t.claimed_by, None);
    assert_eq!(t.status, TaskStatus::Open);
}

#[tokio::test]
async fn restarted_hub_waits_for_heartbeats_before_placing() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    // Both hosts went silent long ago, and the hub restarts.
    h1_sleeps(&fx, 40).await;
    fx.advance(600);
    fx.hub.restart_grace().await.unwrap();
    fx.hub.tick().await.unwrap();
    assert!(next_cmd(&mut h1).await.is_none());
    assert!(next_cmd(&mut h2).await.is_none());
    // Only h2 comes back. After the quiet period, w goes to h2.
    for _ in 0..5 {
        fx.advance(10);
        fx.heartbeat("h2", &[]).await;
        fx.hub.tick().await.unwrap();
    }
    assert!(next_cmd(&mut h1).await.is_none(), "h1 sent no heartbeat");
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
}

#[tokio::test]
async fn expired_session_with_no_takeover_takes_its_lease_back() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    let mut h2 = fx.commands("h2").await;
    // Past the lease, so the tick expires it, but inside the grace period.
    h1_sleeps(&fx, 60).await;
    let reply = fx.heartbeat("h1", &[&w]).await;
    assert!(reply.lost.is_empty(), "{reply:?}");
    assert_eq!(reply.renewed.len(), 1);
    assert_eq!(reply.renewed[0].session, w.session);
    assert_eq!(reply.renewed[0].token.0, w.token.0 + 1);
    let w_new = borant_core::proto::Registered {
        token: reply.renewed[0].token,
        ..w.clone()
    };
    // The new token renews normally.
    let next = fx.heartbeat("h1", &[&w_new]).await;
    assert!(next.lost.is_empty() && next.renewed.is_empty(), "{next:?}");
    // The agent runs on h1 again, so no takeover follows.
    for _ in 0..20 {
        fx.advance(10);
        fx.heartbeat("h1", &[&w_new]).await;
        fx.heartbeat("h2", &[]).await;
        fx.hub.tick().await.unwrap();
    }
    assert!(next_cmd(&mut h2).await.is_none(), "no takeover");
    assert!(fx.heartbeat("h1", &[&w_new]).await.lost.is_empty());
}

#[tokio::test]
async fn lost_reclaim_reply_gives_the_same_token_again() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    h1_sleeps(&fx, 60).await;
    let first = fx.heartbeat("h1", &[&w]).await;
    let second = fx.heartbeat("h1", &[&w]).await;
    assert!(first.lost.is_empty() && second.lost.is_empty());
    assert_eq!(first.renewed.len(), 1);
    assert_eq!(first.renewed, second.renewed);
    assert_eq!(first.renewed[0].token.0, w.token.0 + 1);
}

#[tokio::test]
async fn expired_session_after_takeover_stays_lost() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    let mut h2 = fx.commands("h2").await;
    h1_sleeps(&fx, 160).await;
    assert!(matches!(
        next_cmd(&mut h2).await,
        Some(HostCommand::Launch(_))
    ));
    // The lease is reserved for h2.
    let reply = fx.heartbeat("h1", &[&w]).await;
    assert_eq!(reply.lost, vec![w.session]);
    assert!(reply.renewed.is_empty());
    // The new session on h2 holds it.
    let _w2 = fx.register("h2", Some("w"), None).await;
    let reply = fx.heartbeat("h1", &[&w]).await;
    assert_eq!(reply.lost, vec![w.session]);
    assert!(reply.renewed.is_empty());
}
