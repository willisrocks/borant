//! The spawn queue (phase 3 spec, "The persisted queue" and "Idempotency").
//!
//! A spawn writes `spawn.<agent>` before its first placement try, so a hub
//! that restarts still places the agent. The tick takes the queued records by
//! priority and then by age, and a record that does not fit does not block
//! the ones after it.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use borant_core::model::{
    Agent, CloseReason, Harness, HostInfo, NewTask, Sender, SpawnRecord, SpawnState, Task,
    TaskBinding, TaskStatus,
};
use borant_core::ports::StateStore;
use borant_core::proto::{Actor, ApiErrorCode, CtlRequest, HostCommand, Registered, ToolCall};
use borant_core::rules::roles::{OnePer, RoleConfig};
use borant_core::{AgentId, ChannelName, Clock, RoleName, keys};
use borant_hub::{Hub, HubConfig};
use common::{Fx, name, next_cmd};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn rname(s: &str) -> RoleName {
    s.parse().unwrap()
}

/// A host with room for `max_agents`.
async fn host(fx: &Fx, host: &str, max_agents: u32) {
    fx.req(CtlRequest::RegisterHost {
        info: HostInfo {
            name: name(host),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents,
            ..Default::default()
        },
    })
    .await
    .expect("register the host");
}

/// An owner on h1 with a channel `api`.
async fn owner(fx: &Fx) -> Registered {
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
    .expect("create the channel");
    owner
}

fn spawn_call(prompt: &str, priority: i32) -> ToolCall {
    ToolCall::Spawn {
        channel: api(),
        prompt: prompt.to_owned(),
        harness: None,
        host: None,
        name: None,
        model: None,
        role: None,
        key: None,
        task: None,
        stop_on_close: None,
        priority: Some(priority),
    }
}

fn actor(who: &Registered) -> Actor {
    Actor {
        agent: who.agent.clone(),
        session: who.session,
        token: who.token,
    }
}

/// A spawn with an idempotency key in `CtlRequest::Call`, as the CLI sends it.
async fn spawn_with_key(
    fx: &Fx,
    who: &Registered,
    key: &str,
    call: ToolCall,
) -> Result<serde_json::Value, borant_core::proto::ApiError> {
    fx.req(CtlRequest::Call {
        actor: Some(actor(who)),
        key: Some(key.to_owned()),
        call,
    })
    .await
}

async fn record(fx: &Fx, agent: &AgentId) -> Option<SpawnRecord> {
    fx.state
        .get(&keys::spawn(agent))
        .await
        .expect("read the record")
        .and_then(|e| serde_json::from_slice(&e.value).ok())
}

#[tokio::test]
async fn spawns_a_queued_spawn_survives_a_hub_restart_with_its_prompt() {
    let fx = Fx::new();
    // One slot, and the owner holds it.
    host(&fx, "h1", 1).await;
    let owner = owner(&fx).await;
    let spawned = fx
        .call(Some(&owner), spawn_call("write the docs", 0))
        .await
        .expect("spawn");
    assert_eq!(spawned["agent"]["placed"], false);
    let agent: AgentId = spawned["agent"]["agent"].as_str().unwrap().parse().unwrap();
    let queued = record(&fx, &agent).await.expect("a spawn record");
    assert_eq!(queued.state, SpawnState::Queued);
    assert_eq!(queued.spec.prompt, "write the docs");

    // A restarted hub over the same state places the record when a host has
    // room.
    let restarted = Arc::new(Hub::new(
        Arc::new(fx.state.clone()),
        Arc::new(fx.bus.clone()),
        Arc::new(fx.sessions.clone()),
        fx.clock.clone(),
        HubConfig::default(),
    ));
    host(&fx, "h2", 4).await;
    // The hosts heartbeat to the new hub, so it sees them as online.
    for host_name in ["h1", "h2"] {
        restarted
            .handle(CtlRequest::HostHeartbeat {
                host: name(host_name),
                sessions: Vec::new(),
                stats: None,
            })
            .await
            .into_result()
            .expect("heartbeat");
    }
    let mut commands = fx.commands("h2").await;
    restarted.tick().await.expect("tick");
    match next_cmd(&mut commands).await.expect("a launch on h2") {
        HostCommand::Launch(order) => {
            assert_eq!(order.agent, agent);
            assert_eq!(order.prompt, "write the docs", "the prompt survives");
        }
        other => panic!("not a launch: {other:?}"),
    }
    assert_eq!(
        record(&fx, &agent).await.unwrap().state,
        SpawnState::Launching
    );
}

#[tokio::test]
async fn spawns_places_the_highest_priority_first() {
    let fx = Fx::new();
    host(&fx, "h1", 1).await;
    let owner = owner(&fx).await;
    let low = fx
        .call(Some(&owner), spawn_call("low", 0))
        .await
        .expect("spawn");
    let high = fx
        .call(Some(&owner), spawn_call("high", 9))
        .await
        .expect("spawn");
    let low: AgentId = low["agent"]["agent"].as_str().unwrap().parse().unwrap();
    let high: AgentId = high["agent"]["agent"].as_str().unwrap().parse().unwrap();

    // One new slot, two records: the higher priority goes first.
    host(&fx, "h2", 1).await;
    let mut commands = fx.commands("h2").await;
    fx.hub.tick().await.expect("tick");
    match next_cmd(&mut commands).await.expect("a launch") {
        HostCommand::Launch(order) => assert_eq!(order.agent, high),
        other => panic!("not a launch: {other:?}"),
    }
    assert_eq!(
        record(&fx, &high).await.unwrap().state,
        SpawnState::Launching
    );
    assert_eq!(record(&fx, &low).await.unwrap().state, SpawnState::Queued);
}

#[tokio::test]
async fn spawns_a_record_that_does_not_fit_does_not_block_the_next() {
    let fx = Fx::new();
    host(&fx, "h1", 1).await;
    let owner = owner(&fx).await;
    // The first record wants Claude Code, which no host runs.
    let mut first = spawn_call("claude work", 10);
    if let ToolCall::Spawn { harness, .. } = &mut first {
        *harness = Some(Harness::ClaudeCode);
    }
    let blocked = fx.call(Some(&owner), first).await.expect("spawn");
    let next = fx
        .call(Some(&owner), spawn_call("fake work", 0))
        .await
        .expect("spawn");
    let blocked: AgentId = blocked["agent"]["agent"].as_str().unwrap().parse().unwrap();
    let next: AgentId = next["agent"]["agent"].as_str().unwrap().parse().unwrap();

    host(&fx, "h2", 4).await;
    let mut commands = fx.commands("h2").await;
    fx.hub.tick().await.expect("tick");
    match next_cmd(&mut commands).await.expect("a launch") {
        HostCommand::Launch(order) => assert_eq!(order.agent, next),
        other => panic!("not a launch: {other:?}"),
    }
    let blocked = record(&fx, &blocked).await.unwrap();
    assert_eq!(
        blocked.state,
        SpawnState::Queued,
        "the record that does not fit stays queued"
    );
    assert!(
        blocked
            .last_reject
            .iter()
            .any(|(_, why)| why == "no harness"),
        "the record says why h1 refuses it: {:?}",
        blocked.last_reject
    );
    assert!(
        record(&fx, &next).await.unwrap().last_reject.is_empty(),
        "the later record is placed"
    );
}

#[tokio::test]
async fn spawns_the_same_key_returns_the_same_agent() {
    let fx = Fx::new();
    host(&fx, "h1", 4).await;
    let owner = owner(&fx).await;
    let first = spawn_with_key(&fx, &owner, "k1", spawn_call("work", 0))
        .await
        .expect("the first spawn");
    let second = spawn_with_key(&fx, &owner, "k1", spawn_call("work", 0))
        .await
        .expect("the retry");
    assert_eq!(first["agent"], second["agent"]);
    // The name of a keyed spawn is the hash of its key, not a fresh id.
    let agents: usize = fx
        .state
        .list(keys::AGENT_PREFIX)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            serde_json::from_slice::<borant_core::model::Agent>(&e.value)
                .is_ok_and(|a| a.spawn_key.as_deref() == Some("k1"))
        })
        .count();
    assert_eq!(agents, 1, "one agent for one key");
}

#[tokio::test]
async fn spawns_a_different_spec_with_the_same_key_conflicts() {
    let fx = Fx::new();
    host(&fx, "h1", 4).await;
    let owner = owner(&fx).await;
    spawn_with_key(&fx, &owner, "k2", spawn_call("the first work", 0))
        .await
        .expect("the first spawn");
    let err = spawn_with_key(&fx, &owner, "k2", spawn_call("other work", 0))
        .await
        .expect_err("a different spec must conflict");
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err}");
}

#[tokio::test]
async fn spawns_an_idle_key_matches_the_fields_of_the_spec() {
    let fx = Fx::new();
    host(&fx, "h1", 4).await;
    let owner = owner(&fx).await;
    spawn_with_key(&fx, &owner, "k3", spawn_call("work", 0))
        .await
        .expect("the first spawn");
    // The same arguments are the same spawn.
    spawn_with_key(&fx, &owner, "k3", spawn_call("work", 0))
        .await
        .expect("the retry");
    // A new priority is a different spawn.
    let err = spawn_with_key(&fx, &owner, "k3", spawn_call("work", 3))
        .await
        .expect_err("another priority is another spawn");
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err}");
}

#[tokio::test]
async fn spawns_cancel_removes_a_queued_record() {
    let fx = Fx::new();
    host(&fx, "h1", 1).await;
    let owner = owner(&fx).await;
    let spawned = fx
        .call(Some(&owner), spawn_call("never mind", 0))
        .await
        .expect("spawn");
    let agent: AgentId = spawned["agent"]["agent"].as_str().unwrap().parse().unwrap();

    let cancelled = fx
        .call(
            Some(&owner),
            ToolCall::Cancel {
                agent: agent.clone(),
                close_task: None,
            },
        )
        .await
        .expect("cancel");
    assert_eq!(cancelled["cancelled"], true);
    assert!(record(&fx, &agent).await.is_none(), "the record goes");
    assert!(
        fx.state.get(&keys::agent(&agent)).await.unwrap().is_none(),
        "the agent of a queued spawn goes"
    );
    assert!(
        fx.state
            .get(&keys::member(&api(), &agent))
            .await
            .unwrap()
            .is_none(),
        "its membership goes with it"
    );
    // A repeat succeeds.
    fx.call(
        Some(&owner),
        ToolCall::Cancel {
            agent,
            close_task: None,
        },
    )
    .await
    .expect("a repeat cancel");
}

/// A running cancel stops the session like a retire does: the agent is
/// retired, its claims go back to open, and the bound task closes with the
/// reason that the call gives.
#[tokio::test]
async fn spawns_cancel_stops_a_running_agent_and_closes_its_task() {
    let fx = Fx::new();
    host(&fx, "h1", 4).await;
    let owner = owner(&fx).await;
    let bound = new_task(&fx, &owner, "ship it").await;
    let other = new_task(&fx, &owner, "other work").await;

    let mut call = spawn_call("work", 0);
    if let ToolCall::Spawn {
        name,
        task,
        stop_on_close,
        ..
    } = &mut call
    {
        *name = Some("w1".parse().unwrap());
        *task = Some(TaskBinding::Existing {
            channel: api(),
            id: bound.id,
        });
        *stop_on_close = Some(true);
    }
    let spawned = fx.call(Some(&owner), call).await.expect("spawn");
    assert_eq!(spawned["agent"]["placed"], true);
    let worker = fx.register("h1", Some("w1"), Some("api")).await;
    // An idle session stops at once, without waiting for a turn end.
    fx.heartbeat_idle("h1", &[(&worker, Some(fx.clock.now()))])
        .await;
    fx.call(
        Some(&worker),
        ToolCall::TaskClaim {
            channel: api(),
            id: other.id,
        },
    )
    .await
    .expect("claim");
    let mut commands = fx.commands("h1").await;

    let cancelled = fx
        .call(
            Some(&owner),
            ToolCall::Cancel {
                agent: worker.agent.clone(),
                close_task: Some(CloseReason::Done),
            },
        )
        .await
        .expect("cancel");
    assert_eq!(cancelled["cancelled"], true);
    assert_eq!(cancelled["stopped"], true);
    match next_cmd(&mut commands).await.expect("a stop order") {
        HostCommand::Stop { session } => assert_eq!(session, worker.session),
        other => panic!("not a stop: {other:?}"),
    }
    assert!(
        fx.state
            .get(&keys::spawn(&worker.agent))
            .await
            .unwrap()
            .is_none(),
        "the record goes"
    );
    let bound = read_task(&fx, &bound.id).await;
    assert_eq!(bound.status, TaskStatus::Closed);
    assert_eq!(bound.close_reason, Some(CloseReason::Done));
    let other = read_task(&fx, &other.id).await;
    assert_eq!(other.status, TaskStatus::Open);
    assert_eq!(other.claimed_by, None, "the claim goes back to open");
}

async fn new_task(fx: &Fx, owner: &Registered, title: &str) -> Task {
    serde_json::from_value(
        fx.call(
            Some(owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    ..NewTask::default()
                },
            },
        )
        .await
        .expect("create the task"),
    )
    .expect("a task")
}

async fn read_task(fx: &Fx, id: &borant_core::TaskId) -> Task {
    let doc = fx
        .state
        .get(&keys::task(&api(), id))
        .await
        .expect("read the task")
        .expect("the task");
    serde_json::from_slice(&doc.value).expect("a task")
}

fn worker_roles() -> BTreeMap<RoleName, RoleConfig> {
    let mut roles = BTreeMap::new();
    roles.insert(
        rname("chief"),
        RoleConfig {
            harness: Some(Harness::Fake),
            spawns: vec![rname("worker")],
            ..Default::default()
        },
    );
    roles.insert(
        rname("worker"),
        RoleConfig {
            harness: Some(Harness::Fake),
            one_per: Some(OnePer::Collection),
            ..Default::default()
        },
    );
    roles
}

#[tokio::test]
async fn spawns_one_per_counts_a_queued_record() {
    let fx = Fx::with(HubConfig {
        roles: worker_roles(),
        ..Default::default()
    });
    // One slot, and the chief holds it, so its workers wait in the queue.
    host(&fx, "h1", 1).await;
    let chief = fx.register("h1", Some("chief"), None).await;
    fx.call(
        Some(&chief),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .expect("create the channel");

    let mut first = spawn_call("work", 0);
    let mut second = spawn_call("work", 0);
    if let (
        ToolCall::Spawn { role, name, .. },
        ToolCall::Spawn {
            role: r2, name: n2, ..
        },
    ) = (&mut first, &mut second)
    {
        *role = Some(rname("worker"));
        *name = Some("w1".parse().unwrap());
        *r2 = Some(rname("worker"));
        *n2 = Some("w2".parse().unwrap());
    }
    let queued = fx
        .call(Some(&chief), first)
        .await
        .expect("the first worker");
    assert_eq!(queued["agent"]["placed"], false);
    let err = fx
        .call(Some(&chief), second)
        .await
        .expect_err("one worker per collection");
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err}");
    assert!(
        err.message.contains("only one worker per collection"),
        "{err}"
    );
}

#[tokio::test]
async fn spawns_the_queue_lists_the_placement_order() {
    let fx = Fx::new();
    host(&fx, "h1", 1).await;
    let owner = owner(&fx).await;
    let low = fx
        .call(Some(&owner), spawn_call("low", 0))
        .await
        .expect("spawn");
    let high = fx
        .call(Some(&owner), spawn_call("high", 5))
        .await
        .expect("spawn");
    let low: AgentId = low["agent"]["agent"].as_str().unwrap().parse().unwrap();
    let high: AgentId = high["agent"]["agent"].as_str().unwrap().parse().unwrap();

    let queue = fx
        .call(Some(&owner), ToolCall::SpawnQueue)
        .await
        .expect("spawn_queue");
    let rows = queue.as_array().expect("a list");
    assert_eq!(rows.len(), 2, "{queue}");
    assert_eq!(rows[0]["agent"], serde_json::json!(high));
    assert_eq!(rows[1]["agent"], serde_json::json!(low));
    assert_eq!(rows[0]["state"], serde_json::json!("queued"));
    // The fields that `borant ps` renders: the channel and one line of why
    // the one host refuses the record.
    assert_eq!(rows[0]["spec"]["channel"], serde_json::json!("api"));
    let reject = rows[0]["last_reject"].as_str().expect("a reject line");
    assert!(reject.starts_with("h1: full"), "{reject}");
}

async fn read_agent(fx: &Fx, id: &AgentId) -> Agent {
    let doc = fx
        .state
        .get(&keys::agent(id))
        .await
        .expect("read the agent")
        .expect("the agent");
    serde_json::from_slice(&doc.value).expect("an agent")
}

/// A person spawns from a shell with no session: the hub takes the call with no
/// actor, gives the agent the collection of the channel, and queues it.
#[tokio::test]
async fn a_person_spawns_from_the_cli() {
    let fx = Fx::new();
    host(&fx, "h1", 1).await;
    // The creator holds the only slot, and its collection owns the channel.
    let owner = fx
        .register_in("h1", Some("boss"), None, Some("personal"))
        .await;
    let channel: ChannelName = "personal-api".parse().unwrap();
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: channel.clone(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .expect("create the channel");
    let person = |harness: Option<Harness>, key: Option<&str>| CtlRequest::Call {
        actor: None,
        key: key.map(str::to_owned),
        call: ToolCall::Spawn {
            channel: channel.clone(),
            prompt: "from a shell".into(),
            harness,
            host: None,
            name: Some("from-cli".parse().unwrap()),
            model: None,
            role: None,
            key: None,
            task: None,
            stop_on_close: None,
            priority: None,
        },
    };

    // Without a role, a person's spawn must name its harness.
    let err = fx
        .req(person(None, None))
        .await
        .expect_err("a person needs a harness");
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err}");
    assert!(err.message.contains("needs a harness"), "{err}");

    let spawned = fx
        .req(person(Some(Harness::Fake), Some("cli-1")))
        .await
        .expect("a person spawns");
    assert_eq!(spawned["agent"]["placed"], false, "{spawned}");
    let agent = read_agent(&fx, &"from-cli".parse().unwrap()).await;
    assert_eq!(
        agent.collection.as_ref().map(|c| c.as_str()),
        Some("personal"),
        "a person's spawn takes the collection of its channel"
    );
    assert!(agent.parent.is_none(), "a person's spawn has no parent");
    assert_eq!(
        record(&fx, &agent.id).await.unwrap().by,
        Sender::Human,
        "the record names the person"
    );
    // And the key still makes a retry the same spawn.
    let again = fx
        .req(person(Some(Harness::Fake), Some("cli-1")))
        .await
        .expect("the retry");
    assert_eq!(again["agent"]["agent"], serde_json::json!(agent.id));
}
