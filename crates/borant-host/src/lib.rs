//! The Borant host daemon. It serves the local API to harness adapters,
//! delivers messages into sessions, writes session logs and checkpoints, and
//! runs launch orders from the hub.

pub mod api;
pub mod checkpoint;
pub mod client;
mod commands;
mod delivery;
mod hub_down;
pub mod outbox;
mod persist;
pub mod sandbox;
mod secrets;
mod sessions;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borant_core::model::{Envelope, Harness, HostInfo, HostStats, MemberBundle, SessionStats};
use borant_core::ports::secrets::{Bundle, BundleName, MemberName, SecretStore};
use borant_core::ports::{
    EventLog, LaunchHandle, Launcher, MessageBus, Notice, Notifier, Priority, SessionStore,
};
use borant_core::proto::{ApiError, CtlRequest, Registered, Reply};
use borant_core::rules::redact::redact;
use borant_core::rules::wake::WakeLimiter;
use borant_core::{
    AgentId, ChannelName, Clock, CollectionName, HostName, MessageId, PoolName, RoleName,
    SessionId, Tag, Timestamp, subjects,
};
use borant_launch::harness;
use borant_launch::harness::HarnessCommands;
use borant_secrets::{StoreConfig, open_store};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::checkpoint::RepoFacts;
use crate::hub_down::{HubAlert, HubDownWatch};
use crate::outbox::Outbox;
pub use crate::sandbox::SandboxConfig;
use crate::secrets::{LaunchSecrets, Redactor};

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
    /// The memory that this host keeps free for itself, from `[host]
    /// reserve_mem_mb`. A placement below it is refused with `LowMemory`.
    pub reserve_mem_mb: u32,
    /// The pool that created this host. `None` for a static host.
    pub pool: Option<PoolName>,
    /// The tags of this host, for example `vps` for a pool host.
    pub tags: Vec<Tag>,
    pub ssh: Option<String>,
    /// `[tmux] socket`: the tmux server of this host when its launcher is
    /// `tmux`, for `borant attach`.
    pub tmux_socket: Option<String>,
    pub harnesses: Vec<Harness>,
    pub commands: HarnessCommands,
    /// Where checkpoints go. Defaults to the `origin` remote of each repo.
    pub checkpoint_remote: Option<String>,
    pub lease: Duration,
    pub heartbeat: Duration,
    pub wakes_per_hour: u32,
    pub keep_turns: usize,
    /// How long a launch may take to register before the watchdog stops it
    /// and tells the hub, `launch_timeout_secs` in `host.toml`.
    pub launch_timeout: Duration,
    pub notify: bool,
    pub hub_timeout: Duration,
    /// The hard limit of one inbox pull. A broker request that a dead
    /// connection leaves unanswered must not stop the inbox loop.
    pub inbox_pull_timeout: Duration,
    /// `[alerts] watch_hub`: send a notice when the hub stays unreachable.
    pub watch_hub: bool,
    /// `[alerts] hub_down_minutes`: how long the heartbeats must fail first.
    pub hub_down: Duration,
    /// The local API socket, if not `{home}/host.sock`.
    pub socket: Option<PathBuf>,
    /// `[sandbox]` of `host.toml`, when this host offers one. The start check
    /// clears it if the program cannot sandbox.
    pub sandbox: Option<SandboxConfig>,
    /// The opened secret stores and bundles, from `[secrets]` in `host.toml`.
    pub secrets: Option<HostSecrets>,
    /// Where the secret directories go: `/run/borant/agents` on Linux and
    /// `$TMPDIR/borant/agents` on macOS. Tests set it.
    pub secrets_root: Option<PathBuf>,
}

impl HostConfig {
    pub fn new(name: HostName, home: PathBuf) -> Self {
        Self {
            name,
            home,
            launcher: borant_launch::DEFAULT_LAUNCHER.into(),
            max_agents: 4,
            always_on: false,
            reserve_mem_mb: 1024,
            pool: None,
            tags: Vec::new(),
            ssh: None,
            tmux_socket: None,
            harnesses: vec![Harness::ClaudeCode, Harness::Pi, Harness::Omp],
            commands: HarnessCommands::default(),
            checkpoint_remote: None,
            lease: Duration::from_secs(30),
            heartbeat: Duration::from_secs(10),
            wakes_per_hour: 30,
            keep_turns: 50,
            launch_timeout: Duration::from_secs(90),
            notify: false,
            hub_timeout: Duration::from_secs(5),
            inbox_pull_timeout: Duration::from_secs(12),
            watch_hub: false,
            hub_down: Duration::from_secs(120),
            socket: None,
            sandbox: None,
            secrets: None,
            secrets_root: None,
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.home.join("host.sock"))
    }

    /// The record of this host with its declared `harnesses`. The daemon
    /// announces what it can run, not what `host.toml` names: [`Host::offered_info`]
    /// drops the harnesses whose program is missing.
    pub fn info(&self) -> HostInfo {
        self.info_with(self.harnesses.clone())
    }

    /// The record of this host with `harnesses` instead of the declared ones.
    pub fn info_with(&self, harnesses: Vec<Harness>) -> HostInfo {
        let members = self
            .secrets
            .as_ref()
            .map(|s| s.members(&harnesses))
            .unwrap_or_default();
        HostInfo {
            name: self.name.clone(),
            harnesses,
            launcher: self.launcher.clone(),
            max_agents: self.max_agents,
            always_on: self.always_on,
            draining: false,
            ssh: self.ssh.clone(),
            tmux_socket: self.tmux_socket.clone(),
            members,
            pool: self.pool.clone(),
            tags: self.tags.clone(),
            bundles: self
                .secrets
                .as_ref()
                .map(|s| s.bundles.keys().cloned().collect())
                .unwrap_or_default(),
            reserve_mem_mb: Some(self.reserve_mem_mb),
            // The sandbox kind is what `[sandbox]` enables, once the start
            // check passed.
            sandboxes: match self.sandbox {
                Some(_) => vec![borant_core::model::SandboxKind::Namespace],
                None => Vec::new(),
            },
        }
    }
}

/// The `[secrets]` section of `host.toml`, before the stores open.
#[derive(Debug, Clone, Default)]
pub struct SecretConfig {
    /// `[secrets.stores.<name>]`.
    pub stores: BTreeMap<String, StoreConfig>,
    /// `[secrets.bundles.<name>]`, with the table key as the bundle name.
    pub bundles: BTreeMap<BundleName, Bundle>,
    /// `[secrets.harness]`: `"*"` or a harness name, each with its default
    /// bundles.
    pub harness: BTreeMap<String, Vec<BundleName>>,
    /// The folder of the credential file of an exec plugin, when the service
    /// manager gives one.
    pub credentials_dir: Option<PathBuf>,
}

/// The opened secret stores of this host, with the bundles that name them.
#[derive(Debug, Clone)]
pub struct HostSecrets {
    /// The stores that opened. A store that failed is not here.
    pub stores: BTreeMap<String, Arc<dyn SecretStore>>,
    pub bundles: BTreeMap<BundleName, Bundle>,
    pub harness: BTreeMap<String, Vec<BundleName>>,
}

impl SecretConfig {
    /// Opens every store at host start. A store that fails is logged, and every
    /// harness whose default bundles use it is left out, so a host never tries
    /// to launch an agent whose token it cannot read.
    pub async fn open(&self, harnesses: &[Harness]) -> (HostSecrets, Vec<Harness>) {
        let mut stores = BTreeMap::new();
        let mut failed = Vec::new();
        for (name, config) in &self.stores {
            match open_store(config, self.credentials_dir.as_deref()).await {
                Ok(store) => {
                    stores.insert(name.clone(), store);
                }
                Err(e) => {
                    tracing::error!(store = %name, "the secret store did not open: {e}");
                    failed.push(name.clone());
                }
            }
        }
        let secrets = HostSecrets {
            stores,
            bundles: self.bundles.clone(),
            harness: self.harness.clone(),
        };
        let keep = harnesses
            .iter()
            .copied()
            .filter(|harness| {
                !secrets.bundles_for(*harness).iter().any(|bundle| {
                    let store = &secrets.bundles[bundle].store;
                    failed.iter().any(|f| f == store)
                })
            })
            .collect();
        (secrets, keep)
    }
}

impl HostSecrets {
    /// The bundles of a harness: the `"*"` defaults first, then its own, with
    /// bundles that `host.toml` does not define left out.
    pub fn bundles_for(&self, harness: Harness) -> Vec<BundleName> {
        let mut out: Vec<BundleName> = Vec::new();
        let star = self.harness.get("*").cloned().unwrap_or_default();
        let own = self
            .harness
            .get(harness.as_str())
            .cloned()
            .unwrap_or_default();
        for bundle in star.into_iter().chain(own) {
            if self.bundles.contains_key(&bundle) && !out.contains(&bundle) {
                out.push(bundle);
            }
        }
        out
    }

    /// The bundles of one launch, in order: the harness defaults, then the
    /// bundles of the order, so a later bundle wins on the same variable. A
    /// bundle that `host.toml` does not define is left out.
    pub fn launch_bundles(&self, harness: Harness, bundles: &[BundleName]) -> Vec<BundleName> {
        let mut out = self.bundles_for(harness);
        for bundle in bundles {
            if !self.bundles.contains_key(bundle) {
                continue;
            }
            out.retain(|b| b != bundle);
            out.push(bundle.clone());
        }
        out
    }

    /// The bundles with two or more members, and the harnesses that get them:
    /// what the hub needs to pick a member at each launch.
    pub fn members(&self, harnesses: &[Harness]) -> BTreeMap<BundleName, MemberBundle> {
        let mut out = BTreeMap::new();
        for (name, bundle) in &self.bundles {
            if bundle.members.len() < 2 {
                continue;
            }
            let holders: Vec<Harness> = harnesses
                .iter()
                .copied()
                .filter(|harness| self.bundles_for(*harness).contains(name))
                .collect();
            if holders.is_empty() {
                continue;
            }
            out.insert(
                name.clone(),
                MemberBundle {
                    members: bundle.members.keys().cloned().collect(),
                    harnesses: holders,
                },
            );
        }
        out
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
    /// The role that a person started the session with, from `BORANT_ROLE`.
    pub role_hint: Option<RoleName>,
    /// The collection of the session, from `BORANT_COLLECTION`.
    pub collection_hint: Option<CollectionName>,
    pub repo: Option<RepoFacts>,
    pub shared_dir: bool,
    pub reg: Option<Registered>,
    pub queue: VecDeque<Queued>,
    pub notify: Arc<Notify>,
    pub turn: u64,
    pub busy: bool,
    /// The end of the last turn, or `None` while the session is in one. The
    /// hub parks sessions that stay idle past the pool timeout.
    pub idle_since: Option<Timestamp>,
    /// When this session wrote its last event, for the stall alert.
    pub last_event_at: Option<Timestamp>,
    /// Set when the session lost its lease. Tools stay blocked.
    pub fenced: Option<String>,
    /// Set while the hub is unreachable for longer than a lease.
    pub paused: bool,
    /// Set when a pause must tell this session that the hub is back.
    pub wake_pending: bool,
    pub moving: bool,
    pub last_tree: Option<String>,
    pub checkpointing: bool,
    pub dirty: bool,
    pub launch: Option<LaunchHandle>,
    pub cancel: CancellationToken,
    /// The bundles that the launch resolved: names only.
    pub bundles: Vec<BundleName>,
    /// The member of each member bundle that the launch used.
    pub members: BTreeMap<BundleName, MemberName>,
    /// The secret directory of the session, when it runs through the shim.
    pub secrets_dir: Option<PathBuf>,
}

impl LocalSess {
    pub fn movable(&self) -> bool {
        self.reg.as_ref().is_some_and(|r| r.movable) && self.repo.is_some()
    }

    /// The agent of the session, from its lease or from the registration
    /// hint.
    pub fn agent(&self) -> Option<AgentId> {
        self.reg
            .as_ref()
            .map(|r| r.agent.clone())
            .or_else(|| self.agent_hint.clone())
    }
}

/// One launch that waits for its session to register. After
/// `launch_timeout`, the watchdog stops it and tells the hub why, with the
/// end of its screen.
#[derive(Debug, Clone)]
pub struct PendingLaunch {
    pub handle: LaunchHandle,
    pub started: Instant,
}

#[derive(Debug)]
pub(crate) struct State {
    pub sessions: HashMap<SessionId, LocalSess>,
    pub limiter: WakeLimiter,
    pub limit_noticed: HashSet<AgentId>,
    /// The launches that wait for their session to register, with the start
    /// that the watchdog measures.
    pub pending_launch: HashMap<AgentId, PendingLaunch>,
    /// The last launch of each agent, so a relaunch kills a stale session
    /// before it starts a second one.
    pub last_launch: HashMap<AgentId, LaunchHandle>,
    /// Agents whose launch runs now.
    pub launching: HashSet<AgentId>,
    pub seen: HashMap<AgentId, HashSet<MessageId>>,
    pub turns_since_trim: HashMap<AgentId, usize>,
    pub host_registered: bool,
    /// The harnesses of the last registration, so a program that appears or
    /// goes away registers the host again on the next heartbeat.
    pub registered_harnesses: Option<Vec<Harness>>,
    /// The harnesses whose program was missing at the last check, so one
    /// warning covers a whole absence.
    pub missing_programs: HashSet<Harness>,
    /// Start of the current run of successful heartbeats.
    pub up_since: Option<Instant>,
    /// True when the hub was reachable for a lease, so a pause is a new
    /// outage and wakes its sessions.
    pub outage_over: bool,
    /// The `hub.unreachable` window, when `[alerts] watch_hub` is on.
    pub hub_down: HubDownWatch,
    /// False until the saved sessions are loaded. Until then, an unknown
    /// session can still be one that the restore brings back.
    pub restored: bool,
    /// The start of the minute and its count for each rate-limited event of
    /// an agent and kind, for the `delivery.*` events.
    pub event_limited: HashMap<(AgentId, &'static str), (Timestamp, u64)>,
    /// What the last launch of each agent issued, until its session attaches.
    pub launched: HashMap<AgentId, Arc<LaunchSecrets>>,
    /// The exact values of each agent, for the redactor of the session log.
    pub redactors: HashMap<AgentId, Arc<Redactor>>,
}

/// The free space of the disk that holds `dir`, in megabytes.
fn disk_avail_mb(dir: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(&Path, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if dir.starts_with(mount)
            && best.is_none_or(|(found, _)| mount.as_os_str().len() > found.as_os_str().len())
        {
            best = Some((mount, disk.available_space()));
        }
    }
    best.map(|(_, bytes)| bytes / (1024 * 1024))
}

pub(crate) struct Inner {
    pub cfg: HostConfig,
    pub bus: Arc<dyn MessageBus>,
    pub store: Arc<dyn SessionStore>,
    pub clock: Arc<dyn Clock>,
    pub launcher: Box<dyn Launcher>,
    pub notifier: Option<Box<dyn Notifier>>,
    /// The swarm event stream, when the daemon has one. Without it the host
    /// emits no events and `/v1/events` answers `unavailable`.
    pub events: Option<Arc<dyn EventLog>>,
    pub outbox: Outbox,
    pub state: Mutex<State>,
    pub last_ok: Mutex<Instant>,
    /// When the daemon started, for `HostStats`.
    pub started: Timestamp,
    /// The round trip of the previous heartbeat, in milliseconds.
    pub last_rtt: Mutex<Option<u64>>,
    /// True while heartbeats fail, so a run of failures logs one `warn`.
    pub heartbeat_failing: AtomicBool,
    /// True while outbox flushes fail, so a run of failures logs one `warn`.
    pub outbox_failing: AtomicBool,
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
    /// Builds a host from the ports of its daemon.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: HostConfig,
        bus: Arc<dyn MessageBus>,
        store: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
        launcher: Box<dyn Launcher>,
        notifier: Option<Box<dyn Notifier>>,
        events: Option<Arc<dyn EventLog>>,
        outbox: Outbox,
    ) -> Self {
        let wakes = cfg.wakes_per_hour;
        let started = clock.now();
        let hub_down = HubDownWatch::new(cfg.hub_down, cfg.heartbeat);
        Self {
            inner: Arc::new(Inner {
                cfg,
                bus,
                store,
                clock,
                launcher,
                notifier,
                events,
                outbox,
                state: Mutex::new(State {
                    sessions: HashMap::new(),
                    limiter: WakeLimiter::new(wakes),
                    limit_noticed: HashSet::new(),
                    pending_launch: HashMap::new(),
                    last_launch: HashMap::new(),
                    launching: HashSet::new(),
                    seen: HashMap::new(),
                    turns_since_trim: HashMap::new(),
                    host_registered: false,
                    registered_harnesses: None,
                    missing_programs: HashSet::new(),
                    up_since: None,
                    outage_over: true,
                    hub_down,
                    restored: false,
                    event_limited: HashMap::new(),
                    launched: HashMap::new(),
                    redactors: HashMap::new(),
                }),
                last_ok: Mutex::new(Instant::now()),
                started,
                last_rtt: Mutex::new(None),
                heartbeat_failing: AtomicBool::new(false),
                outbox_failing: AtomicBool::new(false),
            }),
        }
    }

    pub fn config(&self) -> &HostConfig {
        &self.inner.cfg
    }

    /// The harnesses that this host can run: the declared ones whose program
    /// it finds, on the `PATH` of the daemon or in an install dir of a launch.
    /// The check runs again on every call, so a program that a person installs
    /// while the daemon runs is offered from the next heartbeat on. One
    /// warning covers an absence: it comes again only after the program was
    /// found in between.
    pub fn offered_harnesses(&self) -> Vec<Harness> {
        let cfg = &self.inner.cfg;
        let dirs = harness::search_dirs();
        let mut offered = Vec::with_capacity(cfg.harnesses.len());
        let mut missing = Vec::new();
        for h in cfg.harnesses.iter().copied() {
            let program = harness::program(h, &cfg.commands);
            if harness::find_program(&dirs, &program).is_some() {
                offered.push(h);
            } else {
                missing.push((h, program));
            }
        }
        let now_missing: HashSet<Harness> = missing.iter().map(|(h, _)| *h).collect();
        {
            let mut st = self.state();
            for (h, program) in &missing {
                if st.missing_programs.insert(*h) {
                    tracing::warn!(
                        harness = %h,
                        program = %program,
                        "the program of the harness is not installed, so the host does not offer the harness"
                    );
                }
            }
            st.missing_programs.retain(|h| now_missing.contains(h));
        }
        offered
    }

    /// The registration record of this host: what it can run now, not what
    /// `host.toml` declares.
    pub fn offered_info(&self) -> HostInfo {
        self.inner.cfg.info_with(self.offered_harnesses())
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
        self.restore().await;
        // A directory whose session ended while the daemon was down keeps
        // values on disk: no session will ever read it.
        self.remove_stale_secret_dirs().await;
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

    /// The load and counters of this host, for the heartbeat and the live
    /// view.
    fn stats(&self) -> HostStats {
        let load = sysinfo::System::load_average();
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        let total = system.total_memory();
        let mem_used_pct = if total == 0 {
            0.0
        } else {
            (system.used_memory() as f64 / total as f64) * 100.0
        };
        let cpus = std::thread::available_parallelism()
            .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .unwrap_or(0);
        let outbox_depth = self
            .inner
            .outbox
            .len()
            .map(|n| u64::try_from(n).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let last_heartbeat_rtt_ms = *self
            .inner
            .last_rtt
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sessions = {
            let st = self.state();
            st.sessions
                .values()
                .filter_map(|s| {
                    let reg = s.reg.as_ref()?;
                    // The counters of a session arrive with their own task;
                    // there is no source for them yet.
                    Some(SessionStats {
                        agent: reg.agent.clone(),
                        busy: s.busy,
                        paused: s.paused,
                        turn: s.turn,
                        last_event_at: s.last_event_at,
                        wakes: 0,
                        wake_limited: 0,
                        ack_failed: 0,
                        append_failed: 0,
                        redelivered: 0,
                        bad_envelope: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cost_usd: 0.0,
                    })
                })
                .collect()
        };
        HostStats {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            started: Some(self.inner.started),
            load1: load.one,
            cpus,
            mem_used_pct,
            mem_avail_mb: Some(system.available_memory() / (1024 * 1024)),
            disk_avail_mb: disk_avail_mb(&self.inner.cfg.home),
            disk_used_pct: disk_used_pct(&self.inner.cfg.home),
            outbox_depth,
            last_heartbeat_rtt_ms,
            // The bus port has no reconnect counter yet.
            nats_reconnects: 0,
            sessions,
        }
    }

    /// One heartbeat. Public for tests.
    pub async fn heartbeat_once(&self) {
        // The offered harnesses come from the disk on every beat, so a program
        // that a person installs or removes while the daemon runs changes what
        // the hub places here: the registration below follows the change.
        let info = self.offered_info();
        let changed = self.state().registered_harnesses.as_ref() != Some(&info.harnesses);
        if !self.state().host_registered || changed {
            let harnesses = info.harnesses.clone();
            let req = CtlRequest::RegisterHost { info };
            match self.hub(&req).await {
                Ok(_) => {
                    let mut st = self.state();
                    st.host_registered = true;
                    st.registered_harnesses = Some(harnesses);
                }
                Err(e) => self.heartbeat_failure("cannot register the host", &e),
            }
        }
        let refs = self.lease_refs();
        let req = CtlRequest::HostHeartbeat {
            host: self.inner.cfg.name.clone(),
            sessions: refs,
            stats: Some(self.stats()),
        };
        let sent = Instant::now();
        let reply = self.hub(&req).await;
        if reply.is_ok() {
            let rtt = u64::try_from(sent.elapsed().as_millis()).unwrap_or(u64::MAX);
            *self
                .inner
                .last_rtt
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(rtt);
        }
        match reply {
            Ok(v) => {
                *self.inner.last_ok.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                let stable = self.mark_hub_up();
                let reply = serde_json::from_value::<borant_core::proto::HeartbeatReply>(v)
                    .unwrap_or_default();
                let lost = reply.lost;
                // Store the new tokens before the pause ends, so the next
                // tool call and heartbeat use them.
                self.apply_renewed(&reply.renewed);
                // Fence the lost sessions before the pause ends, so no lost
                // session runs a tool in between.
                for sid in &lost {
                    tracing::warn!(session = %sid, "session lost its lease");
                    self.mark_fenced(
                        *sid,
                        "This agent lost its lease. Another host now runs it.".into(),
                    );
                }
                self.set_paused(false);
                // Saving the stale work can take a network push, so it runs
                // outside the heartbeat loop.
                for sid in lost {
                    let host = self.clone();
                    tokio::spawn(async move { host.stop_fenced(sid).await });
                }
                // A hub that flaps during its restart must not wake a session
                // into paused tools again: wait for a stable recovery.
                if stable {
                    self.wake_unpaused().await;
                }
                self.heartbeat_recovered();
                if let Some(alert) = self.beat_recovered() {
                    self.send_hub_alert(alert).await;
                }
            }
            Err(e) => {
                self.state().host_registered = false;
                self.mark_hub_down();
                let since = self
                    .inner
                    .last_ok
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .elapsed();
                self.heartbeat_failure(&format!("hub unreachable for {since:?}"), &e);
                if since > self.inner.cfg.lease {
                    self.set_paused(true);
                }
                if let Some(alert) = self.beat_failed() {
                    self.send_hub_alert(alert).await;
                }
            }
        }
    }

    /// One failed heartbeat in the `hub.unreachable` window, when `[alerts]
    /// watch_hub` is on. The state lock is gone before the caller awaits the
    /// notifier.
    fn beat_failed(&self) -> Option<HubAlert> {
        if !self.inner.cfg.watch_hub {
            return None;
        }
        let now = self.inner.clock.now();
        let alert = self.state().hub_down.failed(now);
        if let Some(alert) = alert {
            tracing::warn!(?alert, "the hub stays unreachable");
        }
        alert
    }

    /// One heartbeat that got through, in the `hub.unreachable` window.
    fn beat_recovered(&self) -> Option<HubAlert> {
        if !self.inner.cfg.watch_hub {
            return None;
        }
        let now = self.inner.clock.now();
        self.state().hub_down.recovered(now)
    }

    /// Tells the person through the notifier of this host. The stderr of a
    /// notifier is not a secret, and a failed notice is a warning, not a
    /// reason to stop the loop.
    async fn send_hub_alert(&self, alert: HubAlert) {
        let Some(notifier) = &self.inner.notifier else {
            tracing::warn!(?alert, "watch_hub is on but this host has no notifier");
            return;
        };
        let notice = match alert {
            HubAlert::Down { minutes } => {
                let text = format!("Borant hub unreachable for {minutes} minutes");
                Notice::new(
                    text,
                    "Heartbeats to the Borant hub keep failing. Check the hub and its NATS broker.",
                )
                .with_priority(Priority::Urgent)
                .with_tags(["hub".to_owned(), "hub.unreachable".to_owned()])
            }
            HubAlert::Back => Notice::new(
                "Borant hub is back",
                "The Borant hub answers heartbeats again.",
            ),
        };
        if let Err(e) = notifier.notify(&notice).await {
            tracing::warn!("cannot send the hub notice: {e}");
        }
    }

    /// Notes one successful heartbeat. Returns true when the hub has stayed
    /// reachable for a lease, so the recovery is stable and the sessions it
    /// paused can be woken.
    fn mark_hub_up(&self) -> bool {
        let mut st = self.state();
        let since = *st.up_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= self.inner.cfg.lease {
            st.outage_over = true;
        }
        st.outage_over
    }

    fn mark_hub_down(&self) {
        self.state().up_since = None;
    }

    /// Logs the first failure of a run at `warn`, and later ones at `debug`.
    fn heartbeat_failure(&self, what: &str, e: &ApiError) {
        if self.inner.heartbeat_failing.swap(true, Ordering::Relaxed) {
            tracing::debug!("{what}: {e}");
        } else {
            tracing::warn!("{what}: {e}");
        }
    }

    fn heartbeat_recovered(&self) {
        if self.inner.heartbeat_failing.swap(false, Ordering::Relaxed) {
            tracing::info!("hub reachable again");
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
    /// Ends local sessions whose harness process exited, and stops launches
    /// that did not register in time. Public for tests.
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
        self.check_launch_timeouts().await;
    }

    /// Stops every launch that has not registered within `launch_timeout`,
    /// and tells the hub with the end of its screen. A launch that sits on a
    /// first-run screen, such as a folder-trust prompt, never registers, and
    /// a relaunch without this text repeats the same wait. Public for tests.
    pub async fn check_launch_timeouts(&self) {
        let timeout = self.inner.cfg.launch_timeout;
        let late: Vec<(AgentId, LaunchHandle)> = self
            .state()
            .pending_launch
            .iter()
            .filter(|(_, p)| p.started.elapsed() >= timeout)
            .map(|(agent, p)| (agent.clone(), p.handle.clone()))
            .collect();
        for (agent, handle) in late {
            // The screen before the stop, redacted: a sticky prompt can hold a
            // value that the harness printed.
            let screen = self
                .inner
                .launcher
                .capture(&handle, WATCHDOG_LINES)
                .map(|text| redact(&text).into_owned())
                .unwrap_or_default();
            tracing::warn!(
                %agent,
                elapsed = ?timeout,
                "a launch did not register in time, stopping it"
            );
            if let Err(e) = self.inner.launcher.stop(&handle).await {
                tracing::warn!(%agent, "cannot stop a launch that did not register: {e}");
            }
            self.state().pending_launch.remove(&agent);
            // Values that no session will read go with the launch.
            self.forget_secrets(&agent, None).await;
            let req = CtlRequest::LaunchFailed {
                agent: agent.clone(),
                host: self.inner.cfg.name.clone(),
                error: screen,
            };
            if let Err(e) = self.hub(&req).await {
                tracing::warn!(
                    %agent,
                    "cannot tell the hub about a stuck launch: {}",
                    e.message
                );
            }
        }
    }
}

/// How many lines of a stuck screen the watchdog keeps.
const WATCHDOG_LINES: usize = 40;

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

/// How full the file system that holds `path` is, as a percentage, from the
/// longest mount point that contains it. `None` when no disk matches, so an
/// unknown disk passes every rule that reads it.
pub fn disk_used_pct(path: &Path) -> Option<f64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())?;
    let total = disk.total_space();
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(disk.available_space());
    Some((used as f64 / total as f64) * 100.0)
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    /// A host with an in-memory bus and store, and a real clock.
    fn new_host(reserve_mem_mb: Option<u32>) -> (Host, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
        cfg.harnesses = vec![Harness::Fake];
        if let Some(v) = reserve_mem_mb {
            cfg.reserve_mem_mb = v;
        }
        let host = Host::new(
            cfg,
            Arc::new(borant_memory::MemoryBus::new()),
            Arc::new(borant_memory::MemorySession::new()),
            Arc::new(borant_core::SystemClock),
            Box::new(borant_launch::ProcessLauncher::new(dir.path().join("logs"))),
            None,
            None,
            Outbox::in_memory().unwrap(),
        );
        (host, dir)
    }

    #[test]
    fn stats_report_the_memory_and_the_disk_that_are_free() {
        let (host, _dir) = new_host(None);
        let stats = host.stats();
        let mem = stats.mem_avail_mb.expect("the free memory of the host");
        assert!(mem > 0, "{mem} MB free");
        let disk = stats.disk_avail_mb.expect("the free disk of BORANT_HOME");
        assert!(disk > 0, "{disk} MB free");
        // The disk guard reads this one.
        let used = stats.disk_used_pct.expect("how full the disk is");
        assert!((0.0..=100.0).contains(&used), "{used}% used");
    }

    #[test]
    fn stats_carry_the_memory_reserve_of_the_host() {
        let (host, _dir) = new_host(None);
        assert_eq!(host.config().reserve_mem_mb, 1024, "the default reserve");
        assert_eq!(host.config().info().reserve_mem_mb, Some(1024));
        let (big, _dir) = new_host(Some(4096));
        assert_eq!(big.config().info().reserve_mem_mb, Some(4096));
    }
}
