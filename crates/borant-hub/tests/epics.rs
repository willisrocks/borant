//! Closing channels, the rules of a closed channel and of a spawn with an
//! epic branch (spec: sagas and epics, Task 3).

mod common;

use borant_core::model::{
    Agent, ChannelKind, CloseReason, Harness, NewTask, Project, TaskBinding, TaskKind, TaskPatch,
};
use borant_core::ports::StateStore;
use borant_core::proto::{
    ApiError, ApiErrorCode, CtlRequest, RegisterSession, Registered, ToolCall,
};
use borant_core::rules::roles::RoleConfig;
use borant_core::{SessionId, TaskId, keys};
use borant_hub::HubConfig;
use common::Fx;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn roles() -> BTreeMap<borant_core::RoleName, RoleConfig> {
    let mut roles = BTreeMap::new();
    roles.insert(
        "chief".parse().unwrap(),
        RoleConfig {
            approves: true,
            ..Default::default()
        },
    );
    roles.insert(
        "orchestrator".parse().unwrap(),
        RoleConfig {
            spawns: vec!["worker".parse().unwrap()],
            ..Default::default()
        },
    );
    roles.insert("worker".parse().unwrap(), RoleConfig::default());
    roles
}

fn fx() -> Fx {
    Fx::with(HubConfig {
        roles: roles(),
        ..Default::default()
    })
}

struct World {
    fx: Fx,
    boss: Registered,
    chief: Registered,
    w1: Registered,
}

async fn register(fx: &Fx, agent: &str, role: &str, collection: &str) -> Registered {
    let v = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: common::name("h1"),
            harness: Harness::Fake,
            harness_session_id: None,
            pid: Some(1),
            cwd: "/work".into(),
            repo: Some(common::repo()),
            agent: Some(agent.parse().unwrap()),
            channel: None,
            shared_dir: false,
            role: Some(role.parse().unwrap()),
            collection: Some(collection.parse().unwrap()),
        }))
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn world() -> World {
    let fx = fx();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let boss = register(&fx, "boss", "orchestrator", "work").await;
    let chief = register(&fx, "chief", "chief", "work").await;
    let w1 = register(&fx, "w1", "worker", "work").await;
    fx.call(
        Some(&boss),
        ToolCall::ProjectAdd {
            project: Project {
                name: "web".parse().unwrap(),
                remote_url: "git://git/web.git".into(),
                default_branch: "main".into(),
                checkpoint_remote: None,
                setup: None,
                test: None,
                collection: Some("work".parse().unwrap()),
                mirror_issues: None,
            },
        },
    )
    .await
    .unwrap();
    World {
        fx,
        boss,
        chief,
        w1,
    }
}

fn id_of(v: &Value) -> TaskId {
    let raw = if v.is_string() { v } else { &v["id"] };
    raw.as_str().expect("an id").parse().unwrap()
}

fn new_task(title: &str) -> NewTask {
    NewTask {
        title: title.into(),
        body: String::new(),
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        issue_target: None,
        properties: Default::default(),
        deadline: None,
        max_attempts: None,
    }
}

impl World {
    async fn create(
        &self,
        name: &str,
        project: Option<&str>,
        kind: ChannelKind,
        title: Option<&str>,
    ) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(&self.boss),
                ToolCall::ChannelCreate {
                    name: name.parse().unwrap(),
                    schema: None,
                    project: project.map(|p| p.parse().unwrap()),
                    branch: None,
                    kind,
                    title: title.map(str::to_owned),
                    issue: None,
                    issue_target: None,
                },
            )
            .await
    }

    async fn channel(&self, name: &str) -> Value {
        let all = self
            .fx
            .call(Some(&self.boss), ToolCall::ChannelList)
            .await
            .unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
            .expect("the channel")
    }

    async fn task(&self, channel: &str, new: NewTask) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(&self.boss),
                ToolCall::TaskCreate {
                    channel: channel.parse().unwrap(),
                    task: new,
                },
            )
            .await
    }

    async fn epic(&self, channel: &str, title: &str) -> Value {
        let mut new = new_task(title);
        new.kind = TaskKind::Epic;
        self.task(channel, new).await.unwrap()
    }

    async fn child(&self, channel: &str, parent: TaskId, title: &str) -> Value {
        let mut new = new_task(title);
        new.parent = Some(parent);
        self.task(channel, new).await.unwrap()
    }

    async fn close(
        &self,
        who: Option<&Registered>,
        channel: &str,
        id: TaskId,
        reason: CloseReason,
    ) -> Result<Value, ApiError> {
        self.fx
            .call(
                who,
                ToolCall::TaskClose {
                    channel: channel.parse().unwrap(),
                    id,
                    reason,
                    message: String::new(),
                },
            )
            .await
    }

    async fn claim(&self, who: &Registered, channel: &str, id: TaskId) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(who),
                ToolCall::TaskClaim {
                    channel: channel.parse().unwrap(),
                    id,
                },
            )
            .await
    }

    async fn close_channel(
        &self,
        who: Option<&Registered>,
        channel: &str,
    ) -> Result<Value, ApiError> {
        self.fx
            .call(
                who,
                ToolCall::ChannelClose {
                    name: channel.parse().unwrap(),
                },
            )
            .await
    }

    async fn spawn(
        &self,
        who: &Registered,
        channel: &str,
        task: Option<TaskBinding>,
        name: &str,
    ) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(who),
                ToolCall::Spawn {
                    channel: channel.parse().unwrap(),
                    prompt: "do the work".into(),
                    harness: None,
                    host: None,
                    name: Some(name.parse().unwrap()),
                    model: None,
                    role: Some("worker".parse().unwrap()),
                    key: None,
                    task,
                    stop_on_close: None,
                    continues: None,
                    priority: None,
                    bundles: Vec::new(),
                },
            )
            .await
    }

    async fn approve(&self, channel: &str, gate: TaskId) {
        self.close(Some(&self.chief), channel, gate, CloseReason::Done)
            .await
            .unwrap();
    }

    async fn agent(&self, name: &str) -> Agent {
        let doc = self
            .fx
            .state
            .get(&keys::agent(&name.parse().unwrap()))
            .await
            .unwrap()
            .expect("the agent record");
        serde_json::from_slice(&doc.value).unwrap()
    }

    async fn queue_empty(&self) -> bool {
        self.fx
            .call(None, ToolCall::SpawnQueue)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    }
}

/// A saga of `web` with its gate.
async fn saga(w: &World) -> (TaskId, Value) {
    let ch = w
        .create("work-saga", Some("web"), ChannelKind::Saga, Some("Elastic"))
        .await
        .unwrap();
    (id_of(&ch["gate"]), ch)
}

#[tokio::test]
async fn epics_channel_close_rules() {
    let w = world().await;
    w.create("work-api", Some("web"), ChannelKind::Project, Some("API"))
        .await
        .unwrap();
    let t1 = id_of(&w.task("work-api", new_task("one")).await.unwrap());

    // A member that is not the owner may not close it.
    let err = w.close_channel(Some(&w.w1), "work-api").await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied, "{err:?}");

    // An open task holds the channel open, and the error names it.
    let err = w
        .close_channel(Some(&w.boss), "work-api")
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(err.message.contains(&t1.to_string()), "{err:?}");

    w.close(Some(&w.boss), "work-api", t1, CloseReason::Done)
        .await
        .unwrap();
    let closed = w.close_channel(Some(&w.boss), "work-api").await.unwrap();
    assert!(!closed["closed"].is_null(), "{closed}");
    assert_eq!(w.channel("work-api").await["closed"], closed["closed"]);

    // A retry returns the channel unchanged.
    let again = w.close_channel(Some(&w.boss), "work-api").await.unwrap();
    assert_eq!(again["closed"], closed["closed"]);

    // A person closes a channel too, under the same open-task rule.
    w.create("work-two", Some("web"), ChannelKind::Project, None)
        .await
        .unwrap();
    let t2 = id_of(&w.task("work-two", new_task("two")).await.unwrap());
    let err = w.close_channel(None, "work-two").await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    w.close(Some(&w.boss), "work-two", t2, CloseReason::Done)
        .await
        .unwrap();
    let by_person = w.close_channel(None, "work-two").await.unwrap();
    assert!(!by_person["closed"].is_null(), "{by_person}");
}

#[tokio::test]
async fn epics_closed_channel_refuses_work() {
    let w = world().await;
    w.create("work-api", Some("web"), ChannelKind::Project, None)
        .await
        .unwrap();
    let t1 = id_of(&w.task("work-api", new_task("one")).await.unwrap());
    w.close(Some(&w.boss), "work-api", t1, CloseReason::Done)
        .await
        .unwrap();
    w.close_channel(Some(&w.boss), "work-api").await.unwrap();

    let err = w.task("work-api", new_task("late")).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(err.message.contains("work-api"), "{err:?}");

    let err = w.claim(&w.w1, "work-api", t1).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");

    let err = w
        .spawn(&w.boss, "work-api", None, "late1")
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");

    assert!(w.queue_empty().await, "a refused spawn queues nothing");
}

#[tokio::test]
async fn epics_root_close_closes_the_channel() {
    let w = world().await;
    let ch = w
        .create("work-epic", Some("web"), ChannelKind::Epic, Some("Root"))
        .await
        .unwrap();
    let root = id_of(&ch["root"]);
    let child = id_of(&w.child("work-epic", root, "one").await);

    w.close(Some(&w.boss), "work-epic", child, CloseReason::Done)
        .await
        .unwrap();
    assert!(w.channel("work-epic").await["closed"].is_null());

    w.close(Some(&w.boss), "work-epic", root, CloseReason::Done)
        .await
        .unwrap();
    assert!(
        !w.channel("work-epic").await["closed"].is_null(),
        "the root closes its channel"
    );
}

#[tokio::test]
async fn epics_spawn_refuses_an_unapproved_saga() {
    let w = world().await;
    let (_gate, _ch) = saga(&w).await;
    let e1 = id_of(&w.epic("work-saga", "e1").await);
    let c1 = id_of(&w.child("work-saga", e1, "c1").await);

    for task in [
        None,
        Some(TaskBinding::Existing {
            channel: "work-saga".parse().unwrap(),
            id: c1,
        }),
        Some(TaskBinding::New {
            channel: "work-saga".parse().unwrap(),
            title: "new work".into(),
        }),
    ] {
        let err = w
            .spawn(&w.boss, "work-saga", task, "early1")
            .await
            .unwrap_err();
        assert_eq!(err.code, ApiErrorCode::SagaNotApproved, "{err:?}");
        assert!(w.queue_empty().await, "a refused spawn queues nothing");
    }
}

#[tokio::test]
async fn epics_spawn_refuses_an_epic_binding() {
    let w = world().await;
    let (gate, _ch) = saga(&w).await;
    let e1 = id_of(&w.epic("work-saga", "e1").await);
    w.approve("work-saga", gate).await;

    let err = w
        .spawn(
            &w.boss,
            "work-saga",
            Some(TaskBinding::Existing {
                channel: "work-saga".parse().unwrap(),
                id: e1,
            }),
            "epic1",
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
    assert!(w.queue_empty().await);
}

#[tokio::test]
async fn epics_spawn_refuses_a_blocked_task() {
    let w = world().await;
    let (gate, _ch) = saga(&w).await;
    let e1 = id_of(&w.epic("work-saga", "e1").await);
    let mut new = new_task("e2");
    new.kind = TaskKind::Epic;
    new.blocked_by = vec![e1];
    let e2 = id_of(&w.task("work-saga", new).await.unwrap());
    let c2 = id_of(&w.child("work-saga", e2, "c2").await);
    w.approve("work-saga", gate).await;

    let err = w
        .spawn(
            &w.boss,
            "work-saga",
            Some(TaskBinding::Existing {
                channel: "work-saga".parse().unwrap(),
                id: c2,
            }),
            "blocked1",
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(w.queue_empty().await);
}

#[tokio::test]
async fn epics_spawn_starts_on_the_epic_branch() {
    let w = world().await;
    let (gate, _ch) = saga(&w).await;
    let e1 = id_of(&w.epic("work-saga", "e1").await);
    let c1 = id_of(&w.child("work-saga", e1, "c1").await);
    w.fx.call(
        Some(&w.boss),
        ToolCall::TaskUpdate {
            channel: "work-saga".parse().unwrap(),
            id: e1,
            patch: TaskPatch {
                properties: Some(json!({"branch": "epic/one"}).as_object().unwrap().clone()),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    w.approve("work-saga", gate).await;

    w.spawn(
        &w.boss,
        "work-saga",
        Some(TaskBinding::Existing {
            channel: "work-saga".parse().unwrap(),
            id: c1,
        }),
        "stacked1",
    )
    .await
    .unwrap();
    assert_eq!(
        w.agent("stacked1").await.repo.unwrap().branch,
        "epic/one",
        "rule 7: a bound spawn starts on the branch of its epic"
    );

    // An epic with no branch keeps the channel branch.
    let e2 = id_of(&w.epic("work-saga", "e2").await);
    let c2 = id_of(&w.child("work-saga", e2, "c2").await);
    w.spawn(
        &w.boss,
        "work-saga",
        Some(TaskBinding::Existing {
            channel: "work-saga".parse().unwrap(),
            id: c2,
        }),
        "plain1",
    )
    .await
    .unwrap();
    assert_eq!(w.agent("plain1").await.repo.unwrap().branch, "main");

    // A plain task in a project channel keeps it too.
    w.create("work-api", Some("web"), ChannelKind::Project, None)
        .await
        .unwrap();
    let t3 = id_of(&w.task("work-api", new_task("three")).await.unwrap());
    w.spawn(
        &w.boss,
        "work-api",
        Some(TaskBinding::Existing {
            channel: "work-api".parse().unwrap(),
            id: t3,
        }),
        "plain2",
    )
    .await
    .unwrap();
    assert_eq!(w.agent("plain2").await.repo.unwrap().branch, "main");
}
