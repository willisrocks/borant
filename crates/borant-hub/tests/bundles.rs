//! Bundles on a spawn and on a channel (phase 9 spec, "Spawn and channel
//! bundles").
//!
//! The hub merges the collection, the role, the channel, and the spawn level
//! into `LaunchOrder.bundles`, without duplicates. An agent that spawns names
//! only bundles that it holds; a person names any. A move recomputes the list
//! from `hub.toml`, so a channel bundle changed in the file applies at the
//! next launch.

mod common;

use std::collections::BTreeMap;

use borant_core::config::ChannelConfig;
use borant_core::model::{Agent, Harness, HostInfo, MoveReason};
use borant_core::ports::StateStore;
use borant_core::proto::{
    ApiErrorCode, CtlRequest, HostCommand, MoveSelector, MoveTarget, Registered, ToolCall,
};
use borant_core::rules::roles::{CollectionConfig, RoleConfig};
use borant_core::{AgentId, BundleName, ChannelName, keys};
use borant_hub::HubConfig;
use common::{Fx, name, next_cmd};

fn chan(s: &str) -> ChannelName {
    s.parse().unwrap()
}

fn bundles(names: &[&str]) -> Vec<BundleName> {
    names.iter().map(|n| n.parse().unwrap()).collect()
}

/// A host that defines every bundle that these tests name.
async fn host(fx: &Fx, n: &str) {
    fx.req(CtlRequest::RegisterHost {
        info: HostInfo {
            name: name(n),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 4,
            bundles: bundles(&[
                "github-work",
                "lunaroute",
                "app-stg",
                "prod-readonly",
                "tokens",
            ]),
            ..Default::default()
        },
    })
    .await
    .expect("register the host");
}

async fn channel(fx: &Fx, who: &Registered, name: &str) {
    fx.call(
        Some(who),
        ToolCall::ChannelCreate {
            name: chan(name),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .expect("create the channel");
}

fn spawn_call(name: &str, prompt: &str, role: Option<&str>, give: &[&str]) -> ToolCall {
    ToolCall::Spawn {
        channel: chan(name),
        prompt: prompt.to_owned(),
        harness: None,
        host: None,
        name: None,
        model: None,
        role: role.map(|r| r.parse().unwrap()),
        key: None,
        task: None,
        stop_on_close: None,
        continues: None,
        priority: None,
        bundles: bundles(give),
    }
}

async fn agent_of(fx: &Fx, id: &AgentId) -> Agent {
    let doc = fx
        .state
        .get(&keys::agent(id))
        .await
        .unwrap()
        .expect("the agent exists");
    serde_json::from_slice(&doc.value).unwrap()
}

/// Rule: the launch bundles are the collection's, then the role's, then the
/// channel's, then the spawn's, without duplicates. A person may name a bundle
/// that nobody in the channel holds.
#[tokio::test]
async fn bundles_merge_collection_role_channel_and_spawn() {
    let fx = Fx::with(HubConfig {
        collections: BTreeMap::from([(
            "work".parse().unwrap(),
            CollectionConfig {
                bundles: bundles(&["github-work"]),
                git_author: None,
                mirror_issues: false,
            },
        )]),
        roles: BTreeMap::from([(
            "worker".parse().unwrap(),
            RoleConfig {
                harness: Some(Harness::Fake),
                bundles: bundles(&["lunaroute"]),
                ..Default::default()
            },
        )]),
        channels: BTreeMap::from([(
            chan("work-api"),
            ChannelConfig {
                max_agents: None,
                // A repeat of the role's bundle adds nothing.
                bundles: bundles(&["app-stg", "lunaroute"]),
            },
        )]),
        ..Default::default()
    });
    host(&fx, "h1").await;
    // The channel takes the collection of its creator, and so does a spawn by
    // a person in it.
    let owner = fx.register_in("h1", Some("boss"), None, Some("work")).await;
    channel(&fx, &owner, "work-api").await;
    let mut h1 = fx.commands("h1").await;

    fx.call(
        None,
        spawn_call(
            "work-api",
            "go",
            Some("worker"),
            &["prod-readonly", "github-work"],
        ),
    )
    .await
    .expect("a person names any bundle");

    let Some(HostCommand::Launch(order)) = next_cmd(&mut h1).await else {
        panic!("no launch happened");
    };
    assert_eq!(
        order.bundles,
        bundles(&["github-work", "lunaroute", "app-stg", "prod-readonly"]),
        "collection, role, channel, spawn, without duplicates"
    );
    let spawned = agent_of(&fx, &order.agent).await;
    assert_eq!(
        spawned.bundles,
        bundles(&["prod-readonly", "github-work"]),
        "the agent keeps the spawn level only"
    );
}

/// Rule: an agent that spawns names only bundles that it holds.
#[tokio::test]
async fn bundles_a_child_cannot_give_a_bundle_it_lacks() {
    let fx = Fx::new();
    host(&fx, "h1").await;
    let owner = fx.register("h1", Some("boss"), None).await;
    channel(&fx, &owner, "api").await;
    let parent = fx.register("h1", Some("parent"), Some("api")).await;

    let err = fx
        .call(
            Some(&parent),
            spawn_call("api", "go", None, &["prod-readonly"]),
        )
        .await
        .expect_err("a parent without the bundle may not give it");
    assert_eq!(err.code, ApiErrorCode::Denied);
    assert_eq!(err.message, "bundle prod-readonly is not yours to give");
}

/// Rule: an agent that spawns may name a bundle of its own merged list.
#[tokio::test]
async fn bundles_a_child_can_give_a_bundle_it_holds() {
    let fx = Fx::with(HubConfig {
        collections: BTreeMap::from([(
            "work".parse().unwrap(),
            CollectionConfig {
                bundles: bundles(&["github-work"]),
                git_author: None,
                mirror_issues: false,
            },
        )]),
        ..Default::default()
    });
    host(&fx, "h1").await;
    let parent = fx
        .register_in("h1", Some("parent"), None, Some("work"))
        .await;
    channel(&fx, &parent, "work-api").await;

    let v = fx
        .call(
            Some(&parent),
            spawn_call("work-api", "go", None, &["github-work"]),
        )
        .await
        .expect("the parent holds the bundle");
    let id: AgentId = v["agent"]["agent"].as_str().unwrap().parse().unwrap();
    let child = agent_of(&fx, &id).await;
    assert_eq!(
        child.bundles,
        bundles(&["github-work"]),
        "the spawn level is what the call named, and the collection level is not stored"
    );
    assert_eq!(child.parent, Some(parent.agent));
}

/// Rule: a move recomputes the bundles, so a channel that the agent joined
/// after its spawn shows up at the next launch.
#[tokio::test]
async fn bundles_a_move_recomputes_the_channel_bundles() {
    let fx = Fx::with(HubConfig {
        channels: BTreeMap::from([
            (
                chan("api"),
                ChannelConfig {
                    max_agents: None,
                    bundles: bundles(&["app-stg"]),
                },
            ),
            (
                chan("tokens"),
                ChannelConfig {
                    max_agents: None,
                    bundles: bundles(&["tokens"]),
                },
            ),
        ]),
        ..Default::default()
    });
    host(&fx, "h1").await;
    host(&fx, "h2").await;
    let owner = fx.register("h1", Some("boss"), None).await;
    channel(&fx, &owner, "api").await;
    channel(&fx, &owner, "tokens").await;
    let w = fx.register("h1", Some("w"), Some("api")).await;
    fx.call(
        Some(&w),
        ToolCall::ChannelJoin {
            name: chan("tokens"),
        },
    )
    .await
    .expect("join the second channel");

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
            member: None,
        },
    )
    .await
    .expect("the move starts");
    match next_cmd(&mut h1).await {
        Some(HostCommand::PrepareMove { .. }) => {}
        other => panic!("expected prepare_move, got {other:?}"),
    }
    fx.req(CtlRequest::MoveReady {
        agent: w.agent.clone(),
        session: w.session,
        token: w.token,
    })
    .await
    .expect("the old session stopped");

    let Some(HostCommand::Launch(order)) = next_cmd(&mut h2).await else {
        panic!("no launch happened");
    };
    assert_eq!(order.reason, MoveReason::Planned);
    assert_eq!(
        order.bundles,
        bundles(&["app-stg", "tokens"]),
        "the move merges every channel of the agent from hub.toml"
    );
}

/// An `Agent` that an older hub wrote has no `bundles` field, and still reads.
#[test]
fn bundles_old_agent_json_parses() {
    let mut old = serde_json::to_value(Agent::default()).unwrap();
    old.as_object_mut().unwrap().remove("bundles");
    let parsed: Agent = serde_json::from_value(old).expect("an agent without bundles");
    assert!(parsed.bundles.is_empty());
}
