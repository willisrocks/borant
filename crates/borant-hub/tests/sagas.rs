//! Saga and epic channels in the hub: the gate, the approvers, and the rules
//! that hang tasks under epics (spec: sagas and epics, Task 2).

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{
    ChannelKind, CloseReason, Envelope, Harness, NewTask, Project, Sender, TaskKind, TaskPatch,
    TaskStatus,
};
use borant_core::ports::MessageBus;
use borant_core::proto::{
    ApiError, ApiErrorCode, CtlRequest, RegisterSession, Registered, ToolCall,
};
use borant_core::rules::roles::RoleConfig;
use borant_core::rules::saga::GATE_TITLE;
use borant_core::{SessionId, TaskId, subjects};
use borant_hub::HubConfig;
use common::Fx;
use futures::StreamExt;
use serde_json::Value;

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

/// `boss` orchestrates `work`, `chief` approves it, `other-chief` approves
/// another collection, and `w1` works. The project `web` belongs to `work`.
struct World {
    fx: Fx,
    boss: Registered,
    chief: Registered,
    other_chief: Registered,
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
    let boss = register(&fx, "boss", "orchestrator", "work").await;
    let chief = register(&fx, "chief", "chief", "work").await;
    let other_chief = register(&fx, "other-chief", "chief", "personal").await;
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
        other_chief,
        w1,
    }
}

/// The id of a task record, or of a bare id value.
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
    #[allow(clippy::too_many_arguments)]
    async fn create(
        &self,
        who: &Registered,
        name: &str,
        project: Option<&str>,
        kind: ChannelKind,
        title: Option<&str>,
        issue: Option<&str>,
    ) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(who),
                ToolCall::ChannelCreate {
                    name: name.parse().unwrap(),
                    schema: None,
                    project: project.map(|p| p.parse().unwrap()),
                    branch: None,
                    kind,
                    title: title.map(str::to_owned),
                    issue: issue.map(str::to_owned),
                },
            )
            .await
    }

    /// A saga of `web`, titled "Elastic compute".
    async fn saga(&self) -> Value {
        self.create(
            &self.boss,
            "work-saga",
            Some("web"),
            ChannelKind::Saga,
            Some("Elastic compute"),
            None,
        )
        .await
        .unwrap()
    }

    async fn task(&self, who: &Registered, channel: &str, new: NewTask) -> Result<Value, ApiError> {
        self.fx
            .call(
                Some(who),
                ToolCall::TaskCreate {
                    channel: channel.parse().unwrap(),
                    task: new,
                },
            )
            .await
    }

    async fn get(&self, who: &Registered, channel: &str, id: TaskId) -> Value {
        self.fx
            .call(
                Some(who),
                ToolCall::TaskGet {
                    channel: channel.parse().unwrap(),
                    id,
                },
            )
            .await
            .unwrap()
    }

    async fn epic(&self, channel: &str, title: &str) -> Value {
        let mut new = new_task(title);
        new.kind = TaskKind::Epic;
        self.task(&self.boss, channel, new).await.unwrap()
    }

    async fn child(&self, channel: &str, parent: TaskId, title: &str) -> Value {
        let mut new = new_task(title);
        new.parent = Some(parent);
        self.task(&self.boss, channel, new).await.unwrap()
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

    async fn ask(&self, who: &Registered, channel: &str, id: TaskId, msg: &str) -> Value {
        self.fx
            .call(
                Some(who),
                ToolCall::TaskUpdate {
                    channel: channel.parse().unwrap(),
                    id,
                    patch: TaskPatch {
                        status: Some(TaskStatus::NeedsHuman),
                        attention_msg: Some(msg.into()),
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap()
    }

    async fn ready(&self, who: &Registered, channel: &str) -> Vec<TaskId> {
        let v = self
            .fx
            .call(
                Some(who),
                ToolCall::TaskList {
                    channel: channel.parse().unwrap(),
                    ready_only: true,
                },
            )
            .await
            .unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|t| id_of(&t["id"]))
            .collect()
    }

    async fn approve(&self, who: &Registered, channel: &str, gate: TaskId) {
        self.close(Some(who), channel, gate, CloseReason::Done)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn sagas_create_makes_the_gate() {
    let w = world().await;
    let ch = w.saga().await;
    assert_eq!(ch["kind"], "saga", "{ch}");
    assert_eq!(ch["title"], "Elastic compute");
    assert_eq!(ch["project"], "web");
    assert!(ch["root"].is_null(), "{ch}");
    let gate = id_of(&ch["gate"]);
    let task = w.get(&w.boss, "work-saga", gate).await;
    assert_eq!(task["title"], GATE_TITLE);
    assert_eq!(task["status"], "open");
    assert_eq!(task["kind"], "task");
    assert!(task["parent"].is_null(), "{task}");
}

#[tokio::test]
async fn sagas_saga_and_epic_channels_need_a_project() {
    let w = world().await;
    for kind in [ChannelKind::Saga, ChannelKind::Epic] {
        let err = w
            .create(&w.boss, "work-loose", None, kind, Some("Loose"), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
        assert!(err.message.contains("needs a project"), "{err:?}");
    }
}

#[tokio::test]
async fn sagas_epic_channel_makes_the_root() {
    let w = world().await;
    let ch = w
        .create(
            &w.boss,
            "work-standalone",
            Some("web"),
            ChannelKind::Epic,
            Some("Standalone epic"),
            Some("LIN-7"),
        )
        .await
        .unwrap();
    assert_eq!(ch["kind"], "epic");
    assert!(ch["gate"].is_null(), "{ch}");
    let root = id_of(&ch["root"]);
    let task = w.get(&w.boss, "work-standalone", root).await;
    assert_eq!(task["kind"], "epic");
    assert_eq!(task["title"], "Standalone epic");
    assert_eq!(task["issue"], "LIN-7");
    assert!(task["parent"].is_null(), "{task}");

    // A task without a parent hangs under the root.
    let t = w
        .task(&w.boss, "work-standalone", new_task("do it"))
        .await
        .unwrap();
    assert_eq!(id_of(&t["parent"]), root);

    // A second epic is refused.
    let mut second = new_task("another epic");
    second.kind = TaskKind::Epic;
    let err = w
        .task(&w.boss, "work-standalone", second)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");

    // A channel with no title takes its name.
    let ch = w
        .create(
            &w.boss,
            "work-nameless",
            Some("web"),
            ChannelKind::Epic,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(ch["title"], "work-nameless");
}

#[tokio::test]
async fn sagas_project_channel_refuses_an_epic() {
    let w = world().await;
    w.create(
        &w.boss,
        "work-web",
        Some("web"),
        ChannelKind::Project,
        None,
        None,
    )
    .await
    .unwrap();
    let mut new = new_task("an epic");
    new.kind = TaskKind::Epic;
    let err = w.task(&w.boss, "work-web", new).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
}

#[tokio::test]
async fn sagas_tasks_hang_under_an_epic() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);

    // A task with no parent is refused.
    let err = w
        .task(&w.boss, "work-saga", new_task("loose"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");

    // A task under the gate is refused: the gate is not an epic.
    let mut under_gate = new_task("under the gate");
    under_gate.parent = Some(gate);
    let err = w.task(&w.boss, "work-saga", under_gate).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");

    // An epic with a parent is refused.
    let epic = w.epic("work-saga", "epic one").await;
    let epic_id = id_of(&epic);
    let mut epic_under = new_task("epic two");
    epic_under.kind = TaskKind::Epic;
    epic_under.parent = Some(epic_id);
    let err = w.task(&w.boss, "work-saga", epic_under).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");

    // A task under an epic passes, and so does a grandchild.
    let child = w.child("work-saga", epic_id, "build it").await;
    let child_id = id_of(&child);
    assert_eq!(id_of(&child["parent"]), epic_id);
    let grandchild = w.child("work-saga", child_id, "test it").await;
    assert_eq!(id_of(&grandchild["parent"]), child_id);

    // Moving a task under the gate is refused.
    let err =
        w.fx.call(
            Some(&w.boss),
            ToolCall::TaskUpdate {
                channel: "work-saga".parse().unwrap(),
                id: child_id,
                patch: TaskPatch {
                    parent: Some(gate),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
}

#[tokio::test]
async fn sagas_gate_refuses_claim_and_ready() {
    let w = world().await;
    w.saga().await;
    let epic = w.epic("work-saga", "epic one").await;
    let child = w.child("work-saga", id_of(&epic), "build it").await;

    let err = w
        .claim(&w.w1, "work-saga", id_of(&child))
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::SagaNotApproved, "{err:?}");
    assert_eq!(w.ready(&w.w1, "work-saga").await, Vec::new());
}

#[tokio::test]
async fn sagas_owner_cannot_approve() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);
    let epic = w.epic("work-saga", "epic one").await;
    let child = w.child("work-saga", id_of(&epic), "build it").await;

    for reason in [CloseReason::Done, CloseReason::Wontfix] {
        let err = w
            .close(Some(&w.boss), "work-saga", gate, reason)
            .await
            .unwrap_err();
        assert_eq!(err.code, ApiErrorCode::Denied, "{err:?}");
    }

    // A person approves.
    let closed = w
        .close(None, "work-saga", gate, CloseReason::Done)
        .await
        .unwrap();
    assert_eq!(closed["status"], "closed");
    assert_eq!(w.ready(&w.w1, "work-saga").await, vec![id_of(&child)]);
    let claimed = w.claim(&w.w1, "work-saga", id_of(&child)).await.unwrap();
    assert_eq!(claimed["claimed_by"], "w1");
}

#[tokio::test]
async fn sagas_approver_role_approves() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);
    let epic = w.epic("work-saga", "epic one").await;
    let child = w.child("work-saga", id_of(&epic), "build it").await;

    w.approve(&w.chief, "work-saga", gate).await;
    assert_eq!(w.ready(&w.w1, "work-saga").await, vec![id_of(&child)]);
}

#[tokio::test]
async fn sagas_gate_needs_human_messages_the_approvers() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);

    let mut chief_inbox =
        w.fx.bus
            .subscribe(&subjects::agent(&w.chief.agent))
            .await
            .unwrap();
    let mut other_inbox =
        w.fx.bus
            .subscribe(&subjects::agent(&w.other_chief.agent))
            .await
            .unwrap();

    w.ask(
        &w.boss,
        "work-saga",
        gate,
        "Scope: web indexing. Epics: one.",
    )
    .await;

    let one = tokio::time::timeout(Duration::from_millis(200), chief_inbox.next())
        .await
        .expect("a message to the chief")
        .expect("an envelope");
    let text = one.text.clone();
    assert_eq!(one.from, Sender::Human);
    assert!(text.contains("#work-saga"), "{text}");
    assert!(text.contains(&gate.to_string()), "{text}");
    assert!(text.contains("Elastic compute"), "{text}");
    assert!(text.contains("Scope: web indexing"), "{text}");
    assert!(text.contains("wontfix"), "{text}");

    // An approver of another collection hears nothing.
    let none: Option<Envelope> =
        tokio::time::timeout(Duration::from_millis(200), other_inbox.next())
            .await
            .ok()
            .flatten();
    assert!(none.is_none(), "{none:?}");

    // A new message on a gate that stays `needs_human` sends nothing new.
    w.fx.call(
        Some(&w.boss),
        ToolCall::TaskUpdate {
            channel: "work-saga".parse().unwrap(),
            id: gate,
            patch: TaskPatch {
                attention_msg: Some("Scope: web indexing, revised.".into()),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    let again: Option<Envelope> =
        tokio::time::timeout(Duration::from_millis(200), chief_inbox.next())
            .await
            .ok()
            .flatten();
    assert!(again.is_none(), "{again:?}");
}

#[tokio::test]
async fn sagas_epic_is_not_work() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);
    let epic = w.epic("work-saga", "epic one").await;
    let epic_id = id_of(&epic);
    w.approve(&w.chief, "work-saga", gate).await;

    let err = w.claim(&w.w1, "work-saga", epic_id).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
    let ready = w.ready(&w.w1, "work-saga").await;
    assert!(!ready.contains(&epic_id), "{ready:?}");
}

#[tokio::test]
async fn sagas_blocks_flow_down() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);
    let one = w.epic("work-saga", "epic one").await;
    let one_id = id_of(&one);
    let c1 = w.child("work-saga", one_id, "build one").await;
    let mut two = new_task("epic two");
    two.kind = TaskKind::Epic;
    two.blocked_by = vec![one_id];
    let two = w.task(&w.boss, "work-saga", two).await.unwrap();
    let c2 = w.child("work-saga", id_of(&two), "build two").await;
    let c2_id = id_of(&c2);

    w.approve(&w.chief, "work-saga", gate).await;
    // Epic 2 waits for epic 1, so its child is not ready and cannot be
    // claimed.
    assert!(!w.ready(&w.w1, "work-saga").await.contains(&c2_id));
    assert_eq!(w.ready(&w.w1, "work-saga").await, vec![id_of(&c1)]);
    let err = w.claim(&w.w1, "work-saga", c2_id).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");

    // With epic 1's child and epic 1 closed, the child of epic 2 is ready.
    w.close(Some(&w.boss), "work-saga", id_of(&c1), CloseReason::Done)
        .await
        .unwrap();
    w.close(Some(&w.boss), "work-saga", one_id, CloseReason::Done)
        .await
        .unwrap();
    assert_eq!(w.ready(&w.w1, "work-saga").await, vec![c2_id]);
    w.claim(&w.w1, "work-saga", c2_id).await.unwrap();
}

#[tokio::test]
async fn sagas_late_epic_needs_no_gate() {
    let w = world().await;
    let ch = w.saga().await;
    let gate = id_of(&ch["gate"]);
    w.approve(&w.chief, "work-saga", gate).await;

    let epic = w.epic("work-saga", "epic late").await;
    let child = w.child("work-saga", id_of(&epic), "build late").await;
    assert_eq!(w.ready(&w.w1, "work-saga").await, vec![id_of(&child)]);
    w.claim(&w.w1, "work-saga", id_of(&child)).await.unwrap();
}

#[tokio::test]
async fn sagas_issue_is_stored() {
    let w = world().await;
    let ch = w
        .create(
            &w.boss,
            "work-saga",
            Some("web"),
            ChannelKind::Saga,
            Some("Elastic compute"),
            Some("LIN-42"),
        )
        .await
        .unwrap();
    let gate = id_of(&ch["gate"]);

    let mut new = new_task("epic one");
    new.kind = TaskKind::Epic;
    new.issue = Some("LIN-43".into());
    let epic = w.task(&w.boss, "work-saga", new).await.unwrap();
    let epic_id = id_of(&epic);
    assert_eq!(epic["issue"], "LIN-43");

    let updated =
        w.fx.call(
            Some(&w.boss),
            ToolCall::TaskUpdate {
                channel: "work-saga".parse().unwrap(),
                id: epic_id,
                patch: TaskPatch {
                    issue: Some("LIN-44".into()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(updated["issue"], "LIN-44");
    assert_eq!(
        w.get(&w.boss, "work-saga", epic_id).await["issue"],
        "LIN-44"
    );

    // `channel_list` returns the new channel fields.
    let listed =
        w.fx.call(Some(&w.boss), ToolCall::ChannelList)
            .await
            .unwrap();
    let saga = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "work-saga")
        .expect("the saga");
    assert_eq!(saga["title"], "Elastic compute");
    assert_eq!(saga["issue"], "LIN-42");
    assert_eq!(saga["gate"], gate.to_string());
    assert_eq!(saga["kind"], "saga");
}
