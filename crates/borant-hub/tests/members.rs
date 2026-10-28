//! Bundle member assignment at launch (spec "Member rules").

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{
    Agent, ChannelKind, Harness, HostInfo, MemberBundle, MemberState, SpawnRecord, SpawnState,
    UsageWindow,
};
use borant_core::ports::StateStore;
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

/// Registers h1 with a `claude_auth` bundle: `members` hold `harnesses`.
async fn host(fx: &Fx, harnesses: &[Harness], members: &[&str]) {
    let info = HostInfo {
        name: name("h1"),
        harnesses: harnesses.to_vec(),
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
async fn setup(fx: &Fx) -> borant_core::proto::Registered {
    host(fx, &[Harness::ClaudeCode, Harness::Omp], &["a", "b"]).await;
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

async fn spawn(fx: &Fx, owner: &borant_core::proto::Registered, who: &str, harness: Harness) {
    spawn_on(fx, owner, who, harness, "h1").await;
}

/// A spawn that names its host.
async fn spawn_on(
    fx: &Fx,
    owner: &borant_core::proto::Registered,
    who: &str,
    harness: Harness,
    host: &str,
) {
    fx.call(
        Some(owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            bundles: Vec::new(),
            channel: api(),
            prompt: "fix the build".into(),
            harness: Some(harness),
            host: Some(name(host)),
            name: Some(agent(who)),
            model: None,
            role: None,
        },
    )
    .await
    .expect("spawn");
}

#[tokio::test]
async fn members_at_spawn_assign_the_first_member() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w1", Harness::ClaudeCode).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("a")));
        }
        other => panic!("expected a launch, got {other:?}"),
    }
    // The agent record keeps the member for its next launch.
    let doc = fx
        .state
        .get(&keys::agent(&agent("w1")))
        .await
        .unwrap()
        .expect("the agent record");
    let saved: Agent = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(saved.members.get(&bundle()), Some(&member("a")));
}

#[tokio::test]
async fn members_omp_launch_gets_no_claude_code_member() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w2", Harness::Omp).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.harness, Harness::Omp);
            assert!(
                o.members.is_empty(),
                "omp must not get a claude-code-only member: {:?}",
                o.members
            );
        }
        other => panic!("expected a launch, got {other:?}"),
    }
}

/// A failed pool launch on h1 must not fall back to h2, which has no
/// `[secrets]`: the launch would run on the login of the box instead of the
/// pool. The spawn waits in the queue, and the reason names the bundle.
#[tokio::test]
async fn members_a_failed_pool_launch_does_not_fall_back_to_a_host_without_the_bundle() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    // h2 runs Claude Code and holds no secrets.
    fx.host("h2", &[Harness::ClaudeCode]).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    spawn(&fx, &owner, "w1", Harness::ClaudeCode).await;
    match next_cmd(&mut h1).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("a")), "{o:?}");
        }
        other => panic!("expected the pool launch, got {other:?}"),
    }
    // The launch stalls on the first-run screen; the watchdog fails it.
    fx.req(CtlRequest::LaunchFailed {
        agent: agent("w1"),
        host: name("h1"),
        error: "first-run screen".into(),
        timeout_secs: Some(90.0),
    })
    .await
    .unwrap();
    // The hub tries again. h1 failed the agent, h2 has no member bundle.
    fx.hub.tick().await.unwrap();
    assert!(
        next_cmd(&mut h2).await.is_none(),
        "a host without the member bundle must not take the agent"
    );
    let doc = fx
        .state
        .get(&keys::spawn(&agent("w1")))
        .await
        .unwrap()
        .expect("the spawn record");
    let record: SpawnRecord = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(record.state, SpawnState::Queued, "{record:?}");
    assert!(
        record
            .last_reject
            .iter()
            .any(|(host, why)| host == &name("h2") && why == "no member bundle claude_auth"),
        "the queue names the bundle: {:?}",
        record.last_reject
    );
    assert!(
        record
            .last_reject
            .iter()
            .any(|(host, why)| host == &name("h1") && why == "failed recently"),
        "{:?}",
        record.last_reject
    );
}

/// An agent with no member keeps today's placement: a host without secrets
/// takes it.
#[tokio::test]
async fn members_no_member_agent_still_lands_on_a_host_without_the_bundle() {
    let fx = Fx::new();
    fx.host("h2", &[Harness::ClaudeCode]).await;
    let owner = fx.register("h2", Some("boss"), None).await;
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
    let mut cmds = fx.commands("h2").await;
    spawn_on(&fx, &owner, "plain", Harness::ClaudeCode, "h2").await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert!(o.members.is_empty(), "{:?}", o.members);
        }
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn members_usage_report_reaches_list_members_and_the_pick() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let now = fx.clock.now();
    fx.call(
        None,
        ToolCall::UsageReport {
            bundle: bundle(),
            member: member("b"),
            windows: vec![UsageWindow {
                name: "5h".into(),
                used: 0.95,
                resets_at: now.plus(Duration::from_secs(90 * 60)),
            }],
        },
    )
    .await
    .expect("a human may report usage");

    let listed = fx
        .call(None, ToolCall::ListMembers)
        .await
        .expect("list members");
    let members = listed[&bundle().to_string()]
        .as_array()
        .expect("the bundle lists its members");
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["member"], "a");
    assert!(members[0]["report"].is_null());
    assert_eq!(members[1]["member"], "b");
    assert_eq!(members[1]["report"]["fresh"], true);
    assert_eq!(members[1]["report"]["windows"][0]["used"], 0.95);

    // The member with a fresh report comes before the one without: rule 4.
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "w3", Harness::ClaudeCode).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("b")));
        }
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn members_usage_report_outside_the_share_is_refused() {
    let fx = Fx::new();
    setup(&fx).await;
    let now = fx.clock.now();
    let err = fx
        .call(
            None,
            ToolCall::UsageReport {
                bundle: bundle(),
                member: member("a"),
                windows: vec![UsageWindow {
                    name: "5h".into(),
                    used: 1.5,
                    resets_at: now,
                }],
            },
        )
        .await
        .expect_err("a share above 1 is invalid");
    assert!(err.message.contains("between 0 and 1"), "{err}");
}

#[tokio::test]
async fn members_every_member_limited_keeps_the_spawn_pending() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let now = fx.clock.now();
    for (who, hours) in [("a", 1), ("b", 2)] {
        let state = MemberState {
            bundle: bundle(),
            member: member(who),
            limited_until: Some(now.plus(Duration::from_secs(hours * 3_600))),
            waiting: vec![],
            restarts: vec![],
        };
        fx.state
            .create(
                &format!("member.claude_auth.{who}"),
                serde_json::to_vec(&state).unwrap().into(),
            )
            .await
            .unwrap();
    }
    let mut cmds = fx.commands("h1").await;
    spawn(&fx, &owner, "waiter", Harness::ClaudeCode).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "nothing launches while every member is limited"
    );
    // The agent waits on the member with the earliest reset.
    let doc = fx
        .state
        .get("member.claude_auth.a")
        .await
        .unwrap()
        .expect("the state of member a");
    let state: MemberState = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(state.waiting, vec![agent("waiter")]);

    // After that reset the next tick places the agent.
    fx.advance(3_600);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("a")));
        }
        other => panic!("expected a launch after the reset, got {other:?}"),
    }
}

/// A claude-code agent of h1, launched with member `a` and registered, so it
/// holds a lease and a switch goes through the planned move path.
async fn running(
    fx: &Fx,
    owner: &borant_core::proto::Registered,
    who: &str,
) -> borant_core::proto::Registered {
    spawn(fx, owner, who, Harness::ClaudeCode).await;
    let v = fx
        .req(CtlRequest::RegisterSession(
            borant_core::proto::RegisterSession {
                session: borant_core::SessionId::new(),
                host: name("h1"),
                harness: Harness::ClaudeCode,
                pid: Some(1),
                cwd: "/work".into(),
                agent: Some(agent(who)),
                ..Default::default()
            },
        ))
        .await
        .expect("register the session of the switched agent");
    serde_json::from_value(v).unwrap()
}

/// `MoveTarget` of a switch that stays where it is.
fn stays() -> borant_core::proto::MoveTarget {
    borant_core::proto::MoveTarget::default()
}

#[tokio::test]
async fn members_switch_relaunches_with_the_named_member() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let mut cmds = fx.commands("h1").await;
    let w = running(&fx, &owner, "w1").await;
    // The spawn launched it with the first member.
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.members.get(&bundle()), Some(&member("a")), "{o:?}");
        }
        other => panic!("expected the spawn launch, got {other:?}"),
    }

    fx.call(
        None,
        ToolCall::Move {
            selector: borant_core::proto::MoveSelector::Agent(w.agent.clone()),
            to: stays(),
            member: Some(member("b")),
        },
    )
    .await
    .expect("switch to member b");

    // The same host stops the session and relaunches it with the new member.
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
    .unwrap();
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.reason, borant_core::model::MoveReason::Planned);
            assert_eq!(o.members.get(&bundle()), Some(&member("b")), "{o:?}");
        }
        other => panic!("expected the relaunch, got {other:?}"),
    }
    // The record keeps the member for the next launch too.
    let doc = fx.state.get(&keys::agent(&w.agent)).await.unwrap().unwrap();
    let saved: Agent = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(saved.members.get(&bundle()), Some(&member("b")));
}

#[tokio::test]
async fn members_switch_to_an_unknown_member_is_invalid() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let mut cmds = fx.commands("h1").await;
    let w = running(&fx, &owner, "w1").await;
    assert!(matches!(
        next_cmd(&mut cmds).await,
        Some(HostCommand::Launch(_))
    ));

    let err = fx
        .call(
            None,
            ToolCall::Move {
                selector: borant_core::proto::MoveSelector::Agent(w.agent.clone()),
                to: stays(),
                member: Some(member("ghost")),
            },
        )
        .await
        .expect_err("ghost is not a member of claude_auth on h1");
    assert_eq!(
        err.code,
        borant_core::proto::ApiErrorCode::Invalid,
        "{err:?}"
    );
    assert_eq!(err.message, "no member ghost in claude_auth on h1");
    // Nothing stopped: no prepare_move reached the host.
    assert!(next_cmd(&mut cmds).await.is_none());
}

#[tokio::test]
async fn members_switch_to_a_limited_member_is_a_conflict() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let mut cmds = fx.commands("h1").await;
    let w = running(&fx, &owner, "w1").await;
    assert!(matches!(
        next_cmd(&mut cmds).await,
        Some(HostCommand::Launch(_))
    ));
    let until = fx.clock.now().plus(Duration::from_secs(3_600));
    let state = MemberState {
        bundle: bundle(),
        member: member("b"),
        limited_until: Some(until),
        waiting: vec![],
        restarts: Vec::new(),
    };
    fx.state
        .create(
            "member.claude_auth.b",
            serde_json::to_vec(&state).unwrap().into(),
        )
        .await
        .unwrap();

    let err = fx
        .call(
            None,
            ToolCall::Move {
                selector: borant_core::proto::MoveSelector::Agent(w.agent.clone()),
                to: stays(),
                member: Some(member("b")),
            },
        )
        .await
        .expect_err("member b is limited");
    assert_eq!(
        err.code,
        borant_core::proto::ApiErrorCode::Conflict,
        "{err:?}"
    );
    assert_eq!(err.message, format!("member b is limited until {until}"));
    assert!(next_cmd(&mut cmds).await.is_none());
}

/// The state of one member of `claude_auth`.
async fn state_of(fx: &Fx, who: &str) -> Option<MemberState> {
    let doc = fx
        .state
        .get(&format!("member.claude_auth.{who}"))
        .await
        .unwrap()?;
    Some(serde_json::from_slice(&doc.value).unwrap())
}

#[tokio::test]
async fn members_a_limit_adds_a_restart_time_and_drops_an_old_one() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    let now = fx.clock.now();
    // A restart from before the week drops at the next write.
    let old = MemberState {
        bundle: bundle(),
        member: member("a"),
        limited_until: None,
        waiting: vec![],
        restarts: vec![now.minus(Duration::from_secs(8 * 24 * 3_600))],
    };
    fx.state
        .create(
            "member.claude_auth.a",
            serde_json::to_vec(&old).unwrap().into(),
        )
        .await
        .unwrap();
    let w = fx.register("h1", Some("w"), Some("api")).await;
    let until = now.plus(Duration::from_secs(3_600));
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

    // One time for this limit, and the one of eight days ago is gone.
    let state = state_of(&fx, "a").await.expect("the state of member a");
    assert_eq!(state.restarts, vec![now]);

    // `borant members` reads the list back.
    let listed = fx
        .call(Some(&owner), ToolCall::ListMembers)
        .await
        .expect("members");
    let a = listed["claude_auth"]
        .as_array()
        .expect("the members of the bundle")
        .iter()
        .find(|m| m["member"] == "a")
        .cloned()
        .expect("member a");
    assert_eq!(a["restarts"].as_array().map(Vec::len), Some(1), "{a}");

    // A second limit adds a second time.
    fx.advance(60);
    let w2 = fx.register("h1", Some("w2"), Some("api")).await;
    fx.req(CtlRequest::MemberLimited {
        agent: w2.agent.clone(),
        session: w2.session,
        token: w2.token,
        bundle: bundle(),
        member: member("b"),
        resets_at: Some(until),
    })
    .await
    .expect("the hub takes the second limit");
    let b = state_of(&fx, "b").await.expect("the state of member b");
    assert_eq!(b.restarts.len(), 1);
}

/// The roster of `borant agents` names the member of each bundle that the
/// launch picked, which the members e2e scenario reads.
#[tokio::test]
async fn members_the_agent_roster_names_the_member() {
    let fx = Fx::new();
    let owner = setup(&fx).await;
    spawn(&fx, &owner, "w1", Harness::ClaudeCode).await;

    let agents = fx
        .req(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::ListAgents,
        })
        .await
        .unwrap();
    let w1 = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "w1")
        .expect("w1 in the roster");
    assert_eq!(
        w1["members"][bundle().as_str()],
        serde_json::json!(member("a").as_str()),
        "the roster names the picked member: {w1}"
    );
}
