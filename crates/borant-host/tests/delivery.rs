//! A message that a paused session pulled goes back to the inbox at once, and
//! a message that comes again after an ack went missing reaches the harness
//! once.
//!
//! The nats_restart e2e of phase 10 found the first one: the broker went down,
//! every host paused its sessions, and a message that a pull took while the
//! session was paused stayed unacked until the 60 s ack wait. The 30 s the
//! spec promises were gone. The second one is the other half: a redelivery
//! that the host logged already must not reach the harness again.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use borant_core::model::SessionEventBody;
use borant_core::model::{Envelope, Harness, Target};
use borant_core::ports::{
    Acker, Delivery, Incoming, MessageBus, PortError, PortResult, SessionStore,
};
use borant_core::proto::LocalRegister;
use borant_core::{AgentId, SessionId, SystemClock, subjects, testing};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use bytes::Bytes;
use futures::stream::BoxStream;
use tokio::sync::Semaphore;

/// A bus that can hold a pull's delivery until the test lets it through, and
/// whose ack can fail once, as an ack that the connection lost.
#[derive(Debug, Clone)]
struct TestBus {
    inner: MemoryBus,
    /// Set to hold the next delivery in `pull` until a permit arrives.
    hold: Arc<AtomicBool>,
    /// True while a pull holds a delivery.
    holding: Arc<AtomicBool>,
    gate: Arc<Semaphore>,
    /// The next ack hands the message back and fails.
    fail_ack: Arc<AtomicBool>,
    /// How many non-empty pulls came back.
    pulls: Arc<AtomicUsize>,
}

impl TestBus {
    fn new(inner: MemoryBus) -> Self {
        Self {
            inner,
            hold: Arc::new(AtomicBool::new(false)),
            holding: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(Semaphore::new(0)),
            fail_ack: Arc::new(AtomicBool::new(false)),
            pulls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn hold(&self, on: bool) {
        self.hold.store(on, Ordering::SeqCst);
    }

    fn holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
    }

    fn release(&self) {
        self.gate.add_permits(1);
    }

    fn fail_next_ack(&self) {
        self.fail_ack.store(true, Ordering::SeqCst);
    }

    fn pulls(&self) -> usize {
        self.pulls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MessageBus for TestBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.inner.publish(subject, env).await
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.ensure_inbox(agent, subjects).await
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        let got = self.inner.pull(agent, max, wait).await?;
        if got.is_empty() {
            return Ok(got);
        }
        self.pulls.fetch_add(1, Ordering::SeqCst);
        let got: Vec<Delivery> = got
            .into_iter()
            .map(|d| Delivery {
                envelope: d.envelope,
                acker: Box::new(TestAck {
                    inner: d.acker,
                    fail: self.fail_ack.clone(),
                }),
            })
            .collect();
        if self.hold.load(Ordering::SeqCst) {
            self.holding.store(true, Ordering::SeqCst);
            let _permit = self.gate.acquire().await;
            self.holding.store(false, Ordering::SeqCst);
        }
        Ok(got)
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.inbox_pending(agent).await
    }

    async fn inbox_stats(&self, agent: &AgentId) -> PortResult<borant_core::ports::InboxStats> {
        self.inner.inbox_stats(agent).await
    }

    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>> {
        self.inner.unacked(agent).await
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

/// The acker of a delivery that the test can make fail once.
struct TestAck {
    inner: Box<dyn Acker>,
    fail: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Acker for TestAck {
    async fn ack(self: Box<Self>) -> PortResult<()> {
        let TestAck { inner, fail } = *self;
        if fail.swap(false, Ordering::SeqCst) {
            // The ack is gone: the inbox hands the message out again.
            inner.nak().await?;
            return Err(PortError::unavailable("the ack was lost"));
        }
        inner.ack().await
    }

    async fn nak(self: Box<Self>) -> PortResult<()> {
        self.inner.nak().await
    }
}

fn cfg(dir: &std::path::Path) -> HostConfig {
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::ClaudeCode];
    // A short lease makes the pause of an outage quick; the limit must outlast
    // the gate that holds a pull.
    cfg.lease = Duration::from_millis(300);
    cfg.inbox_pull_timeout = Duration::from_secs(10);
    cfg
}

fn host_on(cfg: HostConfig, bus: Arc<dyn MessageBus>, sessions: &MemorySession) -> Host {
    Host::new(
        cfg.clone(),
        bus,
        Arc::new(sessions.clone()),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(cfg.home.join("logs"))),
        None,
        None,
        Outbox::in_memory().unwrap(),
    )
}

async fn hub_on(bus: &MemoryBus, sessions: &MemorySession) -> Arc<Hub> {
    Arc::new(Hub::new(
        Arc::new(MemoryState::new()),
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
        Arc::new(SystemClock),
        HubConfig::default(),
    ))
}

/// Registers a live session: the restore only keeps sessions whose PID runs.
async fn register(host: &Host, dir: &std::path::Path) -> (SessionId, AgentId) {
    let s = host
        .register(LocalRegister {
            harness: Harness::ClaudeCode,
            pid: Some(std::process::id()),
            cwd: dir.to_string_lossy().into_owned(),
            agent: Some("nak".parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    (s.session, "nak".parse().unwrap())
}

/// Waits until a pull holds a delivery.
async fn wait_holding(bus: &TestBus) {
    for _ in 0..100 {
        if bus.holding() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the pull never held a delivery");
}

/// How often the session log of `agent` holds the message `id`.
async fn logged(sessions: &MemorySession, agent: &AgentId, id: borant_core::MessageId) -> usize {
    let events = sessions.read(agent).await.expect("the session log");
    events
        .into_iter()
        .filter(|(_, e)| match &e.body {
            SessionEventBody::SwarmMessage { envelope } => envelope.id == id,
            _ => false,
        })
        .count()
}

#[tokio::test]
async fn delivery_to_a_paused_session_goes_back_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let inner = MemoryBus::new();
    let sessions = MemorySession::new();
    let hub = hub_on(&inner, &sessions).await;
    let stop = common::serve_hub(&inner, hub.clone()).await;
    let bus = Arc::new(TestBus::new(inner.clone()));
    let host = host_on(cfg(dir.path()), bus.clone(), &sessions);
    let (session, agent) = register(&host, dir.path()).await;
    host.heartbeat_once().await;
    assert!(host.local_view(session).unwrap().fenced.is_none());

    // The hub goes away, and a heartbeat after the lease pauses the session.
    stop.cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    host.heartbeat_once().await;
    assert!(
        host.local_view(session).unwrap().fenced.is_some(),
        "the session must be paused"
    );

    // A pull that started before the pause takes the message, and the test
    // holds it until the session is paused.
    bus.hold(true);
    let env = testing::envelope(Target::Agent(agent.clone()), "while paused");
    inner.publish(&subjects::agent(&agent), &env).await.unwrap();
    wait_holding(&bus).await;
    bus.release();

    // The paused session hands the message back: it is not ack pending, so
    // the inbox delivers it again at once, not after the ack wait.
    common::eventually(
        || async { inner.inbox_stats(&agent).await.unwrap().ack_pending == 0 },
        "the paused session to hand the message back",
    )
    .await;

    // The hub is back, and the session resumes.
    let _stop = common::serve_hub(&inner, hub.clone()).await;
    for _ in 0..20 {
        host.heartbeat_once().await;
        if host.local_view(session).unwrap().fenced.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        host.local_view(session).unwrap().fenced.is_none(),
        "the session must resume"
    );

    // The message comes back and reaches the harness once.
    let msgs = tokio::time::timeout(
        Duration::from_secs(5),
        host.wait(session, Duration::from_secs(4)),
    )
    .await
    .expect("the message must arrive after the pause")
    .unwrap();
    assert_eq!(
        msgs.iter().filter(|m| m.text == "while paused").count(),
        1,
        "{msgs:?}"
    );
    assert_eq!(logged(&sessions, &agent, env.id).await, 1);
    let again = host
        .wait(session, Duration::from_millis(300))
        .await
        .unwrap();
    assert!(
        !again.iter().any(|m| m.text == "while paused"),
        "the message arrived twice: {again:?}"
    );
}

#[tokio::test]
async fn a_delivery_after_a_nak_reaches_the_harness_once() {
    let dir = tempfile::tempdir().unwrap();
    let inner = MemoryBus::new();
    let sessions = MemorySession::new();
    let _hub = common::serve_hub(&inner, hub_on(&inner, &sessions).await).await;
    let bus = Arc::new(TestBus::new(inner.clone()));
    let host = host_on(cfg(dir.path()), bus.clone(), &sessions);
    let (session, agent) = register(&host, dir.path()).await;

    // The host logs the message, and its ack fails: the inbox hands it out
    // again, as it does after an ack wait or a nak.
    bus.fail_next_ack();
    let env = testing::envelope(Target::Agent(agent.clone()), "logged once");
    inner.publish(&subjects::agent(&agent), &env).await.unwrap();

    common::eventually(|| async { bus.pulls() >= 2 }, "the message to come again").await;

    let msgs = tokio::time::timeout(
        Duration::from_secs(5),
        host.wait(session, Duration::from_secs(4)),
    )
    .await
    .expect("the message must reach the harness")
    .unwrap();
    assert_eq!(
        msgs.iter().filter(|m| m.text == "logged once").count(),
        1,
        "{msgs:?}"
    );
    assert_eq!(
        logged(&sessions, &agent, env.id).await,
        1,
        "the redelivery must not enter the session log twice"
    );
    assert_eq!(inner.inbox_stats(&agent).await.unwrap().ack_pending, 0);
    let again = host
        .wait(session, Duration::from_millis(300))
        .await
        .unwrap();
    assert!(
        !again.iter().any(|m| m.text == "logged once"),
        "the message arrived twice: {again:?}"
    );
}
