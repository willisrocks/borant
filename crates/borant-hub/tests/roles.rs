//! Roles in the hub: spawns, call authorization, one_per, and registration.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{Agent, Harness};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{
    ApiError, ApiErrorCode, CtlRequest, HostCommand, RegisterSession, Registered, ToolCall,
};
use borant_core::rules::roles::{OnePer as OnePerRule, RoleConfig};
use borant_core::{CollectionName, HostName, RoleName, SessionId, keys, subjects};
use borant_hub::HubConfig;
use common::Fx;
use futures::StreamExt;
use serde_json::{Value, json};

fn name(s: &str) -> HostName {
    s.parse().unwrap()
}

fn rname(s: &str) -> RoleName {
    s.parse().unwrap()
}

fn roles() -> BTreeMap<RoleName, RoleConfig> {
    let mut roles = BTreeMap::new();
    roles.insert(
        rname("chief"),
        RoleConfig {
            harness: Some(Harness::ClaudeCode),
            pinned: true,
            one_per: Some(OnePerRule::Collection),
            spawns: vec![rname("worker"), rname("anchor"), rname("orchestrator")],
            skill: Some("borant:chief-of-staff".into()),
            ..Default::default()
        },
    );
    roles.insert(
        rname("worker"),
        RoleConfig {
            harness: Some(Harness::Omp),
            model: Some("lunaroute/deepseek:auto".into()),
            skill: Some("borant:worker".into()),
            tools_deny: vec!["spawn".into()],
            ..Default::default()
        },
    );
    roles.insert(
        rname("anchor"),
        RoleConfig {
            pinned: true,
            ..Default::default()
        },
    );
    roles.insert(rname("outsider"), RoleConfig::default());
    roles.insert(
        rname("orchestrator"),
        RoleConfig {
            one_per: Some(OnePerRule::Project),
            spawns: vec![rname("worker")],
            ..Default::default()
        },
    );
    roles
}

fn fx() -> Fx {
    Fx::with(HubConfig {
        roles: roles(),
        ..Default::default()
    })
}

/// Registers `agent` with a role and a collection.
async fn register(
    fx: &Fx,
    session: SessionId,
    agent: &str,
    role: Option<&str>,
    collection: Option<&str>,
) -> Result<Registered, ApiError> {
    let v = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session,
            host: name("h1"),
            harness: Harness::ClaudeCode,
            harness_session_id: None,
            pid: Some(1),
            cwd: "/work".into(),
            repo: Some(common::repo()),
            agent: Some(agent.parse().unwrap()),
            channel: None,
            shared_dir: false,
            role: role.map(rname),
            collection: collection.map(|c| c.parse().unwrap()),
        }))
        .await?;
    Ok(serde_json::from_value(v).unwrap())
}

/// Registers the chief `boss` and creates `#api`.
async fn chief(fx: &Fx) -> Registered {
    fx.host("h1", &[Harness::ClaudeCode, Harness::Omp]).await;
    let boss = register(
        fx,
        SessionId::new(),
        "boss",
        Some("chief"),
        Some("personal"),
    )
    .await
    .unwrap();
    fx.call(
        Some(&boss),
        ToolCall::ChannelCreate {
            name: "api".parse().unwrap(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    boss
}

async fn spawn(
    fx: &Fx,
    who: &Registered,
    role: Option<&str>,
    name: &str,
) -> Result<Value, ApiError> {
    fx.call(
        Some(who),
        ToolCall::Spawn {
            channel: "api".parse().unwrap(),
            prompt: "do the work".into(),
            harness: None,
            host: None,
            name: Some(name.parse().unwrap()),
            model: None,
            role: role.map(rname),
        },
    )
    .await
}

async fn agent(fx: &Fx, id: &str) -> Agent {
    let e = fx
        .state
        .get(&keys::agent(&id.parse().unwrap()))
        .await
        .unwrap()
        .expect("an agent");
    serde_json::from_slice(&e.value).unwrap()
}

#[tokio::test]
async fn roles_spawn_applies_the_role() {
    let fx = fx();
    let boss = chief(&fx).await;
    let mut cmds = fx
        .bus
        .serve(&subjects::host_cmd(&name("h1")))
        .await
        .unwrap();
    let v = spawn(&fx, &boss, Some("worker"), "kid").await.unwrap();
    assert_eq!(v["agent"]["placed"], json!(true));

    let inc = cmds.next().await.unwrap();
    let HostCommand::Launch(order) = serde_json::from_slice(&inc.payload).unwrap() else {
        panic!("not a launch");
    };
    assert_eq!(order.harness, Harness::Omp);
    assert_eq!(order.model.as_deref(), Some("lunaroute/deepseek:auto"));
    assert_eq!(
        order.prompt,
        "First load the borant:worker skill.\n\ndo the work"
    );

    let kid = agent(&fx, "kid").await;
    assert_eq!(kid.role, Some(rname("worker")));
    assert_eq!(
        kid.collection.as_ref().map(CollectionName::as_str),
        Some("personal")
    );
    assert_eq!(kid.parent, Some("boss".parse().unwrap()));
    assert_eq!(kid.preferred_harness, Harness::Omp);
    assert_eq!(kid.model.as_deref(), Some("lunaroute/deepseek:auto"));
    assert!(!kid.pinned);
}

#[tokio::test]
async fn roles_spawn_sets_the_pin() {
    let fx = fx();
    let boss = chief(&fx).await;
    spawn(&fx, &boss, Some("anchor"), "pin").await.unwrap();
    assert!(agent(&fx, "pin").await.pinned);
    assert_eq!(
        agent(&fx, "pin").await.parent,
        Some("boss".parse().unwrap())
    );
}

#[tokio::test]
async fn roles_spawn_denies_a_role_outside_spawns() {
    let fx = fx();
    let boss = chief(&fx).await;

    // The role exists, but the chief does not list it.
    let err = spawn(&fx, &boss, Some("outsider"), "no").await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied);

    // A roled parent must name a child role.
    let err = spawn(&fx, &boss, None, "no").await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied);

    // An agent without a role may spawn anything.
    let plain = fx.register("h1", Some("plain"), None).await;
    spawn(&fx, &plain, None, "free").await.unwrap();
    assert!(agent(&fx, "free").await.role.is_none());
}

#[tokio::test]
async fn roles_calls_deny_the_tools_of_the_role() {
    let fx = fx();
    let _boss = chief(&fx).await;
    let worker = register(
        &fx,
        SessionId::new(),
        "wrk",
        Some("worker"),
        Some("personal"),
    )
    .await
    .unwrap();

    let err = spawn(&fx, &worker, None, "no").await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied);

    // A tool that the role does not deny still works.
    fx.call(
        Some(&worker),
        ToolCall::TaskList {
            channel: "api".parse().unwrap(),
            ready_only: false,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn roles_one_per_blocks_a_second_spawn() {
    let fx = fx();
    let boss = chief(&fx).await;
    spawn(&fx, &boss, Some("orchestrator"), "o1").await.unwrap();
    let err = spawn(&fx, &boss, Some("orchestrator"), "o2")
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
}

#[tokio::test]
async fn roles_one_per_blocks_a_second_registration() {
    let fx = fx();
    register(&fx, SessionId::new(), "o1", Some("orchestrator"), None)
        .await
        .unwrap();
    let err = register(&fx, SessionId::new(), "o2", Some("orchestrator"), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
}

#[tokio::test]
async fn roles_registration_keeps_the_stored_role() {
    let fx = fx();
    let session = SessionId::new();
    register(&fx, session, "boss", Some("planner"), Some("personal"))
        .await
        .unwrap();

    // Expire the lease so the same session can register again.
    fx.clock.advance(Duration::from_secs(60));
    fx.hub.tick().await.unwrap();

    // The session sends no role: the stored ones stay.
    register(&fx, session, "boss", None, None).await.unwrap();
    let boss = agent(&fx, "boss").await;
    assert_eq!(boss.role, Some(rname("planner")));
    assert_eq!(
        boss.collection.as_ref().map(CollectionName::as_str),
        Some("personal")
    );

    // A change is refused before the lease or a membership is written.
    fx.clock.advance(Duration::from_secs(60));
    fx.hub.tick().await.unwrap();
    let err = register(&fx, session, "boss", Some("worker"), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
    let err = register(&fx, session, "boss", None, Some("work"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict);
    assert_eq!(agent(&fx, "boss").await.role, Some(rname("planner")));
}

#[tokio::test]
async fn roles_registration_sets_the_role_for_a_new_agent() {
    let fx = fx();
    register(
        &fx,
        SessionId::new(),
        "fresh",
        Some("worker"),
        Some("personal"),
    )
    .await
    .unwrap();
    let fresh = agent(&fx, "fresh").await;
    assert_eq!(fresh.role, Some(rname("worker")));
    assert_eq!(
        fresh.collection.as_ref().map(CollectionName::as_str),
        Some("personal")
    );

    // Without a role in the session, the agent has none.
    fx.register("h1", Some("plain"), None).await;
    let plain = agent(&fx, "plain").await;
    assert!(plain.role.is_none());
    assert!(plain.collection.is_none());
}
