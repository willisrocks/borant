//! The Borant host daemon. It serves the local API to harness adapters,
//! delivers messages into sessions, writes session logs and checkpoints, and
//! runs launch orders from the hub.

pub mod api;
pub mod checkpoint;
pub mod client;
mod commands;
mod delivery;
pub mod outbox;
mod sessions;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borant_core::model::{Envelope, Harness, HostInfo};
use borant_core::ports::{LaunchHandle, Launcher, MessageBus, Notifier, SessionStore};
use borant_core::proto::{ApiError, CtlRequest, Registered, Reply};
use borant_core::rules::wake::WakeLimiter;
use borant_core::{AgentId, ChannelName, Clock, HostName, MessageId, SessionId, subjects};
use borant_launch::harness::HarnessCommands;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::checkpoint::RepoFacts;
use crate::outbox::Outbox;

pub use client::LocalClient;
pub use commands::DesktopNotifier;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub name: HostName,
    /// The Borant home folder: socket, outbox, logs, and work trees.
    pub home: PathBuf,
    pub launcher: String,
    pub max_agents: u32,
    pub always_on: bool,
    pub ssh: Option<String>,
    pub harnesses: Vec<Harness>,
    pub commands: HarnessCommands,
    /// Where checkpoints go. Defaults to the `origin` remote of each repo.
    pub checkpoint_remote: Option<String>,
    pub lease: Duration,
    pub heartbeat: Duration,
    pub wakes_per_hour: u32,
    pub keep_turns: usize,
    pub notify: bool,
    pub hub_timeout: Duration,
}

impl HostConfig {
    pub fn new(name: HostName, home: PathBuf) -> Self {
        Self {
            name,
            home,
            launcher: "tmux".into(),
            max_agents: 4,
            always_on: false,
            ssh: None,
            harnesses: vec![Harness::ClaudeCode, Harness::Pi, Harness::Omp],
            commands: HarnessCommands::default(),
            checkpoint_remote: None,
            lease: Duration::from_secs(30),
            heartbeat: Duration::from_secs(10),
            wakes_per_hour: 30,
            keep_turns: 50,
            notify: false,
            hub_timeout: Duration::from_secs(5),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("host.sock")
    }

    pub fn info(&self) -> HostInfo {
        HostInfo {
            name: self.name.clone(),
            harnesses: self.harnesses.clone(),
            launcher: self.launcher.clone(),
            max_agents: self.max_agents,
            always_on: self.always_on,
            draining: false,
            ssh: self.ssh.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Queued {
    pub env: Envelope,
    pub wake: bool,
}

#[derive(Debug)]
pub(crate) struct LocalSess {
    pub id: SessionId,
    pub harness: Harness,
    pub harness_session_id: Option<String>,
    pub pid: Option<u32>,
    pub cwd: PathBuf,
    pub agent_hint: Option<AgentId>,
    pub channel_hint: Option<ChannelName>,
    pub repo: Option<RepoFacts>,
    pub shared_dir: bool,
    pub reg: Option<Registered>,
    pub queue: VecDeque<Queued>,
    pub notify: Arc<Notify>,
    pub turn: u64,
    pub busy: bool,
    /// Set when the session lost its lease. Tools stay blocked.
    pub fenced: Option<String>,
    /// Set while the hub is unreachable for longer than a lease.
    pub paused: bool,
    pub moving: bool,
    pub last_tree: Option<String>,
    pub checkpointing: bool,
    pub dirty: bool,
    pub launch: Option<LaunchHandle>,
    pub cancel: CancellationToken,
}

impl LocalSess {
    pub fn movable(&self) -> bool {
        self.reg.as_ref().is_some_and(|r| r.movable) && self.repo.is_some()
    }
}

#[derive(Debug)]
pub(crate) struct State {
    pub sessions: HashMap<SessionId, LocalSess>,
    pub limiter: WakeLimiter,
    pub limit_noticed: HashSet<AgentId>,
    pub pending_launch: HashMap<AgentId, LaunchHandle>,
    pub seen: HashMap<AgentId, HashSet<MessageId>>,
    pub turns_since_trim: HashMap<AgentId, usize>,
    pub host_registered: bool,
}

pub(crate) struct Inner {
    pub cfg: HostConfig,
    pub bus: Arc<dyn MessageBus>,
    pub store: Arc<dyn SessionStore>,
    pub clock: Arc<dyn Clock>,
    pub launcher: Box<dyn Launcher>,
    pub notifier: Option<Box<dyn Notifier>>,
    pub outbox: Outbox,
    pub state: Mutex<State>,
    pub last_ok: Mutex<Instant>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

/// A cheap handle to the host daemon.
#[derive(Debug, Clone)]
pub struct Host {
    pub(crate) inner: Arc<Inner>,
}

impl Host {
    pub fn new(
        cfg: HostConfig,
        bus: Arc<dyn MessageBus>,
        store: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
        launcher: Box<dyn Launcher>,
        notifier: Option<Box<dyn Notifier>>,
        outbox: Outbox,
    ) -> Self {
        let wakes = cfg.wakes_per_hour;
        Self {
            inner: Arc::new(Inner {
                cfg,
                bus,
                store,
                clock,
                launcher,
                notifier,
                outbox,
                state: Mutex::new(State {
                    sessions: HashMap::new(),
                    limiter: WakeLimiter::new(wakes),
                    limit_noticed: HashSet::new(),
                    pending_launch: HashMap::new(),
                    seen: HashMap::new(),
                    turns_since_trim: HashMap::new(),
                    host_registered: false,
                }),
                last_ok: Mutex::new(Instant::now()),
            }),
        }
    }

    pub fn config(&self) -> &HostConfig {
        &self.inner.cfg
    }

    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends a request to the hub and waits for its reply.
    pub(crate) async fn hub(&self, req: &CtlRequest) -> Result<serde_json::Value, ApiError> {
        let payload = serde_json::to_vec(req).map_err(|e| ApiError::internal(e.to_string()))?;
        let bytes = self
            .inner
            .bus
            .request(subjects::CTL, payload.into(), self.inner.cfg.hub_timeout)
            .await
            .map_err(|e| ApiError::unavailable(format!("hub unavailable: {}", e.message)))?;
        serde_json::from_slice::<Reply>(&bytes)
            .map_err(|e| ApiError::internal(format!("bad hub reply: {e}")))?
            .into_result()
    }

    /// Runs the background loops until `shutdown`: heartbeats, PID checks,
    /// the outbox flusher, host commands, and the notifier.
    pub async fn run(&self, shutdown: CancellationToken) {
        let mut tasks = vec![
            tokio::spawn(self.clone().heartbeat_loop(shutdown.clone())),
            tokio::spawn(self.clone().liveness_loop(shutdown.clone())),
            tokio::spawn(self.clone().outbox_loop(shutdown.clone())),
            tokio::spawn(self.clone().command_loop(shutdown.clone())),
        ];
        if self.inner.cfg.notify && self.inner.notifier.is_some() {
            tasks.push(tokio::spawn(self.clone().notify_loop(shutdown.clone())));
        }
        shutdown.cancelled().await;
        for t in tasks {
            t.abort();
        }
    }

    async fn heartbeat_loop(self, shutdown: CancellationToken) {
        let mut every = tokio::time::interval(self.inner.cfg.heartbeat);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = every.tick() => self.heartbeat_once().await,
            }
        }
    }

    /// One heartbeat. Public for tests.
    pub async fn heartbeat_once(&self) {
        if !self.state().host_registered {
            let req = CtlRequest::RegisterHost {
                info: self.inner.cfg.info(),
            };
            if self.hub(&req).await.is_ok() {
                self.state().host_registered = true;
            }
        }
        let refs = self.lease_refs();
        let req = CtlRequest::HostHeartbeat {
            host: self.inner.cfg.name.clone(),
            sessions: refs,
        };
        match self.hub(&req).await {
            Ok(v) => {
                *self.inner.last_ok.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                self.set_paused(false);
                let lost: Vec<SessionId> =
                    serde_json::from_value::<borant_core::proto::HeartbeatReply>(v)
                        .map(|r| r.lost)
                        .unwrap_or_default();
                for sid in lost {
                    tracing::warn!(session = %sid, "session lost its lease");
                    self.fence(
                        sid,
                        "This agent lost its lease. Another host now runs it.".into(),
                        true,
                    )
                    .await;
                }
            }
            Err(e) => {
                self.state().host_registered = false;
                let since = self
                    .inner
                    .last_ok
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .elapsed();
                if since > self.inner.cfg.lease {
                    tracing::warn!("hub unreachable for {since:?}: {e}");
                    self.set_paused(true);
                }
            }
        }
    }

    async fn liveness_loop(self, shutdown: CancellationToken) {
        let mut every = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = every.tick() => self.check_liveness().await,
            }
        }
    }

    async fn outbox_loop(self, shutdown: CancellationToken) {
        let mut every = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = every.tick() => self.flush_outbox().await,
            }
        }
    }
}

impl Host {
    /// Ends local sessions whose harness process exited. Public for tests.
    pub async fn check_liveness(&self) {
        let dead: Vec<SessionId> = self
            .state()
            .sessions
            .values()
            .filter(|s| s.pid.is_some_and(|p| !pid_alive(p)))
            .map(|s| s.id)
            .collect();
        for sid in dead {
            self.end_session(sid, "the harness process exited").await;
        }
    }
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    match kill(Pid::from_raw(raw), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

pub(crate) fn terminate(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    if let Ok(raw) = i32::try_from(pid) {
        let _ = kill(Pid::from_raw(raw), Signal::SIGTERM);
    }
}
