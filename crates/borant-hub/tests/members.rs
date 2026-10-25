//! Bundle member assignment at launch (spec "Member rules").

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{Agent, Harness, HostInfo, MemberBundle, MemberState, UsageWindow};
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
        },
    )
    .await
    .unwrap();
    owner
}

async fn spawn(fx: &Fx, owner: &borant_core::proto::Registered, who: &str, harness: Harness) {
    fx.call(
        Some(owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            priority: None,
            channel: api(),
            prompt: "fix the build".into(),
            harness: Some(harness),
            host: Some(name("h1")),
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
