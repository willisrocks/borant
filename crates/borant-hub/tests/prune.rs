//! The read-only prune: which stale agents a prune removes, why every other
//! agent stays, and that the request changes nothing.

mod common;

use std::time::Duration;

use borant_core::model::{
    Agent, Channel, Harness, Lease, LeaseState, Member, Session, SessionEvent, SessionEventBody,
    Target, Task, TaskStatus,
};
use borant_core::ports::{MessageBus, SessionStore, StateStore};
use borant_core::proto::ToolCall;
use borant_core::{
    AgentId, ChannelName, Clock, EventId, FencingToken, SessionId, TaskId, Timestamp, keys,
    subjects, testing,
};
use common::Fx;
use serde_json::{Value, json};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// Two days before the manual clock, so it is older than the 24 h default.
fn old(fx: &Fx) -> Timestamp {
    fx.clock.now().minus(DAY * 2)
}

/// The reply of a dry run with the default age.
async fn prune(fx: &Fx) -> Value {
    fx.call(
        None,
        ToolCall::PruneAgents {
            older_than_secs: 24 * 60 * 60,
        },
    )
    .await
    .unwrap()
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

async fn put<T: serde::Serialize>(fx: &Fx, key: &str, v: &T) {
    fx.state
        .create(key, serde_json::to_vec(v).unwrap().into())
        .await
        .unwrap();
}

fn size<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).unwrap().len()
}

/// Writes an agent whose lease ended at `since`, with no session.
async fn seed(fx: &Fx, name: &str, session: Option<SessionId>, since: Timestamp) -> AgentId {
    let id: AgentId = name.parse().unwrap();
    let agent = agent_doc(&id, session);
    let lease = lease_doc(
        &id,
        LeaseState::Expired {
            since,
            last_session: None,
        },
    );
    put(fx, &keys::agent(&id), &agent).await;
    put(fx, &keys::lease_agent(&id), &lease).await;
    id
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_session() {
    let fx = Fx::new();
    seed(&fx, "old", Some(SessionId::new()), old(&fx)).await;
    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("session"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_recent_lease() {
    let fx = Fx::new();
    let since = fx.clock.now().minus(Duration::from_secs(60 * 60));
    seed(&fx, "old", None, since).await;
    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("lease"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_pending_move() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let parent = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&parent),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    // No host can take the agent, so its spawn stays queued.
    fx.call(
        Some(&parent),
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            channel: api(),
            prompt: "go".into(),
            harness: Some(Harness::Fake),
            host: Some("nowhere".parse().unwrap()),
            name: Some("spawned".parse().unwrap()),
            model: None,
            role: None,
            bundles: Vec::new(),
        },
    )
    .await
    .unwrap();
    // The spawn leaves a fresh expiry; age it so that only the queued record
    // keeps the agent.
    let key = keys::lease_agent(&"spawned".parse().unwrap());
    let doc = fx.state.get(&key).await.unwrap().unwrap();
    let lease = lease_doc(
        &"spawned".parse().unwrap(),
        LeaseState::Expired {
            since: old(&fx),
            last_session: None,
        },
    );
    fx.state
        .update(
            &key,
            serde_json::to_vec(&lease).unwrap().into(),
            doc.revision,
        )
        .await
        .unwrap();

    let v = prune(&fx).await;
    assert_eq!(reason(&v, "spawned").as_deref(), Some("pending_move"));
    assert!(candidate(&v, "spawned").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_a_claim() {
    let fx = Fx::new();
    let id = seed(&fx, "old", None, old(&fx)).await;
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
        properties: Default::default(),
        created: Timestamp(1),
        updated: Timestamp(1),
        create_key: None,
    };
    put(&fx, &keys::task(&api(), &task.id), &task).await;

    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("claims"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_a_channel_owner() {
    let fx = Fx::new();
    let id = seed(&fx, "old", None, old(&fx)).await;
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
    };
    put(&fx, &keys::channel(&api()), &channel).await;

    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("owner"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_an_unacked_message() {
    let fx = Fx::new();
    let id = seed(&fx, "old", None, old(&fx)).await;
    fx.bus
        .ensure_inbox(&id, &subjects::inbox(&id, &[]))
        .await
        .unwrap();
    let envelope = testing::envelope(Target::Agent(id.clone()), "hello");
    fx.bus
        .publish(&subjects::agent(&id), &envelope)
        .await
        .unwrap();

    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("unacked"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_keeps_an_agent_with_an_undelivered_message() {
    let fx = Fx::new();
    let id = seed(&fx, "old", None, old(&fx)).await;
    let envelope = testing::envelope(Target::Agent(id.clone()), "hello");
    fx.sessions
        .append(
            &id,
            &SessionEvent {
                id: EventId::new(),
                agent: id.clone(),
                session: None,
                harness: Harness::Fake,
                host: "h1".parse().unwrap(),
                turn: 0,
                at: Timestamp(1),
                body: SessionEventBody::SwarmMessage { envelope },
            },
        )
        .await
        .unwrap();

    let v = prune(&fx).await;
    assert_eq!(reason(&v, "old").as_deref(), Some("undelivered"));
    assert!(candidate(&v, "old").is_none());
}

#[tokio::test]
async fn prune_candidate_lists_its_keys() {
    let fx = Fx::new();
    let id: AgentId = "old".parse().unwrap();
    let s1 = SessionId::new();
    let s2 = SessionId::new();
    let since = old(&fx);
    let agent = agent_doc(&id, None);
    let lease = lease_doc(
        &id,
        LeaseState::Expired {
            since,
            last_session: None,
        },
    );
    let member = Member {
        agent: id.clone(),
        joined: Timestamp(1),
    };
    let first = session_doc(s1, &id);
    let second = session_doc(s2, &id);
    put(&fx, &keys::agent(&id), &agent).await;
    put(&fx, &keys::lease_agent(&id), &lease).await;
    put(&fx, &keys::member(&api(), &id), &member).await;
    put(&fx, &keys::session(&s1), &first).await;
    put(&fx, &keys::session(&s2), &second).await;

    let v = prune(&fx).await;
    let c = candidate(&v, "old").expect("a candidate");
    let mut expected = vec![
        keys::agent(&id),
        keys::member(&api(), &id),
        keys::lease_agent(&id),
        keys::session(&s1),
        keys::session(&s2),
    ];
    expected.sort();
    assert_eq!(c["keys"], json!(expected));
    assert_eq!(c["since"], json!(since.0));
    let bytes = size(&agent) + size(&lease) + size(&member) + size(&first) + size(&second);
    assert_eq!(c["bytes"], json!(bytes));
    assert_eq!(
        v["totals"],
        json!({ "candidates": 1, "keys": 5, "bytes": bytes })
    );
}

#[tokio::test]
async fn prune_writes_nothing() {
    let fx = Fx::new();
    seed(&fx, "old", None, old(&fx)).await;
    seed(&fx, "live", Some(SessionId::new()), old(&fx)).await;
    let before: Vec<_> = fx
        .state
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.revision, e.value.len()))
        .collect();

    let v = prune(&fx).await;
    assert_eq!(v["totals"]["candidates"], json!(1));
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
}
