#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::model::{Envelope, Harness, HostInfo, RepoInfo};
use borant_core::ports::{
    EventLog, Incoming, MessageBus, Notice, Notifier, PortResult, StateStore,
};
use borant_core::proto::{
    Actor, ApiError, CtlRequest, HeartbeatReply, HostCommand, RegisterSession, Registered,
    SessionLeaseRef, ToolCall,
};
use borant_core::{AgentId, HostName, ManualClock, SessionId, Timestamp, subjects};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;

/// The notices of the hub notifier, so a test reads what the person heard.
#[derive(Debug, Default)]
pub struct Recorder {
    notices: Mutex<Vec<Notice>>,
}

impl Recorder {
    pub fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Notifier for Recorder {
    async fn notify(&self, notice: &Notice) -> PortResult<()> {
        self.notices.lock().unwrap().push(notice.clone());
        Ok(())
    }
}

pub struct Fx {
    pub hub: Arc<Hub>,
    pub state: MemoryState,
    pub bus: MemoryBus,
    pub sessions: MemorySession,
    pub events: Arc<dyn EventLog>,
    pub clock: Arc<ManualClock>,
    pub recorder: Arc<Recorder>,
    /// The store calls of the hub since [`Fx::count_from`].
    pub state_calls: Arc<AtomicU64>,
    /// The `inbox_stats` calls of the hub since [`Fx::count_from`].
    pub stats_calls: Arc<AtomicU64>,
}

/// A state store that counts the calls of the hub through it.
#[derive(Debug, Clone)]
pub struct CountingState {
    inner: MemoryState,
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl StateStore for CountingState {
    async fn get(&self, key: &str) -> PortResult<Option<borant_core::ports::Entry>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.get(key).await
    }

    async fn create(&self, key: &str, value: bytes::Bytes) -> PortResult<u64> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.create(key, value).await
    }

    async fn update(&self, key: &str, value: bytes::Bytes, expected: u64) -> PortResult<u64> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.update(key, value, expected).await
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.delete(key, expected).await
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<borant_core::ports::Entry>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.list(prefix).await
    }

    async fn watch(
        &self,
        prefix: &str,
    ) -> PortResult<BoxStream<'static, borant_core::ports::Change>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.watch(prefix).await
    }
}

/// A bus that counts the `inbox_stats` calls: one per agent per caller is
/// what a hub with hundreds of agents must not do each second.
#[derive(Debug)]
pub struct CountingBus {
    inner: Arc<dyn MessageBus>,
    stats: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl MessageBus for CountingBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.inner.publish(subject, env).await
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.ensure_inbox(agent, subjects).await
    }

    async fn recover_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.recover_inbox(agent, subjects).await
    }

    async fn pull(
        &self,
        agent: &AgentId,
        max: usize,
        wait: Duration,
    ) -> PortResult<Vec<borant_core::ports::Delivery>> {
        self.inner.pull(agent, max, wait).await
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.inbox_pending(agent).await
    }

    async fn inbox_stats(&self, agent: &AgentId) -> PortResult<borant_core::ports::InboxStats> {
        self.stats.fetch_add(1, Ordering::Relaxed);
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

    async fn request(
        &self,
        subject: &str,
        payload: bytes::Bytes,
        timeout: Duration,
    ) -> PortResult<bytes::Bytes> {
        self.inner.request(subject, payload, timeout).await
    }

    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>> {
        self.inner.serve(subject).await
    }

    async fn send(&self, subject: &str, payload: bytes::Bytes) -> PortResult<()> {
        self.inner.send(subject, payload).await
    }
}

pub fn name(s: &str) -> HostName {
    s.parse().unwrap()
}

pub fn repo() -> RepoInfo {
    RepoInfo {
        remote_url: "git://git/demo.git".into(),
        branch: "main".into(),
        subdir: String::new(),
    }
}

impl Fx {
    pub fn new() -> Self {
        Self::with(HubConfig::default())
    }

    /// A hub with `cfg`, so a test can name roles or pools.
    pub fn with(cfg: HubConfig) -> Self {
        Self::with_bus(cfg, |bus, _state| Arc::new(bus))
    }

    /// A hub behind the bus that `make` builds from the memory bus and the
    /// state store, for a test that watches a publish itself. `Fx::bus` stays
    /// the memory bus, so an agent subject reads the same messages.
    pub fn with_bus(
        cfg: HubConfig,
        make: impl FnOnce(MemoryBus, MemoryState) -> Arc<dyn MessageBus>,
    ) -> Self {
        Self::build(cfg, make, Arc::new(MemoryEventLog::new()))
    }

    /// A hub whose event log is `log`, so a test sees what the hub read.
    pub fn with_log(cfg: HubConfig, log: Arc<dyn EventLog>) -> Self {
        Self::build(cfg, |bus, _state| Arc::new(bus), log)
    }

    fn build(
        cfg: HubConfig,
        make: impl FnOnce(MemoryBus, MemoryState) -> Arc<dyn MessageBus>,
        events: Arc<dyn EventLog>,
    ) -> Self {
        let state = MemoryState::new();
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let recorder = Arc::new(Recorder::default());
        let state_calls = Arc::new(AtomicU64::new(0));
        let stats_calls = Arc::new(AtomicU64::new(0));
        let counted_state = CountingState {
            inner: state.clone(),
            calls: state_calls.clone(),
        };
        let inner_bus = make(bus.clone(), state.clone());
        let counted_bus = Arc::new(CountingBus {
            inner: inner_bus,
            stats: stats_calls.clone(),
        });
        let hub = Arc::new(
            Hub::new(
                Arc::new(counted_state),
                counted_bus,
                Arc::new(sessions.clone()),
                clock.clone(),
                cfg,
            )
            .with_notifier(recorder.clone())
            .with_events(events.clone()),
        );
        Self {
            hub,
            state,
            bus,
            sessions,
            events,
            clock,
            recorder,
            state_calls,
            stats_calls,
        }
    }

    /// The store calls of the hub from now on.
    pub fn count_from(&self) -> u64 {
        self.state_calls.load(Ordering::Relaxed)
    }

    /// The `inbox_stats` calls of the hub from now on.
    pub fn stats_from(&self) -> u64 {
        self.stats_calls.load(Ordering::Relaxed)
    }

    pub fn count_since(&self, mark: u64) -> u64 {
        self.state_calls.load(Ordering::Relaxed) - mark
    }

    pub async fn req(&self, r: CtlRequest) -> Result<Value, ApiError> {
        self.hub.handle(r).await.into_result()
    }

    pub async fn host(&self, n: &str, harnesses: &[Harness]) {
        self.tagged_host(n, harnesses, None, &[]).await;
    }

    /// Registers a host that belongs to `pool`, when it is one.
    pub async fn pool_host(&self, n: &str, harnesses: &[Harness], pool: Option<&str>) {
        self.tagged_host(n, harnesses, pool, &[]).await;
    }

    /// Registers a host with a pool and the tags that a role asks for.
    pub async fn tagged_host(
        &self,
        n: &str,
        harnesses: &[Harness],
        pool: Option<&str>,
        tags: &[&str],
    ) {
        self.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name(n),
                harnesses: harnesses.to_vec(),
                launcher: "process".into(),
                max_agents: 4,
                pool: pool.map(|p| p.parse().unwrap()),
                tags: tags.iter().map(|t| t.parse().unwrap()).collect(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }

    pub async fn try_register(
        &self,
        host: &str,
        agent: Option<&str>,
        channel: Option<&str>,
        with_repo: bool,
    ) -> Result<Registered, ApiError> {
        self.try_register_in(host, agent, channel, None, with_repo)
            .await
    }

    /// Registers a session that reports a collection.
    pub async fn try_register_in(
        &self,
        host: &str,
        agent: Option<&str>,
        channel: Option<&str>,
        collection: Option<&str>,
        with_repo: bool,
    ) -> Result<Registered, ApiError> {
        let v = self
            .req(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: name(host),
                harness: Harness::Fake,
                pid: Some(1),
                cwd: "/work".into(),
                repo: with_repo.then(repo),
                agent: agent.map(|a| a.parse().unwrap()),
                channel: channel.map(|c| c.parse().unwrap()),
                collection: collection.map(|c| c.parse().unwrap()),
                ..Default::default()
            }))
            .await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    pub async fn register_in(
        &self,
        host: &str,
        agent: Option<&str>,
        channel: Option<&str>,
        collection: Option<&str>,
    ) -> Registered {
        self.try_register_in(host, agent, channel, collection, true)
            .await
            .unwrap()
    }

    pub async fn register(
        &self,
        host: &str,
        agent: Option<&str>,
        channel: Option<&str>,
    ) -> Registered {
        self.try_register(host, agent, channel, true).await.unwrap()
    }

    /// Registers a session that reports a role, as a person's session does
    /// from `BORANT_ROLE`. A chief role with `approves = true` is the one
    /// that can approve.
    pub async fn register_roled(
        &self,
        host: &str,
        agent: &str,
        collection: Option<&str>,
        role: &str,
    ) -> Registered {
        let v = self
            .req(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: name(host),
                harness: Harness::Fake,
                pid: Some(1),
                cwd: "/work".into(),
                repo: Some(repo()),
                agent: Some(agent.parse().unwrap()),
                role: Some(role.parse().unwrap()),
                collection: collection.map(|c| c.parse().unwrap()),
                ..Default::default()
            }))
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    pub async fn call(&self, who: Option<&Registered>, call: ToolCall) -> Result<Value, ApiError> {
        let actor = who.map(|r| Actor {
            agent: r.agent.clone(),
            session: r.session,
            token: r.token,
        });
        self.req(CtlRequest::Call {
            actor,
            key: None,
            call,
        })
        .await
    }

    pub async fn heartbeat(&self, host: &str, regs: &[&Registered]) -> HeartbeatReply {
        let idle: Vec<_> = regs.iter().map(|r| (*r, None)).collect();
        self.heartbeat_idle(host, &idle).await
    }

    /// A heartbeat that also reports the idle signal of each session: the end
    /// of its last turn, or `None` while it is in a turn.
    pub async fn heartbeat_idle(
        &self,
        host: &str,
        regs: &[(&Registered, Option<Timestamp>)],
    ) -> HeartbeatReply {
        let sessions = regs
            .iter()
            .map(|(r, idle_since)| SessionLeaseRef {
                session: r.session,
                agent: r.agent.clone(),
                token: r.token,
                idle_since: *idle_since,
                last_event_at: None,
                pinned: false,
                activity: None,
            })
            .collect();
        let v = self
            .req(CtlRequest::HostHeartbeat {
                host: name(host),
                sessions,
                stats: None,
                launching: Vec::new(),
            })
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    pub fn advance(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }

    pub async fn commands(&self, host: &str) -> BoxStream<'static, HostCommand> {
        self.bus
            .serve(&subjects::host_cmd(&name(host)))
            .await
            .unwrap()
            .filter_map(|i: Incoming| async move { serde_json::from_slice(&i.payload).ok() })
            .boxed()
    }
}

pub async fn next_cmd(s: &mut BoxStream<'static, HostCommand>) -> Option<HostCommand> {
    tokio::time::timeout(Duration::from_millis(200), s.next())
        .await
        .ok()
        .flatten()
}
