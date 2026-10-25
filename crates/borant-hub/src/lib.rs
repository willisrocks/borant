//! The Borant hub daemon: the only writer of control-plane state.
//!
//! [`Hub::handle`] serves one request at a time behind a mutex, so the rules
//! that span several keys (cycle checks, claims, leases) are safe. The tick
//! takes the same mutex for one lease at a time, never for a whole pass, so a
//! restart on a large store still answers requests.

mod alerts;
mod calls;
pub mod elastic;
mod events;
pub mod export;
mod inbox;
mod leases;
mod members;
mod moves;
mod park;
mod prune;
mod retire;
mod store;
mod tasks;
mod view;
mod waits;

pub use elastic::ElasticConfig;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use borant_core::config::AlertsConfig;
use borant_core::model::{HostStats, HubLease, Lease, MoveReason};
use borant_core::ports::provision::ProvisionerCapabilities;
use borant_core::ports::{EventLog, MessageBus, Notifier, SessionStore, StateStore};
use borant_core::proto::{ApiError, ApiErrorCode, CtlRequest, MoveTarget, Reply, ToolCall};
use borant_core::rules::lease::LeaseConfig;
use borant_core::rules::members::MemberRules;
use borant_core::rules::roles::{CollectionConfig, RoleConfig};
use borant_core::{
    AgentId, BundleName, Clock, CollectionName, FencingToken, HostName, MemberName, PoolName,
    RoleName, SessionId, Timestamp, keys, subjects,
};
use futures::StreamExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::members::Report;
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct HubConfig {
    pub lease: LeaseConfig,
    /// A name for this hub instance, stored in `lease.hub`.
    pub instance: String,
    /// The `[elastic]`, `[provisioners]`, and `[[pools]]` sections of
    /// `hub.toml`. `None`, or no pool, keeps today's hub.
    pub elastic: Option<ElasticConfig>,
    /// The roles from `hub.toml`, for spawns, calls, and registrations.
    pub roles: BTreeMap<RoleName, RoleConfig>,
    /// `[collections.<name>]` from `hub.toml`.
    pub collections: BTreeMap<CollectionName, CollectionConfig>,
    /// `[alerts]` of `hub.toml`: which rules run and their timeouts.
    pub alerts: AlertsConfig,
    /// `[secrets.members]`: the reset time to assume and the restart limit.
    pub member_rules: MemberRules,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            lease: LeaseConfig::default(),
            instance: format!("hub-{}", ulid::Ulid::generate()),
            elastic: None,
            roles: BTreeMap::new(),
            collections: BTreeMap::new(),
            alerts: AlertsConfig::default(),
            member_rules: MemberRules::default(),
        }
    }
}

/// A planned move that waits for the old session to stop.
#[derive(Debug, Clone)]
pub(crate) struct PendingMove {
    pub target: MoveTarget,
    pub reason: MoveReason,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct IdleReport {
    pub session: SessionId,
    pub host: HostName,
    /// The end of the last turn, or `None` while the session is in a turn.
    pub since: Option<Timestamp>,
    /// When the host reported it, on the hub clock.
    pub at: Timestamp,
}

#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// The last heartbeat of each host, on the hub clock.
    pub hosts_seen: HashMap<HostName, Timestamp>,
    /// The idle signal of each agent, from its host's latest heartbeat.
    pub idle: HashMap<AgentId, IdleReport>,
    /// When the hub last sent a park command, per agent.
    pub park_sent: HashMap<AgentId, Timestamp>,
    /// The sessions that the hub already told to stop, so it sends one.
    pub stopped: HashSet<SessionId>,
    /// Retired agents whose work dir still waits for their session to end,
    /// with the host that keeps it.
    pub delete_pending: BTreeMap<AgentId, HostName>,
    pub pending_moves: HashMap<AgentId, PendingMove>,
    /// Hosts where a launch failed, for each agent, until the agent registers.
    pub failed_hosts: HashMap<AgentId, HashSet<HostName>>,
    /// Idle expired agents whose session log was already checked.
    pub idle_checked: HashSet<AgentId>,
    /// When the hub last read the pending messages of an idle agent.
    pub wake_checked: HashMap<AgentId, Timestamp>,
    /// When the lease of each agent first expired, for the claim timeout.
    pub expired_at: HashMap<AgentId, Timestamp>,
    /// Agents whose claims the claim timeout already released.
    pub claims_released: HashSet<AgentId>,
    /// When this hub started serving, for the quiet period after a restart.
    pub started: Option<Timestamp>,
    /// The latest usage report of each bundle member, in memory only.
    pub reports: HashMap<(BundleName, MemberName), Report>,
    /// Bundles whose limited members were already logged once.
    pub limit_warned: HashSet<BundleName>,
    /// The times of the limit restarts of each agent in the last hour, for the
    /// loop guard of the limit restart.
    pub limit_restarts: HashMap<AgentId, Vec<Timestamp>>,
    /// The latest stats of each host, from its last heartbeat.
    pub host_stats: HashMap<HostName, HostStats>,
    /// When the elastic rules last ran.
    pub last_elastic: Option<Timestamp>,
    /// When the alert pass last ran.
    pub last_alerts: Option<Timestamp>,
    /// The provisioner refusals that the alerts report, per pool.
    pub elastic_refused: BTreeMap<PoolName, borant_core::rules::alerts::Refusal>,
    /// When a failed create lets the pool create again.
    pub elastic_backoff: BTreeMap<PoolName, Timestamp>,
    /// The capabilities of each provisioner, read once at the first pass.
    pub elastic_caps: BTreeMap<String, ProvisionerCapabilities>,
    /// The swarm events of the writes in this request or tick step, which are
    /// published after the `mem` guard drops.
    pub outbox: Vec<borant_core::events::SwarmEvent>,
    pub maintenance: bool,
}

#[derive(Debug)]
pub struct Hub {
    pub(crate) store: Store,
    pub(crate) bus: Arc<dyn MessageBus>,
    pub(crate) sessions: Arc<dyn SessionStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) cfg: HubConfig,
    /// The swarm event stream. `None` writes no events, as before phase 2.
    pub(crate) events: Option<Arc<dyn EventLog>>,
    /// The notifier of the hub alerts. `None` keeps the alerts in KV only.
    pub(crate) notifier: Option<Arc<dyn Notifier>>,
    pub(crate) mem: Mutex<Memory>,
}

impl Hub {
    pub fn new(
        state: Arc<dyn StateStore>,
        bus: Arc<dyn MessageBus>,
        sessions: Arc<dyn SessionStore>,
        clock: Arc<dyn Clock>,
        cfg: HubConfig,
    ) -> Self {
        Self {
            store: Store(state),
            bus,
            sessions,
            clock,
            cfg,
            events: None,
            notifier: None,
            mem: Mutex::new(Memory::default()),
        }
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// Handles one request. Requests run one at a time.
    pub async fn handle(&self, req: CtlRequest) -> Reply {
        let mut mem = self.mem.lock().await;
        if mem.maintenance && !matches!(req, CtlRequest::Maintenance { .. }) {
            return Reply::Error(ApiError::unavailable("the hub is in maintenance mode"));
        }
        let req = match req {
            // A prune reads every inbox and session log. `prune_agents` takes
            // `mem` itself, briefly, so the request does not hold it.
            CtlRequest::Call {
                actor,
                key: None,
                call: ToolCall::PruneAgents { older_than_secs },
                ..
            } => {
                drop(mem);
                let result = async {
                    self.authorize(&actor, &ToolCall::PruneAgents { older_than_secs })
                        .await?;
                    prune::prune_agents(self, Duration::from_secs(older_than_secs)).await
                }
                .await;
                if let Err(e) = &result {
                    tracing::debug!("request failed: {e}");
                }
                return result.into();
            }
            // A snapshot reads every key, inbox, and session log.
            // `export::snapshot` takes `mem` itself, for the KV read only.
            CtlRequest::Snapshot { out } => {
                drop(mem);
                let result = match export::snapshot(self, &out).await {
                    Ok(report) => {
                        serde_json::to_value(report).map_err(|e| ApiError::internal(e.to_string()))
                    }
                    Err(e) => Err(ApiError::internal(format!("snapshot: {e:#}"))),
                };
                if let Err(e) = &result {
                    match e.code {
                        ApiErrorCode::Internal | ApiErrorCode::Unavailable => {
                            tracing::warn!("request failed: {e}");
                        }
                        _ => tracing::debug!("request failed: {e}"),
                    }
                }
                return result.into();
            }
            // The live view reads every key and each inbox, so it must not
            // run behind the request mutex. `view` copies the seen times and
            // the stats from memory itself, briefly.
            CtlRequest::Call {
                actor,
                key: None,
                call: ToolCall::View,
                ..
            } => {
                drop(mem);
                let result = async {
                    if let Some(a) = &actor {
                        self.verify_actor(a).await?;
                    }
                    serde_json::to_value(self.view().await?)
                        .map_err(|e| ApiError::internal(e.to_string()))
                }
                .await;
                if let Err(e) = &result {
                    tracing::debug!("request failed: {e}");
                }
                return result.into();
            }
            other => other,
        };
        let result = self.dispatch(&mut mem, req).await;
        // The events of this request wait here, so no publish holds the lock.
        drop(mem);
        self.flush().await;
        if let Err(e) = &result {
            match e.code {
                ApiErrorCode::Internal | ApiErrorCode::Unavailable => {
                    tracing::warn!("request failed: {e}");
                }
                _ => tracing::debug!("request failed: {e}"),
            }
        }
        result.into()
    }

    /// One pass of lease expiry, takeovers, and claim timeouts.
    ///
    /// The pass takes the memory lock for one lease at a time, never for the
    /// whole pass, so requests are served while a restart catches up on a
    /// large store.
    pub async fn tick(&self) -> Result<(), ApiError> {
        if self.mem.lock().await.maintenance {
            return Ok(());
        }
        let leases = self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await?;
        self.tick_leases(leases).await?;
        // The agents that wait for a member of their bundle to reset.
        self.wake_limited_members().await?;
        // The elastic pools park with the idle timeout of each pool; a hub
        // with no pool keeps the one timeout for its hosts.
        let parked = match &self.cfg.elastic {
            Some(elastic) if !elastic.pools.is_empty() => self.tick_elastic().await,
            _ => self
                .park_idle(None, park::DEFAULT_IDLE_TIMEOUT)
                .await
                .map(|_| ()),
        };
        parked?;
        // The waits of the agents, then the alerts that see what the tick
        // changed.
        self.tick_waits().await?;
        self.tick_alerts().await
    }

    /// Takes the hub lease, gives every held agent lease a new expiry, then
    /// serves requests and runs the tick loop until `shutdown`.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) -> anyhow::Result<()> {
        let token = self.take_hub_lease().await?;
        self.restart_grace().await?;
        let mut requests = self
            .bus
            .serve(subjects::CTL)
            .await
            .map_err(|e| anyhow::anyhow!("serve {}: {e}", subjects::CTL))?;
        tracing::info!(instance = %self.cfg.instance, "hub is serving {}", subjects::CTL);

        // Cancelled when another hub takes the hub lease.
        let lost = CancellationToken::new();
        let ticker = {
            let hub = self.clone();
            let shutdown = shutdown.clone();
            let lost = lost.clone();
            tokio::spawn(async move {
                let mut every = tokio::time::interval(Duration::from_secs(1));
                let mut renew = tokio::time::interval(hub.cfg.lease.heartbeat);
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = every.tick() => {
                            if let Err(e) = hub.tick().await {
                                tracing::warn!("tick failed: {e}");
                            }
                        }
                        _ = renew.tick() => {
                            match hub.renew_hub_lease(token).await {
                                Ok(()) => {}
                                Err(e) if e.code == borant_core::proto::ApiErrorCode::StaleToken => {
                                    tracing::error!("stepping down: {}", e.message);
                                    lost.cancel();
                                    break;
                                }
                                Err(e) => tracing::error!("hub lease renewal failed: {e}"),
                            }
                        }
                    }
                }
            })
        };

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = lost.cancelled() => {
                    ticker.abort();
                    anyhow::bail!("another hub took the hub lease");
                }
                next = requests.next() => {
                    let Some(req) = next else { break };
                    let hub = self.clone();
                    tokio::spawn(async move {
                        let reply = match serde_json::from_slice::<CtlRequest>(&req.payload) {
                            Ok(r) => hub.handle(r).await,
                            Err(e) => Reply::Error(ApiError::invalid(format!("bad request: {e}"))),
                        };
                        let bytes = serde_json::to_vec(&reply).unwrap_or_default();
                        if let Err(e) = req.responder.respond(bytes.into()).await {
                            tracing::debug!("reply failed: {e}");
                        }
                    });
                }
            }
        }
        ticker.abort();
        Ok(())
    }

    async fn take_hub_lease(&self) -> anyhow::Result<FencingToken> {
        let now = self.now();
        let key = keys::lease_hub();
        let current = self.store.get::<HubLease>(&key).await?;
        let (token, rev) = match &current {
            Some(d) if d.value.expires > now && d.value.holder != self.cfg.instance => {
                anyhow::bail!(
                    "another hub ({}) holds the hub lease until {}",
                    d.value.holder,
                    d.value.expires
                );
            }
            Some(d) => (FencingToken(d.value.token.0 + 1), Some(d.revision)),
            None => (FencingToken(1), None),
        };
        let lease = HubLease {
            holder: self.cfg.instance.clone(),
            token,
            expires: now.plus(self.cfg.lease.lease),
        };
        self.store.put(&key, &lease, rev).await?;
        Ok(token)
    }

    async fn renew_hub_lease(&self, token: FencingToken) -> Result<(), ApiError> {
        let key = keys::lease_hub();
        let current = self.store.get::<HubLease>(&key).await?;
        if let Some(d) = &current
            && (d.value.holder != self.cfg.instance || d.value.token != token)
        {
            return Err(ApiError::new(
                borant_core::proto::ApiErrorCode::StaleToken,
                format!("hub {} took the hub lease", d.value.holder),
            ));
        }
        let lease = HubLease {
            holder: self.cfg.instance.clone(),
            token,
            expires: self.now().plus(self.cfg.lease.lease),
        };
        self.store
            .put(&key, &lease, current.map(|d| d.revision))
            .await
            .map(|_| ())
    }

    /// Releases the hub lease, for example before a hub move.
    pub async fn release_hub_lease(&self) -> Result<(), ApiError> {
        self.store.delete(&keys::lease_hub()).await
    }
}
