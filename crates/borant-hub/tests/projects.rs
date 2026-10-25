mod common;

use borant_core::model::{Agent, Harness, NewTask, Project, Task};
use borant_core::ports::StateStore;
use borant_core::proto::{
    ApiErrorCode, CtlRequest, HostCommand, LaunchOrder, RegisterSession, Registered, ToolCall,
};
use borant_core::{ChannelName, ProjectName, SessionId, keys};
use common::{Fx, name, next_cmd, repo};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn project() -> Project {
    Project {
        name: "web".parse().unwrap(),
        remote_url: "git@example.com:me/web.git".into(),
        default_branch: "main".into(),
        checkpoint_remote: Some("/srv/ckpt.git".into()),
        setup: Some("just bootstrap".into()),
        test: None,
        collection: None,
    }
}

async fn add_project(fx: &Fx) {
    fx.call(None, ToolCall::ProjectAdd { project: project() })
        .await
        .unwrap();
}

/// Two hosts and an owner on h1 that owns #api.
async fn setup(fx: &Fx, project: Option<&str>, branch: Option<&str>) -> Registered {
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: project.map(|p| p.parse().unwrap()),
            branch: branch.map(Into::into),
        },
    )
    .await
    .unwrap();
    owner
}

async fn spawn(fx: &Fx, owner: &Registered, model: Option<&str>) {
    fx.call(
        Some(owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            channel: api(),
            prompt: "Fix the build".into(),
            harness: None,
            host: Some(name("h2")),
            name: Some("helper".parse().unwrap()),
            model: model.map(Into::into),
            role: None,
            bundles: Vec::new(),
        },
    )
    .await
    .unwrap();
}

async fn launch(s: &mut futures::stream::BoxStream<'static, HostCommand>) -> LaunchOrder {
    match next_cmd(s).await {
        Some(HostCommand::Launch(o)) => o,
        other => panic!("expected a launch, got {other:?}"),
    }
}

#[tokio::test]
async fn project_add_list_get() {
    let fx = Fx::new();
    add_project(&fx).await;
    // An add of the same name replaces the project.
    let mut changed = project();
    changed.default_branch = "trunk".into();
    fx.call(
        None,
        ToolCall::ProjectAdd {
            project: changed.clone(),
        },
    )
    .await
    .unwrap();
    let list: Vec<Project> =
        serde_json::from_value(fx.call(None, ToolCall::ProjectList).await.unwrap()).unwrap();
    assert_eq!(list, vec![changed.clone()]);
    let got: Project = serde_json::from_value(
        fx.call(
            None,
            ToolCall::ProjectGet {
                name: "web".parse().unwrap(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(got, changed);
    let err = fx
        .call(
            None,
            ToolCall::ProjectGet {
                name: "nope".parse().unwrap(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::NotFound);
}

#[tokio::test]
async fn channel_create_with_unknown_project_fails() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    let err = fx
        .call(
            Some(&owner),
            ToolCall::ChannelCreate {
                name: api(),
                schema: None,
                project: Some("nope".parse::<ProjectName>().unwrap()),
                branch: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::NotFound, "{err:?}");
    assert!(
        fx.call(None, ToolCall::SchemaGet { channel: api() })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn spawn_in_project_channel_clones_the_project() {
    let fx = Fx::new();
    add_project(&fx).await;
    let owner = setup(&fx, Some("web"), Some("feat/health")).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    spawn(&fx, &owner, None).await;
    let o = launch(&mut h2).await;
    let r = o.repo.expect("a repo");
    assert_eq!(r.remote_url, "git@example.com:me/web.git");
    assert_eq!(r.branch, "feat/health");
    assert_eq!(r.subdir, "");
    assert_eq!(o.project, Some(project()));
    assert_eq!(o.reserve_secs, Some(300));
    // The agent record keeps the repository and the project for moves.
    let doc = fx
        .state
        .get(&keys::agent(&"helper".parse().unwrap()))
        .await
        .unwrap()
        .unwrap();
    let agent: Agent = serde_json::from_slice(&doc.value).unwrap();
    assert_eq!(agent.repo, Some(r));
    assert!(agent.movable);
    assert_eq!(agent.project, Some("web".parse().unwrap()));
}

#[tokio::test]
async fn project_channel_branch_defaults_to_the_default_branch() {
    let fx = Fx::new();
    add_project(&fx).await;
    let owner = setup(&fx, Some("web"), None).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    spawn(&fx, &owner, None).await;
    assert_eq!(launch(&mut h2).await.repo.unwrap().branch, "main");
}

#[tokio::test]
async fn spawn_without_project_keeps_the_caller_repo() {
    let fx = Fx::new();
    let owner = setup(&fx, None, None).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    spawn(&fx, &owner, None).await;
    let o = launch(&mut h2).await;
    assert_eq!(o.repo, Some(repo()));
    assert_eq!(o.project, None);
    assert_eq!(o.model, None);
}

#[tokio::test]
async fn model_survives_a_takeover() {
    let fx = Fx::new();
    let owner = setup(&fx, None, None).await;
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    spawn(&fx, &owner, Some("opus")).await;
    assert_eq!(launch(&mut h2).await.model.as_deref(), Some("opus"));

    // The helper runs on h2 and claims a task, then h2 goes silent.
    let helper = fx.register("h2", Some("helper"), None).await;
    let t: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
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
        Some(&helper),
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await
    .unwrap();
    let mut h1 = fx.commands("h1").await;
    for _ in 0..16 {
        fx.advance(10);
        fx.heartbeat("h1", &[&owner]).await;
        fx.hub.tick().await.unwrap();
    }
    let o = launch(&mut h1).await;
    assert_eq!(o.agent, helper.agent);
    assert_eq!(o.model.as_deref(), Some("opus"));
}

#[tokio::test]
async fn takeover_in_another_harness_drops_the_model() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Pi]).await;
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
    let mut h2 = fx.commands("h2").await;
    fx.heartbeat("h1", &[]).await;
    fx.heartbeat("h2", &[]).await;
    fx.call(
        Some(&owner),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            channel: api(),
            prompt: "Fix the build".into(),
            harness: Some(Harness::Pi),
            host: Some(name("h2")),
            name: Some("helper".parse().unwrap()),
            model: Some("opus".into()),
            role: None,
            bundles: Vec::new(),
        },
    )
    .await
    .unwrap();
    let o = launch(&mut h2).await;
    assert_eq!((o.harness, o.model.as_deref()), (Harness::Pi, Some("opus")));

    // The helper runs in Pi on h2 and claims a task, then h2 goes silent.
    let v = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: name("h2"),
            harness: Harness::Pi,
            pid: Some(2),
            cwd: "/work".into(),
            repo: Some(repo()),
            agent: Some("helper".parse().unwrap()),
            ..Default::default()
        }))
        .await
        .unwrap();
    let helper: Registered = serde_json::from_value(v).unwrap();
    let t: Task = serde_json::from_value(
        fx.call(
            Some(&owner),
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
        Some(&helper),
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await
    .unwrap();
    let mut h1 = fx.commands("h1").await;
    for _ in 0..16 {
        fx.advance(10);
        fx.heartbeat("h1", &[&owner]).await;
        fx.hub.tick().await.unwrap();
    }
    // Only h1 is left, and it has no Pi.
    let o = launch(&mut h1).await;
    assert_eq!(o.agent, helper.agent);
    assert_eq!((o.harness, o.model), (Harness::Fake, None));
}
