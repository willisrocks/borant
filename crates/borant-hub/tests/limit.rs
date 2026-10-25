//! The limit restart (spec "Limit restart"): a session stops at the usage
//! limit of its bundle member, and the hub restarts the agent on the same host
//! with the next free member, or waits for the earliest reset.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{Agent, Harness, HostInfo, MemberBundle, MemberState, MoveReason};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, HostCommand, ToolCall};
use borant_core::{AgentId, BundleName, ChannelName, Clock, MemberName, keys};
use common::{Fx, name, next_cmd};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn bundle() -> BundleName {
    "claude_auth".parse().unwrap()
}

fn member(name: &str) -> MemberName {
    name.parse().unwrap()
}

fn agent(name: &str) -> AgentId {
    name.parse().unwrap()
}

/// Registers h1 with a `claude_auth` bundle of `members`.
async fn host(fx: &Fx, members: &[&str]) {
    let info = HostInfo {
        name: name("h1"),
        harnesses: vec![Harness::ClaudeCode],
        launcher: "process".into(),
        max_agents: 4,
        members: BTreeMap::from([(
            bundle(),
            MemberBundle {
                members: members.iter().map(|m| member(m)).collect(),
                harnesses: vec![Harness::ClaudeCode],
            },
        )]),
        ..Default::default()
    };
    fx.req(CtlRequest::RegisterHost { info })
        .await
        .expect("register h1");
}

/// An owner on h1 with a channel to spawn in.
async fn setup(fx: &Fx, members: &[&str]) -> borant_core::proto::Registered {
    host(fx, members).await;
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
    owner
}

async fn spawn(fx: &Fx, owner: &borant_core::proto::Registered, who: &str) {
    fx.call(
        Some(owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            priority: None,
            bundles: Vec::new(),
            channel: api(),
            prompt: "fix the build".into(),
            harness: Some(Harness::ClaudeCode),
            host: Some(name("h1")),
            name: Some(agent(who)),
            model: None,
            role: None,
        },
    )
    .await
    .expect("spawn");
}

/// The state of one member, from KV.
async fn state_of(fx: &Fx, member: &str) -> Option<MemberState> {
    fx.state
        .get(&format!("member.claude_auth.{member}"))
        .await
        .unwrap()
        .map(|d| serde_json::from_slice(&d.value).unwrap())
}

#[tokio::test]
async fn limit_restart_moves_to_the_next_member_on_the_same_host() {
    let fx = Fx::new();
    let owner = setup(&fx, &["a", "b"]).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w1").await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("a")));
        }
        other => panic!("expected a launch, got {other:?}"),
    }
    let w = fx.register("h1", Some("w1"), None).await;
    // The session stopped at the limit of its member.
    let until = fx.clock.now().plus(Duration::from_secs(5 * 3_600));
    let reply = fx
        .req(CtlRequest::MemberLimited {
            agent: w.agent.clone(),
            session: w.session,
            token: w.token,
            bundle: bundle(),
            member: member("a"),
            resets_at: Some(until),
        })
        .await
        .expect("the hub takes the limit");
    assert_eq!(reply["restart"], true, "{reply}");
    assert_eq!(reply["member"], "b", "{reply}");

    // The member is limited until its reset.
    let state = state_of(&fx, "a").await.expect("the state of member a");
    assert_eq!(state.limited_until, Some(until));

    // The old session ends through the planned move path, on the same host.
    match next_cmd(&mut cmds).await {
        Some(HostCommand::PrepareMove { agent: a, session }) => {
            assert_eq!(a, w.agent);
            assert_eq!(session, w.session);
        }
        other => panic!("expected prepare_move, got {other:?}"),
    }
    fx.req(CtlRequest::MoveReady {
        agent: w.agent.clone(),
        session: w.session,
        token: w.token,
    })
    .await
    .expect("the session ends");
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.agent, w.agent);
            assert_eq!(o.harness, Harness::ClaudeCode);
            assert_eq!(o.members.get(&bundle()), Some(&member("b")));
            assert_eq!(o.reason, MoveReason::Limit);
            // The restart stays on the host that reported the limit.
            assert_eq!(o.from_host, Some(name("h1")));
        }
        other => panic!("expected a launch on the next member, got {other:?}"),
    }
    // The agent record keeps the new member for the next launch.
    let doc = fx
        .state
        .get(&keys::agent(&agent("w1")))
        .await
        .unwrap()
        .expect("the agent record");
    let saved: Agent = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(saved.members.get(&bundle()), Some(&member("b")));
}

#[tokio::test]
async fn limit_restart_with_every_member_limited_waits_then_wakes() {
    let fx = Fx::new();
    let owner = setup(&fx, &["a", "b"]).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w1").await;
    assert!(next_cmd(&mut cmds).await.is_some(), "the launch");
    let w = fx.register("h1", Some("w1"), None).await;
    let now = fx.clock.now();
    let until_a = now.plus(Duration::from_secs(3_600));
    // The other member is limited for longer, so `a` wakes the agent first.
    let other = MemberState {
        bundle: bundle(),
        member: member("b"),
        limited_until: Some(now.plus(Duration::from_secs(7_200))),
        waiting: Vec::new(),
        restarts: Vec::new(),
    };
    fx.state
        .create(
            "member.claude_auth.b",
            serde_json::to_vec(&other).unwrap().into(),
        )
        .await
        .unwrap();

    let reply = fx
        .req(CtlRequest::MemberLimited {
            agent: w.agent.clone(),
            session: w.session,
            token: w.token,
            bundle: bundle(),
            member: member("a"),
            resets_at: Some(until_a),
        })
        .await
        .expect("the hub takes the limit");
    assert_eq!(reply["restart"], false, "{reply}");
    // The session stays alive: no move, and the agent waits on `a`.
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "no restart while every member is limited"
    );
    let state = state_of(&fx, "a").await.expect("the state of member a");
    assert_eq!(state.waiting, vec![w.agent.clone()]);

    // Before the reset, nothing happens.
    fx.advance(3_000);
    fx.hub.tick().await.unwrap();
    let early = fx
        .bus
        .pull(&w.agent, 10, Duration::from_millis(50))
        .await
        .unwrap();
    assert!(early.is_empty(), "a wake before the reset");

    // When the limit of `a` passes, the agent gets the wake message once.
    fx.advance(700);
    fx.hub.tick().await.unwrap();
    let got = fx
        .bus
        .pull(&w.agent, 10, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        got[0].envelope.text,
        "Your account limit reset. Continue your task."
    );
    let state = state_of(&fx, "a").await.expect("the state of member a");
    assert_eq!(state.limited_until, None, "the limit is cleared");
    assert!(state.waiting.is_empty(), "the agent is no longer waiting");
    // The other limit stays until its own reset.
    let other = state_of(&fx, "b").await.expect("the state of member b");
    assert!(other.limited_until.is_some());

    // The tick does not wake the agent a second time.
    fx.hub.tick().await.unwrap();
    let again = fx
        .bus
        .pull(&w.agent, 10, Duration::from_millis(50))
        .await
        .unwrap();
    assert!(again.is_empty(), "a second wake: {again:?}");
}

#[tokio::test]
async fn limit_restart_guard_makes_the_agent_wait() {
    let cfg = borant_hub::HubConfig {
        member_rules: borant_core::rules::members::MemberRules {
            unknown_reset_mins: 60,
            max_restarts_per_hour: 1,
        },
        ..borant_hub::HubConfig::default()
    };
    let fx = Fx::with(cfg);
    let owner = setup(&fx, &["a", "b", "c"]).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w1").await;
    assert!(next_cmd(&mut cmds).await.is_some(), "the launch");
    let w = fx.register("h1", Some("w1"), None).await;

    // One restart is allowed.
    let until = fx.clock.now().plus(Duration::from_secs(3_600));
    let reply = fx
        .req(CtlRequest::MemberLimited {
            agent: w.agent.clone(),
            session: w.session,
            token: w.token,
            bundle: bundle(),
            member: member("a"),
            resets_at: Some(until),
        })
        .await
        .unwrap();
    assert_eq!(reply["restart"], true, "{reply}");
    assert!(matches!(
        next_cmd(&mut cmds).await,
        Some(HostCommand::PrepareMove { .. })
    ));
    fx.req(CtlRequest::MoveReady {
        agent: w.agent.clone(),
        session: w.session,
        token: w.token,
    })
    .await
    .unwrap();
    let launched = match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => o,
        other => panic!("expected a launch, got {other:?}"),
    };
    assert_eq!(launched.members.get(&bundle()), Some(&member("b")));

    // The second session hits the limit of `b`; `c` is free, but the guard
    // holds the agent after one restart in an hour.
    let w = fx.register("h1", Some("w1"), None).await;
    assert!(next_cmd(&mut cmds).await.is_none(), "nothing else runs");
    let reply = fx
        .req(CtlRequest::MemberLimited {
            agent: w.agent.clone(),
            session: w.session,
            token: w.token,
            bundle: bundle(),
            member: member("b"),
            resets_at: None,
        })
        .await
        .unwrap();
    assert_eq!(reply["restart"], false, "{reply}");
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the guard holds the agent, so no move"
    );
    // The unknown reset time takes `unknown_reset_mins`.
    let state = state_of(&fx, "b").await.expect("the state of member b");
    assert_eq!(
        state.limited_until,
        Some(fx.clock.now().plus(Duration::from_secs(60 * 60)))
    );
    // The agent waits on the member with the earliest reset, which is `a`
    // here because both reset at the same time.
    let waiting = state_of(&fx, "a").await.expect("the state of member a");
    assert_eq!(waiting.waiting, vec![w.agent.clone()]);
}

#[tokio::test]
async fn limit_restart_of_a_stale_session_is_refused() {
    let fx = Fx::new();
    let owner = setup(&fx, &["a", "b"]).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w1").await;
    assert!(next_cmd(&mut cmds).await.is_some(), "the launch");
    let w = fx.register("h1", Some("w1"), None).await;
    // A limit from a session that no longer holds the lease changes nothing.
    let err = fx
        .req(CtlRequest::MemberLimited {
            agent: w.agent.clone(),
            session: w.session,
            token: borant_core::FencingToken(w.token.0 + 1),
            bundle: bundle(),
            member: member("a"),
            resets_at: None,
        })
        .await
        .expect_err("a stale token is refused");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::StaleToken);
    assert!(
        state_of(&fx, "a").await.is_none(),
        "no member state written"
    );
    assert!(next_cmd(&mut cmds).await.is_none(), "no move");
}
