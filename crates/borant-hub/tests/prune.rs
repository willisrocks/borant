//! The prune: what it lists, what it keeps, what an apply deletes, and the
//! backup and token that an apply needs.

mod common;

use std::time::Duration;

use borant_core::model::{
    Agent, Channel, ChannelKind, Harness, Lease, LeaseState, Member, PauseReason, Sender, Session,
    SessionEvent, SessionEventBody, SpawnRecord, SpawnState, Target, Task, TaskKind, TaskRef,
    TaskStatus,
};
use borant_core::ports::{MessageBus, SessionStore, StateStore};
use borant_core::proto::{ApiError, ToolCall};
use borant_core::rules::roles::Resolved;
use borant_core::{
    AgentId, ChannelName, Clock, EventId, FencingToken, SessionId, TaskId, Timestamp, keys,
    subjects, testing,
};
use common::Fx;
use serde_json::{Value, json};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const RETENTION: u64 = 30 * 24 * 60 * 60;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// Two days before the manual clock, so it is older than the 24 h default.
fn old(fx: &Fx) -> Timestamp {
    fx.clock.now().minus(DAY * 2)
}

/// A prune call: a dry run, or an apply with `token`.
async fn run(fx: &Fx, apply: bool, token: Option<&str>) -> Result<Value, ApiError> {
    fx.call(
        None,
        ToolCall::PruneAgents {
            older_than_secs: DAY.as_secs(),
            retention_secs: RETENTION,
            apply,
            token: token.map(str::to_owned),
        },
    )
    .await
}

async fn dry(fx: &Fx) -> Value {
    run(fx, false, None).await.unwrap()
}

/// The mark that a snapshot writes, with its own clock.
async fn backup(fx: &Fx, at: Timestamp) {
    let v = json!({ "at": at.0, "path": "/tmp/hub.jsonl", "bytes": 10 });
    put(fx, keys::SNAPSHOT, &v).await;
}

fn candidate<'a>(v: &'a Value, agent: &str) -> Option<&'a Value> {
    v["candidates"]
        .as_array()?
        .iter()
        .find(|c| c["agent"] == json!(agent))
}

fn reason(v: &Value, agent: &str) -> Option<String> {
    v["kept"]
        .as_array()?
        .iter()
        .find(|k| k["agent"] == json!(agent))
        .and_then(|k| k["reason"].as_str())
        .map(str::to_owned)
}

fn kind<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["kinds"]
        .as_array()
        .expect("kinds")
        .iter()
        .find(|k| k["kind"] == json!(name))
        .expect("a kind")
}

fn agent_doc(id: &AgentId, session: Option<SessionId>) -> Agent {
    Agent {
        id: id.clone(),
        spawn_key: None,
        task: None,
        stop_on_close: false,
        continues: None,
        retired: false,
        preferred_harness: Harness::Fake,
        session,
        created: Timestamp(1),
        bundles: Vec::new(),
        ..Default::default()
    }
}

fn lease_doc(id: &AgentId, state: LeaseState) -> Lease {
    Lease {
        agent: id.clone(),
        token: FencingToken(1),
        state,
    }
}

fn expired(at: Timestamp) -> LeaseState {
    LeaseState::Expired {
        since: at,
        last_session: None,
    }
}

fn released(at: Timestamp, parked: bool) -> LeaseState {
    LeaseState::Released { since: at, parked }
}

fn session_doc(id: SessionId, agent: &AgentId) -> Session {
    Session {
        id,
        agent: agent.clone(),
        harness: Harness::Fake,
        harness_session_id: None,
        host: "h1".parse().unwrap(),
        cwd: "/work".into(),
        pid: None,
        started: Timestamp(1),
        ended: None,
    }
}

fn event(agent: &AgentId, at: Timestamp, body: SessionEventBody) -> SessionEvent {
    SessionEvent {
        id: EventId::new(),
        agent: agent.clone(),
        session: None,
        harness: Harness::Fake,
        host: "h1".parse().unwrap(),
        turn: 0,
        at,
        body,
    }
}

fn spawn_record(agent: &AgentId, state: SpawnState, at: Timestamp) -> SpawnRecord {
    SpawnRecord {
        agent: agent.clone(),
        spec: serde_json::from_value(json!({ "channel": "api" })).unwrap(),
        resolved: Resolved::default(),
        by: Sender::Human,
        queued_at: at,
        priority: 0,
        state,
        attempts: 0,
        last_reject: Vec::new(),
        queue_reason: None,
        launch_failures: Vec::new(),
        recoveries: 0,
    }
}

async fn put<T: serde::Serialize>(fx: &Fx, key: &str, v: &T) {
    fx.state
        .create(key, serde_json::to_vec(v).unwrap().into())
        .await
        .unwrap();
}

fn size<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).unwrap().len()
}

/// Writes an agent and the lease that ended at `state`'s `since`.
async fn seed(fx: &Fx, agent: Agent, state: LeaseState) -> AgentId {
    let id = agent.id.clone();
    put(fx, &keys::agent(&id), &agent).await;
    put(fx, &keys::lease_agent(&id), &lease_doc(&id, state)).await;
    id
}

/// Writes an agent whose lease ended, with no session.
async fn released_agent(fx: &Fx, name: &str, since: Timestamp) -> AgentId {
    let id: AgentId = name.parse().unwrap();
    seed(fx, agent_doc(&id, None), expired(since)).await
}

async fn exists(fx: &Fx, key: &str) -> bool {
    fx.state.get(key).await.unwrap().is_some()
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_session() {
    let fx = Fx::new();
    let id: AgentId = "old".parse().unwrap();
    seed(
        &fx,
        agent_doc(&id, Some(SessionId::new())),
        expired(old(&fx)),
    )
    .await;
    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("session"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_recent_lease() {
    let fx = Fx::new();
    let since = fx.clock.now().minus(Duration::from_secs(60 * 60));
    released_agent(&fx, "old", since).await;
    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("lease"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_a_parked_agent() {
    let fx = Fx::new();
    let id: AgentId = "sleepy".parse().unwrap();
    seed(&fx, agent_doc(&id, None), released(old(&fx), true)).await;
    let v = dry(&fx).await;
    assert_eq!(reason(&v, "sleepy").as_deref(), Some("parked"));
    assert!(candidate(&v, "sleepy").is_none());
}

#[tokio::test]
async fn prune_keeps_a_paused_agent() {
    let fx = Fx::new();
    let id: AgentId = "halted".parse().unwrap();
    let mut agent = agent_doc(&id, None);
    agent.paused = Some(PauseReason {
        by: Sender::Human,
        why: "cap: 200 turns today".into(),
        at: Timestamp(1),
    });
    seed(&fx, agent, expired(old(&fx))).await;
    let v = dry(&fx).await;
    assert_eq!(reason(&v, "halted").as_deref(), Some("paused"));
    assert!(candidate(&v, "halted").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_bound_to_an_open_task() {
    let fx = Fx::new();
    let id: AgentId = "bound".parse().unwrap();
    let task = Task {
        id: TaskId::new(),
        title: "work".into(),
        body: String::new(),
        status: TaskStatus::Open,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        issue_target: None,
        properties: Default::default(),
        created: Timestamp(1),
        updated: Timestamp(1),
        create_key: None,
    };
    put(&fx, &keys::task(&api(), &task.id), &task).await;
    let mut agent = agent_doc(&id, None);
    agent.task = Some(TaskRef {
        channel: api(),
        id: task.id,
    });
    seed(&fx, agent, expired(old(&fx))).await;

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "bound").as_deref(), Some("task"));
    assert!(candidate(&v, "bound").is_none());

    // The same agent with a closed task is a candidate again.
    let mut closed = task;
    closed.status = TaskStatus::Closed;
    let key = keys::task(&api(), &closed.id);
    let doc = fx.state.get(&key).await.unwrap().unwrap();
    fx.state
        .update(
            &key,
            serde_json::to_vec(&closed).unwrap().into(),
            doc.revision,
        )
        .await
        .unwrap();
    let v = dry(&fx).await;
    assert!(candidate(&v, "bound").is_some());
}

#[tokio::test]
async fn prune_keeps_a_queued_spawn() {
    let fx = Fx::new();
    let id = released_agent(&fx, "spawned", old(&fx)).await;
    put(
        &fx,
        &keys::spawn(&id),
        &spawn_record(&id, SpawnState::Queued, Timestamp(1)),
    )
    .await;
    let v = dry(&fx).await;
    assert_eq!(reason(&v, "spawned").as_deref(), Some("pending_move"));
    assert!(candidate(&v, "spawned").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_claim() {
    let fx = Fx::new();
    let id = released_agent(&fx, "old", old(&fx)).await;
    let task = Task {
        id: TaskId::new(),
        title: "work".into(),
        body: String::new(),
        status: TaskStatus::InProgress,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: Some(id.clone()),
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        issue_target: None,
        properties: Default::default(),
        created: Timestamp(1),
        updated: Timestamp(1),
        create_key: None,
    };
    put(&fx, &keys::task(&api(), &task.id), &task).await;

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("claims"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_a_channel_owner() {
    let fx = Fx::new();
    let id = released_agent(&fx, "old", old(&fx)).await;
    let channel = Channel {
        name: api(),
        owner: Some(id.clone()),
        schema: json!({}),
        created: Timestamp(1),
        closed: None,
        project: None,
        branch: None,
        collection: None,
        paused: false,
        kind: ChannelKind::Project,
        title: None,
        issue: None,
        gate: None,
        root: None,
    };
    put(&fx, &keys::channel(&api()), &channel).await;

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("owner"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_message_inside_the_retention() {
    let fx = Fx::new();
    let id = released_agent(&fx, "old", old(&fx)).await;
    fx.bus
        .ensure_inbox(&id, &subjects::inbox(&id, &[]))
        .await
        .unwrap();
    let envelope = testing::envelope(Target::Agent(id.clone()), "hello");
    fx.bus
        .publish(&subjects::agent(&id), &envelope)
        .await
        .unwrap();

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("unacked"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_an_undelivered_message_inside_the_retention() {
    let fx = Fx::new();
    let id = released_agent(&fx, "old", old(&fx)).await;
    let envelope = testing::envelope(Target::Agent(id.clone()), "hello");
    fx.sessions
        .append(
            &id,
            &event(
                &id,
                Timestamp(1),
                SessionEventBody::SwarmMessage { envelope },
            ),
        )
        .await
        .unwrap();

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("undelivered"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_a_message_past_the_retention_no_longer_keeps_an_agent() {
    let fx = Fx::new();
    let id = released_agent(&fx, "old", old(&fx)).await;
    fx.bus
        .ensure_inbox(&id, &subjects::inbox(&id, &[]))
        .await
        .unwrap();
    // A message sent 31 days ago is past the 30 day retention of the
    // message streams.
    let mut envelope = testing::envelope(Target::Agent(id.clone()), "hello");
    envelope.sent = fx.clock.now().minus(DAY * 31);
    fx.bus
        .publish(&subjects::agent(&id), &envelope)
        .await
        .unwrap();

    let v = dry(&fx).await;
    assert!(candidate(&v, "old").is_some());
    assert_eq!(kind(&v, "inboxes")["messages"], json!(1));
}

#[tokio::test]
async fn prune_dry_run_reports_each_kind_and_lists_the_keys() {
    let fx = Fx::new();
    let id: AgentId = "old".parse().unwrap();
    let s1 = SessionId::new();
    let s2 = SessionId::new();
    let since = old(&fx);
    let agent = agent_doc(&id, None);
    let lease = lease_doc(&id, expired(since));
    let member = Member {
        agent: id.clone(),
        joined: Timestamp(1),
    };
    let first = session_doc(s1, &id);
    let second = session_doc(s2, &id);
    let spawn = spawn_record(&id, SpawnState::Ended, Timestamp(1));
    put(&fx, &keys::agent(&id), &agent).await;
    put(&fx, &keys::lease_agent(&id), &lease).await;
    put(&fx, &keys::member(&api(), &id), &member).await;
    put(&fx, &keys::session(&s1), &first).await;
    put(&fx, &keys::session(&s2), &second).await;
    put(&fx, &keys::spawn(&id), &spawn).await;
    fx.sessions
        .append(&id, &event(&id, Timestamp(1), SessionEventBody::TurnStart))
        .await
        .unwrap();
    fx.sessions
        .append(&id, &event(&id, Timestamp(2), SessionEventBody::TurnStart))
        .await
        .unwrap();

    let v = dry(&fx).await;
    let c = candidate(&v, "old").expect("a candidate");
    let mut expected = vec![
        keys::agent(&id),
        keys::member(&api(), &id),
        keys::lease_agent(&id),
        keys::session(&s1),
        keys::session(&s2),
        keys::spawn(&id),
    ];
    expected.sort();
    assert_eq!(c["keys"], json!(expected));
    assert_eq!(c["since"], json!(since.0));
    let bytes =
        size(&agent) + size(&lease) + size(&member) + size(&first) + size(&second) + size(&spawn);
    assert_eq!(c["bytes"], json!(bytes));

    assert_eq!(v["mode"], json!("dry-run"));
    assert_eq!(v["applied"], Value::Null);
    assert_eq!(v["token"].as_str().map(str::len), Some(16));
    assert_eq!(kind(&v, "agents")["items"], json!(1));
    assert_eq!(kind(&v, "agents")["keys"], json!(6));
    assert_eq!(kind(&v, "agents")["oldest"], json!(since.0));
    assert_eq!(kind(&v, "agents")["newest"], json!(since.0));
    assert_eq!(kind(&v, "inboxes")["items"], json!(1));
    assert_eq!(kind(&v, "session_events")["items"], json!(1));
    assert_eq!(kind(&v, "session_events")["events"], json!(2));
    assert_eq!(kind(&v, "session_events")["oldest"], json!(1));
    assert_eq!(kind(&v, "session_events")["newest"], json!(2));
    assert_eq!(kind(&v, "leases")["items"], json!(0));
    assert_eq!(kind(&v, "spawns")["items"], json!(0));
    assert_eq!(v["totals"]["keys"], json!(6));
    assert_eq!(v["totals"]["events"], json!(2));
}

#[tokio::test]
async fn prune_dry_run_deletes_nothing() {
    let fx = Fx::new();
    let old_id = released_agent(&fx, "old", old(&fx)).await;
    seed(
        &fx,
        agent_doc(&"live".parse().unwrap(), Some(SessionId::new())),
        expired(old(&fx)),
    )
    .await;
    fx.sessions
        .append(
            &old_id,
            &event(&old_id, Timestamp(1), SessionEventBody::TurnStart),
        )
        .await
        .unwrap();
    let before: Vec<_> = fx
        .state
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.revision, e.value.len()))
        .collect();

    let v = dry(&fx).await;
    assert_eq!(kind(&v, "agents")["items"], json!(1));
    assert_eq!(reason(&v, "live").as_deref(), Some("session"));

    let after: Vec<_> = fx
        .state
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.revision, e.value.len()))
        .collect();
    assert_eq!(before, after);
    assert_eq!(fx.sessions.read(&old_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn prune_apply_needs_the_token_of_the_dry_run() {
    let fx = Fx::new();
    released_agent(&fx, "old", old(&fx)).await;
    backup(&fx, fx.clock.now()).await;

    let err = run(&fx, true, None).await.unwrap_err();
    assert!(err.message.contains("token"), "{}", err.message);
    assert!(exists(&fx, &keys::agent(&"old".parse().unwrap())).await);
}

#[tokio::test]
async fn prune_apply_refuses_a_plan_that_changed() {
    let fx = Fx::new();
    released_agent(&fx, "old", old(&fx)).await;
    backup(&fx, fx.clock.now()).await;
    let token = dry(&fx).await["token"].as_str().unwrap().to_owned();
    released_agent(&fx, "newer", old(&fx)).await;

    let err = run(&fx, true, Some(&token)).await.unwrap_err();
    assert!(err.message.contains("plan changed"), "{}", err.message);
    assert!(exists(&fx, &keys::agent(&"old".parse().unwrap())).await);
}

#[tokio::test]
async fn prune_apply_refuses_without_a_snapshot_on_record() {
    let fx = Fx::new();
    released_agent(&fx, "old", old(&fx)).await;
    let token = dry(&fx).await["token"].as_str().unwrap().to_owned();

    let err = run(&fx, true, Some(&token)).await.unwrap_err();
    assert!(err.message.contains("no snapshot"), "{}", err.message);
    assert!(exists(&fx, &keys::agent(&"old".parse().unwrap())).await);
}

#[tokio::test]
async fn prune_apply_refuses_a_stale_snapshot() {
    let fx = Fx::new();
    released_agent(&fx, "old", old(&fx)).await;
    backup(&fx, fx.clock.now().minus(Duration::from_secs(2 * 60 * 60))).await;
    let token = dry(&fx).await["token"].as_str().unwrap().to_owned();

    let err = run(&fx, true, Some(&token)).await.unwrap_err();
    assert!(err.message.contains("7200s old"), "{}", err.message);
    assert!(exists(&fx, &keys::agent(&"old".parse().unwrap())).await);
}

#[tokio::test]
async fn prune_apply_deletes_only_the_long_released_agents() {
    let fx = Fx::new();
    let old_id = released_agent(&fx, "old", old(&fx)).await;
    let live = seed(
        &fx,
        agent_doc(&"live".parse().unwrap(), Some(SessionId::new())),
        expired(old(&fx)),
    )
    .await;
    let parked = seed(
        &fx,
        agent_doc(&"parked".parse().unwrap(), None),
        released(old(&fx), true),
    )
    .await;
    let queued = released_agent(&fx, "queued", old(&fx)).await;
    put(
        &fx,
        &keys::spawn(&queued),
        &spawn_record(&queued, SpawnState::Queued, Timestamp(1)),
    )
    .await;
    let task = Task {
        id: TaskId::new(),
        title: "work".into(),
        body: String::new(),
        status: TaskStatus::Open,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        issue_target: None,
        properties: Default::default(),
        created: Timestamp(1),
        updated: Timestamp(1),
        create_key: None,
    };
    put(&fx, &keys::task(&api(), &task.id), &task).await;
    let bound_id: AgentId = "bound".parse().unwrap();
    let mut bound_doc = agent_doc(&bound_id, None);
    bound_doc.task = Some(TaskRef {
        channel: api(),
        id: task.id,
    });
    let bounded = seed(&fx, bound_doc, expired(old(&fx))).await;
    // The old agent has a membership and a log, so the apply names them.
    put(
        &fx,
        &keys::member(&api(), &old_id),
        &Member {
            agent: old_id.clone(),
            joined: Timestamp(1),
        },
    )
    .await;
    fx.sessions
        .append(
            &old_id,
            &event(&old_id, Timestamp(1), SessionEventBody::TurnStart),
        )
        .await
        .unwrap();
    backup(&fx, fx.clock.now()).await;

    let v = dry(&fx).await;
    assert_eq!(kind(&v, "agents")["items"], json!(1));
    assert_eq!(reason(&v, "live").as_deref(), Some("session"));
    assert_eq!(reason(&v, "parked").as_deref(), Some("parked"));
    assert_eq!(reason(&v, "queued").as_deref(), Some("pending_move"));
    assert_eq!(reason(&v, "bound").as_deref(), Some("task"));
    let token = v["token"].as_str().unwrap().to_owned();

    let applied = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(applied["mode"], json!("applied"));
    assert_eq!(applied["applied"]["keys"], json!(3));
    assert_eq!(applied["applied"]["inboxes"], json!(1));
    assert_eq!(applied["applied"]["logs"], json!(1));
    assert_eq!(applied["applied"]["events"], json!(1));

    // The agent released two days ago and its keys are gone, with its log.
    assert!(!exists(&fx, &keys::agent(&old_id)).await);
    assert!(!exists(&fx, &keys::lease_agent(&old_id)).await);
    assert!(!exists(&fx, &keys::member(&api(), &old_id)).await);
    assert!(fx.sessions.read(&old_id).await.unwrap().is_empty());
    // Every agent that the rules keep is still there, with its lease.
    for id in [&live, &parked, &queued, &bounded] {
        assert!(exists(&fx, &keys::agent(id)).await, "{id} must stay");
        assert!(
            exists(&fx, &keys::lease_agent(id)).await,
            "{id} keeps a lease"
        );
    }

    // A second apply with the same token finds an empty plan and deletes
    // nothing: the prune is idempotent.
    let again = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(again["applied"]["keys"], json!(0));
    assert_eq!(again["applied"]["batches"], json!(0));
    assert_eq!(again["totals"]["keys"], json!(0));
}

#[tokio::test]
async fn prune_apply_deletes_an_orphan_released_lease_and_keeps_a_held_one() {
    let fx = Fx::new();
    let ghost: AgentId = "ghost".parse().unwrap();
    let held: AgentId = "held".parse().unwrap();
    put(
        &fx,
        &keys::lease_agent(&ghost),
        &lease_doc(&ghost, released(old(&fx), false)),
    )
    .await;
    put(
        &fx,
        &keys::lease_agent(&held),
        &lease_doc(
            &held,
            LeaseState::Held {
                session: SessionId::new(),
                host: "h1".parse().unwrap(),
                expires: fx.clock.now().plus(Duration::from_secs(30)),
            },
        ),
    )
    .await;
    backup(&fx, fx.clock.now()).await;
    let v = dry(&fx).await;
    assert_eq!(kind(&v, "leases")["items"], json!(1));
    let token = v["token"].as_str().unwrap().to_owned();

    let applied = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(applied["applied"]["keys"], json!(1));
    assert!(!exists(&fx, &keys::lease_agent(&ghost)).await);
    assert!(exists(&fx, &keys::lease_agent(&held)).await);
}

#[tokio::test]
async fn prune_apply_deletes_an_ended_spawn_record_of_an_absent_agent() {
    let fx = Fx::new();
    let gone: AgentId = "gone".parse().unwrap();
    let young: AgentId = "young".parse().unwrap();
    put(
        &fx,
        &keys::spawn(&gone),
        &spawn_record(&gone, SpawnState::Ended, old(&fx)),
    )
    .await;
    put(
        &fx,
        &keys::spawn(&young),
        &spawn_record(&young, SpawnState::Ended, fx.clock.now()),
    )
    .await;
    backup(&fx, fx.clock.now()).await;
    let v = dry(&fx).await;
    assert_eq!(kind(&v, "spawns")["items"], json!(1));
    let token = v["token"].as_str().unwrap().to_owned();

    let applied = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(applied["applied"]["keys"], json!(1));
    assert!(!exists(&fx, &keys::spawn(&gone)).await);
    assert!(exists(&fx, &keys::spawn(&young)).await);
}

#[tokio::test]
async fn prune_retention_deletes_the_log_of_an_idle_agent_that_stays() {
    let fx = Fx::new();
    let id = released_agent(&fx, "owner", fx.clock.now().minus(DAY * 40)).await;
    let channel = Channel {
        name: api(),
        owner: Some(id.clone()),
        schema: json!({}),
        created: Timestamp(1),
        closed: None,
        project: None,
        branch: None,
        collection: None,
        paused: false,
        kind: ChannelKind::Project,
        title: None,
        issue: None,
        gate: None,
        root: None,
    };
    put(&fx, &keys::channel(&api()), &channel).await;
    fx.sessions
        .append(&id, &event(&id, Timestamp(1), SessionEventBody::TurnStart))
        .await
        .unwrap();
    backup(&fx, fx.clock.now()).await;

    let v = dry(&fx).await;
    assert_eq!(reason(&v, "owner").as_deref(), Some("owner"));
    assert_eq!(kind(&v, "session_events")["items"], json!(1));
    assert_eq!(kind(&v, "session_events")["events"], json!(1));
    let token = v["token"].as_str().unwrap().to_owned();

    let applied = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(applied["applied"]["logs"], json!(1));
    assert!(fx.sessions.read(&id).await.unwrap().is_empty());
    // The agent keeps its record: only its log was past the retention.
    assert!(exists(&fx, &keys::agent(&id)).await);
}

#[tokio::test]
async fn prune_only_a_person_can_apply() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let reg = fx.register("h1", Some("wrk"), None).await;
    let err = fx
        .call(
            Some(&reg),
            ToolCall::PruneAgents {
                older_than_secs: DAY.as_secs(),
                retention_secs: RETENTION,
                apply: true,
                token: Some("0".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, borant_core::proto::ApiErrorCode::Denied);
}

#[tokio::test]
async fn prune_a_snapshot_records_its_age_for_the_prune() {
    let fx = Fx::new();
    let out = std::env::temp_dir().join(format!(
        "borant-prune-snapshot-{}-{}.jsonl",
        std::process::id(),
        fx.clock.now().0
    ));
    let _ = std::fs::remove_file(&out);
    borant_hub::export::snapshot(&fx.hub, &out).await.unwrap();
    let mark: Value = serde_json::from_slice(
        &fx.state
            .get(keys::SNAPSHOT)
            .await
            .unwrap()
            .expect("a mark")
            .value,
    )
    .unwrap();
    assert_eq!(mark["at"], json!(fx.clock.now().0));
    assert_eq!(
        mark["path"],
        json!(out.display().to_string()),
        "the mark names the file that landed"
    );
    let _ = std::fs::remove_file(&out);

    // That mark is fresh, so an apply over an empty plan passes the check.
    let v = run(&fx, true, None).await.unwrap();
    assert_eq!(v["backup"]["fresh"], json!(true));
    assert_eq!(v["applied"]["keys"], json!(0));
}

#[tokio::test]
async fn prune_batching_stays_under_the_cap() {
    let fx = Fx::new();
    // 205 released agents: two batches of at most 200.
    for i in 0..205 {
        released_agent(&fx, &format!("old{i}"), old(&fx)).await;
    }
    backup(&fx, fx.clock.now()).await;
    let v = dry(&fx).await;
    assert_eq!(kind(&v, "agents")["items"], json!(205));
    let token = v["token"].as_str().unwrap().to_owned();
    let applied = run(&fx, true, Some(&token)).await.unwrap();
    assert_eq!(applied["applied"]["keys"], json!(410));
    // 410 keys need three batches of at most 200; the empty inboxes of the
    // 205 agents add their own.
    assert!(
        applied["applied"]["batches"].as_u64().unwrap() >= 3,
        "410 keys cannot go in fewer than three batches: {}",
        applied["applied"]["batches"]
    );
}
