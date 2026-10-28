//! Placement uses the rule of the role of an agent (spec "Placement with
//! tags").

mod common;

use std::collections::BTreeMap;

use borant_core::model::{Agent, ChannelKind, Harness, Target};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{HostCommand, Registered, ToolCall};
use borant_core::rules::roles::RoleConfig;
use borant_core::{AgentId, ChannelName, RoleName, Tag, keys, subjects, testing};
use borant_hub::HubConfig;
use common::{Fx, next_cmd};

/// The heartbeat interval of the hub.
const BEAT: u64 = 10;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn worker() -> RoleName {
    "worker".parse().unwrap()
}

/// A `worker` role that runs only on a host with the `vps` tag.
fn roles() -> BTreeMap<RoleName, RoleConfig> {
    let require: Vec<Tag> = vec!["vps".parse().unwrap()];
    BTreeMap::from([(
        worker(),
        RoleConfig {
            require,
            ..Default::default()
        },
    )])
}

/// Sets the role of a registered agent.
async fn set_role(fx: &Fx, agent: &AgentId, role: &RoleName) {
    let key = keys::agent(agent);
    let e = fx.state.get(&key).await.unwrap().expect("the agent exists");
    let mut a: Agent = serde_json::from_slice(&e.value).unwrap();
    a.role = Some(role.clone());
    let bytes = serde_json::to_vec(&a).unwrap();
    fx.state
        .update(&key, bytes.into(), e.revision)
        .await
        .unwrap();
}

/// The agents of the fixture.
struct Setup {
    owner: Registered,
    /// Two agents that keep `h2` busier than `h1`.
    x1: Registered,
    x2: Registered,
    w: Registered,
}

/// An owner on `h1` with channel `api` and a `worker` on `h1`, and two agents
/// on the only host that carries `vps`, so `h1` wins the plain order.
async fn setup(fx: &Fx) -> Setup {
    fx.tagged_host("h1", &[Harness::Fake], None, &[]).await;
    fx.tagged_host("h2", &[Harness::Fake], None, &["vps"]).await;
    let x1 = fx.register("h2", Some("x1"), None).await;
    let x2 = fx.register("h2", Some("x2"), None).await;
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
    let w = fx.register("h1", Some("w"), Some("api")).await;
    set_role(fx, &w.agent, &worker()).await;
    Setup { owner, x1, x2, w }
}

/// Heartbeats both hosts, so only the lease of `w` expires, and ticks.
async fn w_expires(fx: &Fx, s: &Setup, secs: u64) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(BEAT);
        fx.advance(step);
        fx.heartbeat("h1", &[&s.owner]).await;
        fx.heartbeat("h2", &[&s.x1, &s.x2]).await;
        fx.hub.tick().await.unwrap();
        left -= step;
    }
}

#[tokio::test]
async fn placement_keeps_the_require_of_the_role_on_a_takeover() {
    let fx = Fx::with(HubConfig {
        roles: roles(),
        ..Default::default()
    });
    let s = setup(&fx).await;
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    let env = testing::envelope(Target::Agent(s.w.agent.clone()), "wake up");
    fx.bus
        .publish(&subjects::agent(&s.w.agent), &env)
        .await
        .unwrap();
    w_expires(&fx, &s, 200).await;
    // `h1` has fewer agents and would win the plain order, so only the
    // require of the role sends the agent to `h2`.
    assert!(next_cmd(&mut h1).await.is_none());
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => assert_eq!(o.agent, s.w.agent),
        other => panic!("expected a launch on h2, got {other:?}"),
    }
}
