//! Collection guards in the hub (spec section "Collections").

mod common;

use std::collections::BTreeMap;

use borant_core::model::{
    Agent, Channel, ChannelKind, CloseReason, GitAuthor, Harness, HostInfo, NewTask, Project,
    Target, TaskPatch,
};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{ApiError, ApiErrorCode, CtlRequest, HostCommand, Registered, ToolCall};
use borant_core::rules::roles::{CollectionConfig, RoleConfig};
use borant_core::{
    AgentId, ChannelName, CollectionName, ProjectName, RoleName, TaskId, keys, subjects, testing,
};
use borant_hub::HubConfig;
use common::{Fx, next_cmd};
use serde_json::json;

fn col(s: &str) -> CollectionName {
    s.parse().unwrap()
}

fn chan(s: &str) -> ChannelName {
    s.parse().unwrap()
}

fn proj(s: &str) -> ProjectName {
    s.parse().unwrap()
}

async fn create_channel(
    fx: &Fx,
    who: &Registered,
    name: &str,
    project: Option<&str>,
) -> Result<Channel, ApiError> {
    let v = fx
        .call(
            Some(who),
            ToolCall::ChannelCreate {
                name: chan(name),
                schema: None,
                project: project.map(proj),
                branch: None,
                kind: ChannelKind::Project,
                title: None,
                issue: None,
                issue_target: None,
            },
        )
        .await?;
    Ok(serde_json::from_value(v).unwrap())
}

async fn add_project(fx: &Fx, name: &str, collection: Option<&str>) {
    fx.call(
        None,
        ToolCall::ProjectAdd {
            project: Project {
                name: proj(name),
                remote_url: "git://git/demo.git".into(),
                default_branch: "main".into(),
                checkpoint_remote: None,
                setup: None,
                test: None,
                collection: collection.map(col),
                mirror_issues: None,
            },
        },
    )
    .await
    .unwrap();
}

async fn task(fx: &Fx, who: &Registered, channel: &str) -> Result<serde_json::Value, ApiError> {
    fx.call(
        Some(who),
        ToolCall::TaskCreate {
            channel: chan(channel),
            task: NewTask {
                title: "a task".into(),
                ..Default::default()
            },
        },
    )
    .await
}

async fn channel_names(fx: &Fx, who: Option<&Registered>) -> Vec<String> {
    let v = fx.call(who, ToolCall::ChannelList).await.unwrap();
    let mut names: Vec<String> = serde_json::from_value::<Vec<Channel>>(v)
        .unwrap()
        .iter()
        .map(|c| c.name.to_string())
        .collect();
    names.sort();
    names
}

async fn agent_names(fx: &Fx, who: Option<&Registered>) -> Vec<String> {
    let v = fx.call(who, ToolCall::ListAgents).await.unwrap();
    let mut names: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["agent"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

async fn project_names(fx: &Fx, who: Option<&Registered>) -> Vec<String> {
    let v = fx.call(who, ToolCall::ProjectList).await.unwrap();
    let mut names: Vec<String> = serde_json::from_value::<Vec<Project>>(v)
        .unwrap()
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    names.sort();
    names
}

/// Three agents and their channels: one in `work`, one in `personal`, one
/// with no collection at all.
async fn swarm() -> (Fx, Registered, Registered, Registered) {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let w = fx.register_in("h1", Some("wi"), None, Some("work")).await;
    let p = fx
        .register_in("h1", Some("pi"), None, Some("personal"))
        .await;
    let free = fx.register("h1", Some("free"), None).await;
    create_channel(&fx, &w, "work-api", None).await.unwrap();
    create_channel(&fx, &p, "personal-api", None).await.unwrap();
    create_channel(&fx, &free, "open", None).await.unwrap();
    (fx, w, p, free)
}

/// A host that has both bundles of the launch, so placement can choose it.
async fn host_with_bundles(fx: &Fx, n: &str) {
    fx.req(CtlRequest::RegisterHost {
        info: HostInfo {
            name: n.parse().unwrap(),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 4,
            always_on: false,
            draining: false,
            ssh: None,
            tmux_socket: None,
            members: Default::default(),
            pool: None,
            tags: Vec::new(),
            bundles: ["github-work", "lunaroute"]
                .iter()
                .map(|b| b.parse().unwrap())
                .collect(),
            reserve_mem_mb: None,
            sandboxes: Vec::new(),
        },
    })
    .await
    .unwrap();
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

#[tokio::test]
async fn collections_launch_order_gets_the_bundles_and_git_author() {
    let author = GitAuthor {
        name: "Chris Fenton".into(),
        email: "you@work.example".into(),
    };
    let collections = BTreeMap::from([(
        col("work"),
        CollectionConfig {
            bundles: vec!["github-work".parse().unwrap()],
            git_author: Some(author.clone()),
            mirror_issues: false,
        },
    )]);
    let worker: RoleName = "worker".parse().unwrap();
    let roles = BTreeMap::from([(
        worker.clone(),
        RoleConfig {
            bundles: vec!["lunaroute".parse().unwrap()],
            ..Default::default()
        },
    )]);
    let fx = Fx::with(HubConfig {
        collections,
        roles,
        ..Default::default()
    });
    host_with_bundles(&fx, "h1").await;
    host_with_bundles(&fx, "h2").await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: chan("api"),
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
    let w = fx
        .register_in("h1", Some("w"), Some("api"), Some("work"))
        .await;
    set_role(&fx, &w.agent, &worker).await;

    // A message wakes the agent, and the hub places it again once its lease
    // expires. The launch order is built there.
    let mut h1 = fx.commands("h1").await;
    let mut h2 = fx.commands("h2").await;
    let env = testing::envelope(Target::Agent(w.agent.clone()), "wake up");
    fx.bus
        .publish(&subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    for _ in 0..30 {
        fx.advance(10);
        fx.heartbeat("h1", &[&owner]).await;
        fx.hub.tick().await.unwrap();
    }
    let order = tokio::select! {
        Some(HostCommand::Launch(o)) = next_cmd(&mut h1) => o,
        Some(HostCommand::Launch(o)) = next_cmd(&mut h2) => o,
        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
            panic!("no launch happened")
        }
    };
    assert_eq!(order.agent, w.agent);
    assert_eq!(
        order.bundles,
        ["github-work".parse().unwrap(), "lunaroute".parse().unwrap()],
        "the collection bundles come first, then the role's"
    );
    assert_eq!(order.git_author, Some(author));
}

#[tokio::test]
async fn collections_channel_create_needs_the_prefix() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let p = fx
        .register_in("h1", Some("pi"), None, Some("personal"))
        .await;

    let ch = create_channel(&fx, &p, "personal-api", None).await.unwrap();
    assert_eq!(ch.collection, Some(col("personal")));
    assert_eq!(
        ch.name,
        chan("personal-api"),
        "the hub never rewrites a name"
    );

    let err = create_channel(&fx, &p, "api", None).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{}", err.message);

    let err = create_channel(&fx, &p, "work-api", None).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{}", err.message);

    assert_eq!(channel_names(&fx, Some(&p)).await, ["personal-api"]);
}

#[tokio::test]
async fn collections_project_channel_takes_the_project_collection() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let w = fx.register_in("h1", Some("wi"), None, Some("work")).await;
    let p = fx
        .register_in("h1", Some("pi"), None, Some("personal"))
        .await;
    add_project(&fx, "demo", Some("work")).await;

    let err = create_channel(&fx, &p, "personal-demo", Some("demo"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied, "{}", err.message);

    let ch = create_channel(&fx, &w, "work-demo", Some("demo"))
        .await
        .unwrap();
    assert_eq!(ch.collection, Some(col("work")));
    assert_eq!(ch.project, Some(proj("demo")));
    assert_eq!(ch.branch.as_deref(), Some("main"));

    // A project with no collection gives a channel with none, and no prefix.
    add_project(&fx, "plain", None).await;
    let ch = create_channel(&fx, &p, "anything", Some("plain"))
        .await
        .unwrap();
    assert_eq!(ch.collection, None);
}

#[tokio::test]
async fn collections_join_and_registration_refuse_another_collection() {
    let (fx, _w, p, _free) = swarm().await;

    let err = fx
        .call(
            Some(&p),
            ToolCall::ChannelJoin {
                name: chan("work-api"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied, "{}", err.message);

    let err = fx
        .try_register_in("h1", Some("p2"), Some("work-api"), Some("personal"), false)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied, "{}", err.message);

    // The own collection works, and a channel with no collection is open.
    fx.call(
        Some(&p),
        ToolCall::ChannelJoin {
            name: chan("personal-api"),
        },
    )
    .await
    .unwrap();
    fx.call(Some(&p), ToolCall::ChannelJoin { name: chan("open") })
        .await
        .unwrap();
}

#[tokio::test]
async fn collections_task_and_channel_calls_refuse_another_collection() {
    let (fx, _w, p, free) = swarm().await;

    let mut properties = serde_json::Map::new();
    properties.insert("x".into(), json!({ "type": "string" }));
    let calls = [
        ToolCall::Members {
            name: chan("work-api"),
        },
        ToolCall::SchemaGet {
            channel: chan("work-api"),
        },
        ToolCall::SchemaExtend {
            channel: chan("work-api"),
            properties,
        },
        ToolCall::TaskCreate {
            channel: chan("work-api"),
            task: NewTask {
                title: "a task".into(),
                ..Default::default()
            },
        },
        ToolCall::TaskGet {
            channel: chan("work-api"),
            id: TaskId::new(),
        },
        ToolCall::TaskList {
            channel: chan("work-api"),
            ready_only: false,
        },
        ToolCall::TaskUpdate {
            channel: chan("work-api"),
            id: TaskId::new(),
            patch: TaskPatch::default(),
        },
        ToolCall::TaskClaim {
            channel: chan("work-api"),
            id: TaskId::new(),
        },
        ToolCall::TaskClose {
            channel: chan("work-api"),
            id: TaskId::new(),
            reason: CloseReason::Done,
            message: String::new(),
        },
        ToolCall::ChannelTransfer {
            name: chan("work-api"),
            to: free.agent.clone(),
        },
    ];
    for call in calls {
        let what = format!("{call:?}");
        match fx.call(Some(&p), call).await {
            Ok(v) => panic!("{what} was allowed: {v}"),
            Err(e) => assert_eq!(e.code, ApiErrorCode::Denied, "{what}: {}", e.message),
        }
    }

    // The same calls work inside the own collection.
    let v = fx
        .call(
            Some(&p),
            ToolCall::Members {
                name: chan("personal-api"),
            },
        )
        .await
        .unwrap();
    assert!(v.is_array());
    task(&fx, &p, "personal-api").await.unwrap();
}

#[tokio::test]
async fn collections_lists_show_only_the_own_collection() {
    let (fx, _w, p, free) = swarm().await;
    add_project(&fx, "demo", Some("work")).await;
    add_project(&fx, "notes", Some("personal")).await;
    add_project(&fx, "plain", None).await;

    assert_eq!(channel_names(&fx, Some(&p)).await, ["open", "personal-api"]);
    assert_eq!(agent_names(&fx, Some(&p)).await, ["free", "pi"]);
    assert_eq!(project_names(&fx, Some(&p)).await, ["notes", "plain"]);

    // An agent with no collection sees everything, as today.
    assert_eq!(
        channel_names(&fx, Some(&free)).await,
        ["open", "personal-api", "work-api"]
    );
    assert_eq!(agent_names(&fx, Some(&free)).await, ["free", "pi", "wi"]);
    assert_eq!(
        project_names(&fx, Some(&free)).await,
        ["demo", "notes", "plain"]
    );

    // A person sees everything.
    assert_eq!(
        channel_names(&fx, None).await,
        ["open", "personal-api", "work-api"]
    );
    assert_eq!(agent_names(&fx, None).await, ["free", "pi", "wi"]);
    assert_eq!(project_names(&fx, None).await, ["demo", "notes", "plain"]);
}

#[tokio::test]
async fn collections_no_collection_keeps_todays_behavior() {
    let (fx, _w, p, free) = swarm().await;

    // An agent with no collection uses any channel, and creates one without a
    // prefix.
    create_channel(&fx, &free, "anything", None).await.unwrap();
    fx.call(
        Some(&free),
        ToolCall::ChannelJoin {
            name: chan("work-api"),
        },
    )
    .await
    .unwrap();
    task(&fx, &free, "work-api").await.unwrap();
    fx.call(
        Some(&free),
        ToolCall::Members {
            name: chan("personal-api"),
        },
    )
    .await
    .unwrap();

    // A channel with no collection is open to an agent of a collection.
    task(&fx, &p, "open").await.unwrap();
    fx.call(
        Some(&p),
        ToolCall::SchemaGet {
            channel: chan("open"),
        },
    )
    .await
    .unwrap();
}
