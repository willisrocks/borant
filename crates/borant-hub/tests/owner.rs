//! A channel whose owner retired: its spawner takes it, or it has no owner
//! and an orchestrator or chief, or a person, may close its tasks and take it.

mod common;

use borant_core::events::{EventFilter, kind_of};
use borant_core::model::{Channel, CloseReason, Harness, NewTask, Task};
use borant_core::ports::{EventLog, StateStore};
use borant_core::proto::{CtlRequest, RegisterSession, Registered, ToolCall};
use borant_core::{AgentId, ChannelName, SessionId, keys};
use common::{Fx, name};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn lab() -> ChannelName {
    "lab".parse().unwrap()
}

/// Registers a host with room for four agents.
async fn host(fx: &Fx) {
    fx.host("h1", &[Harness::Fake]).await;
}

/// Registers a session with a role, which a plain `Fx::register` cannot name.
async fn register_role(fx: &Fx, agent: &str, role: Option<&str>) -> Registered {
    let v = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: name("h1"),
            harness: Harness::Fake,
            pid: Some(1),
            cwd: "/work".into(),
            repo: Some(common::repo()),
            agent: Some(agent.parse().unwrap()),
            role: role.map(|r| r.parse().unwrap()),
            ..Default::default()
        }))
        .await
        .expect("register the session");
    serde_json::from_value(v).unwrap()
}

async fn create_channel(fx: &Fx, who: &Registered, name: ChannelName) {
    fx.call(
        Some(who),
        ToolCall::ChannelCreate {
            name,
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .expect("create the channel");
}

async fn create_task(fx: &Fx, who: &Registered, channel: ChannelName) -> Task {
    serde_json::from_value(
        fx.call(
            Some(who),
            ToolCall::TaskCreate {
                channel,
                task: NewTask {
                    title: "ship it".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .expect("create the task"),
    )
    .unwrap()
}

/// Spawns `child` from `parent` in #api, then registers the child's session.
async fn spawn_child(fx: &Fx, parent: &Registered, child: &str) -> Registered {
    fx.call(
        Some(parent),
        ToolCall::Spawn {
            channel: api(),
            prompt: "work".into(),
            harness: None,
            host: None,
            name: Some(child.parse().unwrap()),
            model: None,
            role: None,
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            bundles: Vec::new(),
        },
    )
    .await
    .expect("spawn the child");
    fx.register("h1", Some(child), None).await
}

async fn close(
    fx: &Fx,
    who: Option<&Registered>,
    channel: ChannelName,
    id: borant_core::TaskId,
    reason: CloseReason,
) -> Result<Task, borant_core::proto::ApiError> {
    fx.call(
        who,
        ToolCall::TaskClose {
            channel,
            id,
            reason,
            message: "done".into(),
        },
    )
    .await
    .map(|v| serde_json::from_value(v).unwrap())
}

async fn stored_channel(fx: &Fx, name: &ChannelName) -> Channel {
    let entry = fx
        .state
        .get(&keys::channel(name))
        .await
        .unwrap()
        .expect("the channel");
    serde_json::from_slice(&entry.value).unwrap()
}

/// The `channel.owner` events of one channel, as `(previous, owner)`.
async fn owner_events(fx: &Fx, channel: &ChannelName) -> Vec<(AgentId, Option<AgentId>)> {
    let mut stream = fx
        .events
        .read(&EventFilter::default(), false)
        .await
        .expect("read the log");
    let mut out = Vec::new();
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        if kind_of(&event) != "channel.owner" || event.channel.as_ref() != Some(channel) {
            continue;
        }
        match event.body {
            borant_core::events::SwarmEventBody::ChannelOwner { previous, owner } => {
                out.push((previous, owner));
            }
            _ => unreachable!("the kind says a channel owner event"),
        }
    }
    out
}

/// The owner of #lab retires: its spawner, still alive, takes the channel,
/// the owner can close its tasks, and one event says who owns it now.
#[tokio::test]
async fn owner_retired_passes_the_channel_to_the_spawner() {
    let fx = Fx::new();
    host(&fx).await;
    let boss = register_role(&fx, "boss", None).await;
    create_channel(&fx, &boss, api()).await;
    let worker = spawn_child(&fx, &boss, "w1").await;
    create_channel(&fx, &worker, lab()).await;
    let task = create_task(&fx, &worker, lab()).await;

    // The worker owns #lab and stops as a cancelled agent does.
    fx.call(
        Some(&boss),
        ToolCall::Cancel {
            agent: worker.agent.clone(),
            close_task: None,
        },
    )
    .await
    .expect("cancel the owner");

    let ch = stored_channel(&fx, &lab()).await;
    assert_eq!(ch.owner, Some(boss.agent.clone()), "the spawner owns #lab");
    assert_eq!(
        owner_events(&fx, &lab()).await,
        vec![(worker.agent.clone(), Some(boss.agent.clone()))],
        "one event says who owns the channel now"
    );

    // The new owner closes the task as done.
    let closed = close(&fx, Some(&boss), lab(), task.id, CloseReason::Done)
        .await
        .expect("the owner closes the task");
    assert_eq!(closed.status, borant_core::model::TaskStatus::Closed);
}

/// The spawner of the owner is retired too: the channel has no owner, and an
/// orchestrator closes its tasks and takes it with `channel_transfer`.
#[tokio::test]
async fn owner_retired_with_a_retired_spawner_leaves_no_owner() {
    let fx = Fx::new();
    host(&fx).await;
    let boss = register_role(&fx, "boss", None).await;
    create_channel(&fx, &boss, api()).await;
    let worker = spawn_child(&fx, &boss, "w1").await;
    create_channel(&fx, &worker, lab()).await;
    let task = create_task(&fx, &worker, lab()).await;

    // The spawner retires first, so it cannot take the channel later.
    fx.call(
        None,
        ToolCall::Cancel {
            agent: boss.agent.clone(),
            close_task: None,
        },
    )
    .await
    .expect("retire the spawner");
    assert_eq!(stored_channel(&fx, &api()).await.owner, None);
    fx.call(
        None,
        ToolCall::Cancel {
            agent: worker.agent.clone(),
            close_task: None,
        },
    )
    .await
    .expect("retire the owner");

    assert_eq!(stored_channel(&fx, &lab()).await.owner, None);
    assert_eq!(
        owner_events(&fx, &lab()).await,
        vec![(worker.agent.clone(), None)],
        "one event says the channel has no owner"
    );

    // An orchestrator closes the task as done while the channel has no owner.
    let orch = register_role(&fx, "orch", Some("orchestrator")).await;
    let closed = close(&fx, Some(&orch), lab(), task.id, CloseReason::Done)
        .await
        .expect("an orchestrator closes an ownerless task");
    assert_eq!(closed.status, borant_core::model::TaskStatus::Closed);

    // And it takes the channel, once it is a member.
    fx.call(Some(&orch), ToolCall::ChannelJoin { name: lab() })
        .await
        .expect("join #lab");
    fx.call(
        Some(&orch),
        ToolCall::ChannelTransfer {
            name: lab(),
            to: orch.agent.clone(),
        },
    )
    .await
    .expect("take the ownerless channel");
    assert_eq!(stored_channel(&fx, &lab()).await.owner, Some(orch.agent));
}

/// A channel with a live owner keeps today's rules: an orchestrator does not
/// close a task as done, and a person does.
#[tokio::test]
async fn owner_live_keeps_the_close_rule() {
    let fx = Fx::new();
    host(&fx).await;
    let owner = register_role(&fx, "boss", None).await;
    create_channel(&fx, &owner, api()).await;
    let task = create_task(&fx, &owner, api()).await;
    let orch = register_role(&fx, "orch", Some("orchestrator")).await;

    let err = close(&fx, Some(&orch), api(), task.id, CloseReason::Done)
        .await
        .expect_err("an orchestrator may not close a live owner's task");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Denied);

    let closed = close(&fx, None, api(), task.id, CloseReason::Done)
        .await
        .expect("a person closes the task");
    assert_eq!(closed.status, borant_core::model::TaskStatus::Closed);
}
