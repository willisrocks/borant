#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{Harness, HostInfo, RepoInfo};
use borant_core::ports::{Incoming, MessageBus};
use borant_core::proto::{
    Actor, ApiError, CtlRequest, HeartbeatReply, HostCommand, RegisterSession, Registered,
    SessionLeaseRef, ToolCall,
};
use borant_core::{HostName, ManualClock, SessionId, Timestamp, subjects};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;

pub struct Fx {
    pub hub: Arc<Hub>,
    pub state: MemoryState,
    pub bus: MemoryBus,
    pub sessions: MemorySession,
    pub clock: Arc<ManualClock>,
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
        let state = MemoryState::new();
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let hub = Arc::new(Hub::new(
            Arc::new(state.clone()),
            Arc::new(bus.clone()),
            Arc::new(sessions.clone()),
            clock.clone(),
            HubConfig::default(),
        ));
        Self {
            hub,
            state,
            bus,
            sessions,
            clock,
        }
    }

    pub async fn req(&self, r: CtlRequest) -> Result<Value, ApiError> {
        self.hub.handle(r).await.into_result()
    }

    pub async fn host(&self, n: &str, harnesses: &[Harness]) {
        self.pool_host(n, harnesses, None).await;
    }

    /// Registers a host that belongs to `pool`, when it is one.
    pub async fn pool_host(&self, n: &str, harnesses: &[Harness], pool: Option<&str>) {
        self.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name(n),
                harnesses: harnesses.to_vec(),
                launcher: "process".into(),
                max_agents: 4,
                pool: pool.map(|p| p.parse().unwrap()),
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
                ..Default::default()
            }))
            .await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    pub async fn register(
        &self,
        host: &str,
        agent: Option<&str>,
        channel: Option<&str>,
    ) -> Registered {
        self.try_register(host, agent, channel, true).await.unwrap()
    }

    pub async fn call(&self, who: Option<&Registered>, call: ToolCall) -> Result<Value, ApiError> {
        let actor = who.map(|r| Actor {
            agent: r.agent.clone(),
            session: r.session,
            token: r.token,
        });
        self.req(CtlRequest::Call { actor, call }).await
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
            })
            .collect();
        let v = self
            .req(CtlRequest::HostHeartbeat {
                host: name(host),
                sessions,
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
