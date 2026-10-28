mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use borant_core::model::{
    Agent, Channel, ChannelKind, Envelope, Harness, Member, SessionEvent, SessionEventBody, Target,
};
use borant_core::ports::{
    Change, Delivery, Entry, Incoming, MessageBus, PortError, PortResult, SessionStore, StateStore,
};
use borant_core::proto::{CtlRequest, Reply, ToolCall};
use borant_core::{AgentId, ChannelName, EventId, ManualClock, Timestamp, keys, subjects, testing};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use bytes::Bytes;
use common::Fx;
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn agent_doc(id: &AgentId, channels: Vec<ChannelName>) -> Agent {
    Agent {
        id: id.clone(),
        spawn_key: None,
        task: None,
        stop_on_close: false,
        continues: None,
        retired: false,
        preferred_harness: Harness::Fake,
        channels,
        created: Timestamp(1),
        bundles: Vec::new(),
        ..Default::default()
    }
}

async fn put<T: serde::Serialize>(state: &dyn StateStore, key: &str, v: &T) {
    state
        .create(key, serde_json::to_vec(v).unwrap().into())
        .await
        .unwrap();
}

fn hub(
    state: Arc<dyn StateStore>,
    bus: Arc<dyn MessageBus>,
    sessions: Arc<dyn SessionStore>,
) -> Hub {
    Hub::new(
        state,
        bus,
        sessions,
        Arc::new(ManualClock::new(Timestamp(1_000))),
        HubConfig::default(),
    )
}

#[tokio::test]
async fn export_import_round_trip() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: "api".parse().unwrap(),
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
    fx.sessions
        .append(
            &owner.agent,
            &SessionEvent {
                id: EventId::new(),
                agent: owner.agent.clone(),
                session: Some(owner.session),
                harness: Harness::Fake,
                host: "h1".parse().unwrap(),
                turn: 1,
                at: Timestamp(0),
                body: SessionEventBody::UserPrompt {
                    text: "plan the work".into(),
                },
            },
        )
        .await
        .unwrap();
    let pending = testing::envelope(Target::Agent(owner.agent.clone()), "while you were away");
    fx.bus
        .publish(&subjects::agent(&owner.agent), &pending)
        .await
        .unwrap();

    let mut buf = Vec::new();
    borant_hub::export::export(&fx.hub, &mut buf).await.unwrap();

    let state = MemoryState::new();
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let report = borant_hub::export::import(&state, &bus, &sessions, &mut buf.as_slice())
        .await
        .unwrap();
    assert_eq!(report.sessions, 1);
    assert_eq!(report.pending, 1);
    assert_eq!(
        state.list("").await.unwrap().len(),
        fx.state.list("").await.unwrap().len()
    );
    let got = bus
        .pull(&owner.agent, 10, std::time::Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].envelope.id, pending.id);
}

/// A session store whose first `read` blocks until the test releases it.
#[derive(Debug, Clone)]
struct GatedSessions {
    inner: MemorySession,
    gate: Arc<Gate>,
}

#[derive(Debug, Default)]
struct Gate {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl Gate {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
        }
    }

    async fn block_once(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for GatedSessions {
    async fn append(&self, agent: &AgentId, ev: &SessionEvent) -> PortResult<u64> {
        self.inner.append(agent, ev).await
    }

    async fn read(&self, agent: &AgentId) -> PortResult<Vec<(u64, SessionEvent)>> {
        self.gate.block_once().await;
        self.inner.read(agent).await
    }

    async fn trim_before(&self, agent: &AgentId, seq: u64) -> PortResult<()> {
        self.inner.trim_before(agent, seq).await
    }

    async fn delete(&self, agent: &AgentId) -> PortResult<()> {
        self.inner.delete(agent).await
    }

    async fn agents(&self) -> PortResult<Vec<AgentId>> {
        self.inner.agents().await
    }
}

#[tokio::test]
async fn export_snapshot_keeps_serving() {
    let state = MemoryState::new();
    let gate = Arc::new(Gate::new());
    let sessions = GatedSessions {
        inner: MemorySession::new(),
        gate: gate.clone(),
    };
    let id: AgentId = "w1".parse().unwrap();
    put(&state, &keys::agent(&id), &agent_doc(&id, Vec::new())).await;
    let bus = MemoryBus::new();
    let hub = Arc::new(hub(
        Arc::new(state.clone()),
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
    ));
    let shutdown = CancellationToken::new();
    let running = tokio::spawn(hub.clone().run(shutdown.clone()));

    // Wait until the hub serves requests.
    let list = serde_json::to_vec(&CtlRequest::Call {
        actor: None,
        key: None,
        call: ToolCall::ListAgents,
    })
    .unwrap();
    let mut serving = false;
    for _ in 0..200 {
        if bus
            .request(subjects::CTL, list.clone().into(), Duration::from_secs(1))
            .await
            .is_ok()
        {
            serving = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(serving, "the hub did not start serving");

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("snapshot.jsonl");
    let req = serde_json::to_vec(&CtlRequest::Snapshot { out: out.clone() }).unwrap();
    let snapshot = {
        let bus = bus.clone();
        tokio::spawn(async move {
            bus.request(subjects::CTL, req.into(), Duration::from_secs(10))
                .await
        })
    };

    // The snapshot now waits on the session log, after it released `mem`.
    gate.entered.notified().await;
    let answer = bus
        .request(subjects::CTL, list.into(), Duration::from_secs(1))
        .await;
    let answer = answer.expect("the hub must keep serving during a snapshot");
    let reply: Reply = serde_json::from_slice(&answer).unwrap();
    assert!(
        reply.into_result().is_ok(),
        "no maintenance mode and no held request mutex"
    );

    gate.release.notify_one();
    let answer = snapshot.await.unwrap().unwrap();
    let reply: Reply = serde_json::from_slice(&answer).unwrap();
    let report = reply.into_result().unwrap();
    assert_eq!(report["path"], json!(out));
    assert!(out.exists());

    shutdown.cancel();
    running.await.unwrap().unwrap();
}

/// A state store whose `list("")` fails, like a key-iterator error.
#[derive(Debug, Clone)]
struct FailingState {
    inner: MemoryState,
}

#[async_trait::async_trait]
impl StateStore for FailingState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        self.inner.get(key).await
    }

    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64> {
        self.inner.create(key, value).await
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        self.inner.update(key, value, expected).await
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        self.inner.delete(key, expected).await
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        if prefix.is_empty() {
            return Err(PortError::other("the key iterator failed"));
        }
        self.inner.list(prefix).await
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        self.inner.watch(prefix).await
    }
}

#[tokio::test]
async fn export_snapshot_fails_without_a_file() {
    let hub = hub(
        Arc::new(FailingState {
            inner: MemoryState::new(),
        }),
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("snapshot.jsonl");
    let err = borant_hub::export::snapshot(&hub, &out).await.unwrap_err();
    assert!(err.to_string().contains("key iterator"), "{err}");
    assert!(!out.exists(), "no file at the target path");
    let tmp = format!("{}.tmp", out.display());
    assert!(!std::path::Path::new(&tmp).exists(), "no partial file");
}

/// A bus that appends the receipt of the message to the session log and acks
/// it, after `unacked` returned the message for the export.
#[derive(Debug, Clone)]
struct AckingBus {
    inner: MemoryBus,
    sessions: MemorySession,
    done: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl MessageBus for AckingBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.inner.publish(subject, env).await
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.ensure_inbox(agent, subjects).await
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        self.inner.pull(agent, max, wait).await
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.inbox_pending(agent).await
    }

    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>> {
        let got = self.inner.unacked(agent).await?;
        if !got.is_empty() && !self.done.swap(true, Ordering::SeqCst) {
            for d in self.inner.pull(agent, got.len(), Duration::ZERO).await? {
                let ev = SessionEvent {
                    id: EventId::new(),
                    agent: agent.clone(),
                    session: None,
                    harness: Harness::Fake,
                    host: "h1".parse().unwrap(),
                    turn: 0,
                    at: Timestamp(1),
                    body: SessionEventBody::Delivered {
                        message: d.envelope.id,
                    },
                };
                self.sessions.append(agent, &ev).await?;
                d.acker.ack().await?;
            }
        }
        Ok(got)
    }

    async fn delete_inbox(&self, agent: &AgentId) -> PortResult<()> {
        self.inner.delete_inbox(agent).await
    }

    async fn subscribe(&self, pattern: &str) -> PortResult<BoxStream<'static, Envelope>> {
        self.inner.subscribe(pattern).await
    }

    async fn request(&self, subject: &str, payload: Bytes, timeout: Duration) -> PortResult<Bytes> {
        self.inner.request(subject, payload, timeout).await
    }

    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>> {
        self.inner.serve(subject).await
    }

    async fn send(&self, subject: &str, payload: Bytes) -> PortResult<()> {
        self.inner.send(subject, payload).await
    }
}

#[tokio::test]
async fn export_snapshot_keeps_a_message_acked_during_it() {
    let state = MemoryState::new();
    let id: AgentId = "w1".parse().unwrap();
    put(&state, &keys::agent(&id), &agent_doc(&id, Vec::new())).await;
    let inner = MemoryBus::new();
    let sessions = MemorySession::new();
    let bus = AckingBus {
        inner: inner.clone(),
        sessions: sessions.clone(),
        done: Arc::new(AtomicBool::new(false)),
    };
    inner
        .ensure_inbox(&id, &subjects::inbox(&id, &[]))
        .await
        .unwrap();
    let pending = testing::envelope(Target::Agent(id.clone()), "hello");
    inner
        .publish(&subjects::agent(&id), &pending)
        .await
        .unwrap();

    let hub = hub(Arc::new(state), Arc::new(bus), Arc::new(sessions));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("snapshot.jsonl");
    let report = borant_hub::export::snapshot(&hub, &out).await.unwrap();
    assert_eq!(report.pending, 1);
    assert_eq!(report.sessions, 1);

    // The receipt event was appended after the inbox read, so the message is
    // in both copies.
    let text = std::fs::read_to_string(&out).unwrap();
    let msg = pending.id.to_string();
    assert!(
        text.lines()
            .any(|l| l.starts_with("{\"t\":\"pending\"") && l.contains(&msg)),
        "the inbox copy must be in the snapshot"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("{\"t\":\"session\"") && l.contains(&msg)),
        "the session-log copy must be in the snapshot"
    );

    // The import takes the pending copy and the session event; the inbox
    // holds the message again for its own agent.
    let imported = MemoryState::new();
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let mut r = std::io::BufReader::new(std::fs::File::open(&out).unwrap());
    let report = borant_hub::export::import(&imported, &bus, &sessions, &mut r)
        .await
        .unwrap();
    assert_eq!(report.pending, 1);
    let got = bus.unacked(&id).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, pending.id);
}

#[tokio::test]
async fn export_import_sends_pending_only_to_its_agent() {
    let fx = Fx::new();
    let channel = api();
    let a: AgentId = "a1".parse().unwrap();
    let b: AgentId = "b1".parse().unwrap();
    let owner = Channel {
        name: channel.clone(),
        owner: Some(a.clone()),
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
    put(&fx.state, &keys::channel(&channel), &owner).await;
    for agent in [&a, &b] {
        put(
            &fx.state,
            &keys::agent(agent),
            &agent_doc(agent, vec![channel.clone()]),
        )
        .await;
        put(
            &fx.state,
            &keys::member(&channel, agent),
            &Member {
                agent: agent.clone(),
                joined: Timestamp(1),
            },
        )
        .await;
    }

    // `a` was a member before the broadcast; `b` joined after it, so only
    // `a` has the message in its inbox.
    fx.bus
        .ensure_inbox(&a, &subjects::inbox(&a, std::slice::from_ref(&channel)))
        .await
        .unwrap();
    let pending = testing::envelope(Target::Channel(channel.clone()), "for the channel");
    fx.bus
        .publish(&subjects::broadcast(&channel), &pending)
        .await
        .unwrap();
    fx.bus
        .ensure_inbox(&b, &subjects::inbox(&b, std::slice::from_ref(&channel)))
        .await
        .unwrap();
    assert!(fx.bus.unacked(&b).await.unwrap().is_empty());

    let mut buf = Vec::new();
    borant_hub::export::export(&fx.hub, &mut buf).await.unwrap();

    let state = MemoryState::new();
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let report = borant_hub::export::import(&state, &bus, &sessions, &mut buf.as_slice())
        .await
        .unwrap();
    assert_eq!(report.pending, 1);

    let got = bus.unacked(&a).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, pending.id);
    assert!(
        bus.unacked(&b).await.unwrap().is_empty(),
        "a pending message must not reach another member of the channel"
    );
}

#[tokio::test]
async fn export_snapshot_lock_is_short() {
    let fx = Fx::new();
    let mut n = 0;
    for i in 0..1307 {
        put(&fx.state, &format!("chan.c{i}"), &json!({})).await;
        n += 1;
    }
    for i in 0..158 {
        put(&fx.state, &format!("session.s{i}"), &json!({})).await;
        n += 1;
    }
    for i in 0..147 {
        put(&fx.state, &format!("lease.agent.w{i}"), &json!({})).await;
        n += 1;
    }
    for i in 0..146 {
        let id: AgentId = format!("w{i}").parse().unwrap();
        put(&fx.state, &keys::agent(&id), &agent_doc(&id, Vec::new())).await;
        n += 1;
    }
    assert_eq!(n, 1758);

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("snapshot.jsonl");
    let report = borant_hub::export::snapshot(&fx.hub, &out).await.unwrap();
    assert_eq!(report.state, 1758);
    assert!(
        report.lock_ms < 1000,
        "the hub held its request mutex for {} ms",
        report.lock_ms
    );
    assert!(out.exists());
}
