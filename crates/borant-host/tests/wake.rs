//! A direct message must wake an idle session after a bus outage, and after a
//! host daemon restart.
//!
//! The outage of 2026-10-25 hung the inbox pull of the laptop host for good:
//! the NATS request in flight when the connection died never answered, so the
//! loop never pulled again, never queued the message, and never woke the
//! stop-wait waiter.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use borant_core::model::{Envelope, Harness, Target};
use borant_core::ports::{Delivery, Incoming, MessageBus, PortResult};
use borant_core::proto::LocalRegister;
use borant_core::{AgentId, SessionId, SystemClock, subjects, testing};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use bytes::Bytes;
use futures::stream::BoxStream;

/// A bus whose `pull` can stall forever, like a NATS request sent on a
/// connection that died before its reply.
#[derive(Debug, Clone)]
struct StallingBus {
    inner: MemoryBus,
    stall: Arc<AtomicBool>,
    /// How many pulls entered the stall. The test waits for one before it
    /// publishes, so the message cannot arrive through an older pull.
    hung: Arc<AtomicUsize>,
}

impl StallingBus {
    fn new(inner: MemoryBus) -> Self {
        Self {
            inner,
            stall: Arc::new(AtomicBool::new(false)),
            hung: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn stall(&self, on: bool) {
        self.stall.store(on, Ordering::SeqCst);
    }

    fn hung(&self) -> usize {
        self.hung.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MessageBus for StallingBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.inner.publish(subject, env).await
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.ensure_inbox(agent, subjects).await
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        if self.stall.load(Ordering::SeqCst) {
            self.hung.fetch_add(1, Ordering::SeqCst);
            // The request is out and its reply never comes: exactly what
            // stopped the laptop host.
            std::future::pending::<()>().await;
        }
        self.inner.pull(agent, max, wait).await
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.inbox_pending(agent).await
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

/// Waits until the inbox loop is inside a stalled pull, so the message cannot
/// arrive through a pull that started before the outage.
async fn wait_hung(bus: &StallingBus) {
    // The first pull can last the bus long poll (5 s), so this waits longer.
    for _ in 0..400 {
        if bus.hung() > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the inbox pull never stalled");
}

fn cfg(dir: &std::path::Path) -> HostConfig {
    let mut cfg = HostConfig::new("macbook".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::ClaudeCode];
    // A short limit keeps the test fast; production uses 12 s.
    cfg.inbox_pull_timeout = Duration::from_millis(200);
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
            agent: Some("waker".parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    (s.session, "waker".parse().unwrap())
}

#[tokio::test]
async fn wake_after_a_stalled_inbox_pull() {
    let dir = tempfile::tempdir().unwrap();
    let inner = MemoryBus::new();
    let sessions = MemorySession::new();
    let _hub = common::serve_hub(&inner, hub_on(&inner, &sessions).await).await;
    let bus = Arc::new(StallingBus::new(inner.clone()));
    let host = host_on(cfg(dir.path()), bus.clone(), &sessions);
    let (session, agent) = register(&host, dir.path()).await;

    // The stop-wait waiter of the idle session.
    let waiter = {
        let host = host.clone();
        tokio::spawn(async move { host.wait(session, Duration::from_secs(20)).await })
    };

    // The broker goes away: the next pull never answers, as in the outage.
    bus.stall(true);
    wait_hung(&bus).await;
    // A direct message arrives while the pull is stuck.
    inner
        .publish(
            &subjects::agent(&agent),
            &testing::envelope(Target::Agent(agent.clone()), "wake up"),
        )
        .await
        .unwrap();
    // The broker is back, but the stuck request stays stuck: only the loop's
    // own limit gets it pulling again.
    bus.stall(false);

    let msgs = tokio::time::timeout(Duration::from_secs(6), waiter)
        .await
        .expect("the waiter must wake after the outage")
        .unwrap()
        .unwrap();
    assert!(msgs.iter().any(|e| e.text == "wake up"), "{msgs:?}");
}

#[tokio::test]
async fn wake_after_a_host_restart() {
    let dir = tempfile::tempdir().unwrap();
    let inner = MemoryBus::new();
    let sessions = MemorySession::new();
    let _hub = common::serve_hub(&inner, hub_on(&inner, &sessions).await).await;

    // The old daemon: its pull hangs, so it cannot take the message.
    let old_bus = Arc::new(StallingBus::new(inner.clone()));
    let old = host_on(cfg(dir.path()), old_bus.clone(), &sessions);
    let (session, agent) = register(&old, dir.path()).await;
    old_bus.stall(true);
    wait_hung(&old_bus).await;

    // The message arrives while no host runs, then the daemon starts again
    // from the same home.
    inner
        .publish(
            &subjects::agent(&agent),
            &testing::envelope(Target::Agent(agent.clone()), "wake up"),
        )
        .await
        .unwrap();
    let new_bus = Arc::new(StallingBus::new(inner.clone()));
    let new = host_on(cfg(dir.path()), new_bus, &sessions);
    new.restore().await;

    let msgs = tokio::time::timeout(
        Duration::from_secs(6),
        new.wait(session, Duration::from_secs(5)),
    )
    .await
    .expect("the waiter must wake after the restart")
    .unwrap();
    assert!(msgs.iter().any(|e| e.text == "wake up"), "{msgs:?}");
}
