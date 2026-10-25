//! A channel whose owner retired: its spawner takes it, or it has no owner and
//! only a person may close its tasks and take it.

mod common;

use borant_core::events::{EventFilter, kind_of};
use borant_core::model::{Channel, CloseReason, Harness, NewTask, Task};
use borant_core::ports::{EventLog, StateStore};
use borant_core::proto::{Registered, ToolCall};
use borant_core::{AgentId, ChannelName, keys};
use common::Fx;

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
    let boss = fx.register("h1", Some("boss"), None).await;
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

/// The spawner of the owner is retired too: the channel has no owner, so an
/// agent may not close its tasks or take it, and a person may.
#[tokio::test]
async fn owner_retired_with_a_retired_spawner_leaves_no_owner() {
    let fx = Fx::new();
    host(&fx).await;
    let boss = fx.register("h1", Some("boss"), None).await;
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

    // An agent may not close an ownerless task, whatever its role: only a
    // person may.
    let w2 = fx.register("h1", Some("w2"), None).await;
    let err = close(&fx, Some(&w2), lab(), task.id, CloseReason::Done)
        .await
        .expect_err("an agent may not close an ownerless task");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Denied);
    let err = fx
        .call(
            Some(&w2),
            ToolCall::ChannelTransfer {
                name: lab(),
                to: w2.agent.clone(),
            },
        )
        .await
        .expect_err("an agent may not take an ownerless channel");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Invalid);

    let closed = close(&fx, None, lab(), task.id, CloseReason::Done)
        .await
        .expect("a person closes an ownerless task");
    assert_eq!(closed.status, borant_core::model::TaskStatus::Closed);

    // A person also takes the ownerless channel, once the new owner is a
    // member.
    fx.call(Some(&w2), ToolCall::ChannelJoin { name: lab() })
        .await
        .expect("join #lab");
    fx.call(
        None,
        ToolCall::ChannelTransfer {
            name: lab(),
            to: w2.agent.clone(),
        },
    )
    .await
    .expect("a person takes the ownerless channel");
    assert_eq!(stored_channel(&fx, &lab()).await.owner, Some(w2.agent));
}

/// A channel with a live owner keeps today's rules: an agent does not close a
/// task as done, and a person does.
#[tokio::test]
async fn owner_live_keeps_the_close_rule() {
    let fx = Fx::new();
    host(&fx).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    create_channel(&fx, &owner, api()).await;
    let task = create_task(&fx, &owner, api()).await;
    let w2 = fx.register("h1", Some("w2"), None).await;

    let err = close(&fx, Some(&w2), api(), task.id, CloseReason::Done)
        .await
        .expect_err("an agent may not close a live owner's task");
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Denied);

    let closed = close(&fx, None, api(), task.id, CloseReason::Done)
        .await
        .expect("a person closes the task");
    assert_eq!(closed.status, borant_core::model::TaskStatus::Closed);
}
