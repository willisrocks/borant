//! The elastic executor: one pass per `elastic.tick`. It gathers the inputs
//! of [`rules::pool::plan`], runs it, and executes the actions on the
//! provisioner and on the `elastic.*` keys.
//!
//! The pass never holds the `mem` mutex across a provisioner call, because a
//! create can take two minutes. It reads `mem` briefly at the start, drops it
//! for the provider ops, and takes it again for the steps that need it.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, Harness, HostInfo, Lease, LeaseState, SpawnRecord, SpawnState};
use borant_core::ports::provision::{
    HostSpec, ProviderHostId, Provisioner, ProvisionerCapabilities,
};
use borant_core::ports::{PortError, PortErrorKind};
use borant_core::proto::ApiError;
use borant_core::rules::alerts::Refusal;
use borant_core::rules::pool::{
    self, Action, ElasticHost, Phase, PlacementNeed, PoolConfig, Spend,
};
use borant_core::rules::schedule::{self, HostView};
use borant_core::{AgentId, HostName, PoolName, Timestamp, keys};

use crate::join::JoinToken;
use crate::secrets::host_boot;
use crate::{Hub, PendingMove};

/// The `[elastic]`, `[provisioners]`, and `[[pools]]` sections of `hub.toml`,
/// with the adapter that the binary opened for each provisioner.
#[derive(Debug, Clone)]
pub struct ElasticConfig {
    /// How often the rules run.
    pub tick: Duration,
    /// How long a host may go without a heartbeat before it is lost. Each
    /// pool carries it, because `plan` takes no separate value.
    pub lost_timeout: Duration,
    /// How long a pool waits after a failed create.
    pub create_backoff: Duration,
    /// The cap of the month, all pools together.
    pub monthly_budget_usd: Option<f64>,
    pub pools: Vec<PoolConfig>,
    /// The backend of each `[provisioners.<name>]` entry, by its name.
    pub provisioners: BTreeMap<String, Arc<dyn Provisioner>>,
}

/// What one pass needs from the store, gathered before any provisioner call.
struct Pass {
    pools: Vec<PoolConfig>,
    hosts: Vec<ElasticHost>,
    live: Vec<HostView>,
    unplaced: Vec<PlacementNeed>,
    records: BTreeMap<PoolName, Vec<borant_core::ports::provision::HostRecord>>,
    spend: Spend,
}

impl Hub {
    /// Runs the elastic rules when their tick has come. A hub with no pool
    /// does nothing here.
    pub async fn tick_elastic(&self) -> Result<(), ApiError> {
        let Some(elastic) = self.cfg.elastic.clone() else {
            return Ok(());
        };
        if elastic.pools.is_empty() {
            return Ok(());
        }
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_elastic
                .is_some_and(|last| now.since(last) < elastic.tick)
            {
                return Ok(());
            }
            mem.last_elastic = Some(now);
        }
        if let Err(e) = self.elastic_pass(&elastic, now).await {
            tracing::warn!("elastic pass failed: {e}");
        }
        Ok(())
    }

    async fn elastic_pass(&self, elastic: &ElasticConfig, now: Timestamp) -> Result<(), ApiError> {
        let caps = self.capabilities(elastic).await;
        let pools = self.pools(elastic).await;
        let mut pass = self.gather(&pools, &caps, now).await?;
        let actions = pool::plan(
            &pass.pools,
            &pass.hosts,
            &pass.live,
            &pass.unplaced,
            &pass.records,
            &pass.spend,
            now,
        );
        if !actions.is_empty() {
            tracing::debug!(n = actions.len(), "elastic actions");
        }
        for action in actions {
            self.execute(elastic, &mut pass, &caps, action, now).await?;
        }
        self.write_spend(&pass.spend, now).await?;
        // A create that left no host behind leaves an unspent token, and the
        // pool's `boot_timeout` bounds it.
        if let Err(e) = self.reap_joins(now).await {
            tracing::warn!("the join reap failed: {e}");
        }
        // The drain and the park of a pool host queue events while they take
        // `mem`; publish them now that no lock is held.
        self.flush().await;
        // Free the pool hosts first: an idle host goes only with no agent.
        for pool in &pass.pools {
            if let Err(e) = self.park_idle(Some(&pool.name), pool.idle_timeout).await {
                tracing::warn!(pool = %pool.name, "park failed: {e}");
            }
        }
        Ok(())
    }

    /// The capability of each provisioner, read once and kept in memory.
    async fn capabilities(
        &self,
        elastic: &ElasticConfig,
    ) -> BTreeMap<String, ProvisionerCapabilities> {
        let mut caps = self.mem.lock().await.elastic_caps.clone();
        let mut changed = false;
        for (name, provisioner) in &elastic.provisioners {
            if caps.contains_key(name) {
                continue;
            }
            match provisioner.describe().await {
                Ok(described) => {
                    caps.insert(name.clone(), described);
                    changed = true;
                }
                Err(e) => tracing::warn!(provisioner = %name, "describe failed: {e}"),
            }
        }
        if changed {
            self.mem.lock().await.elastic_caps = caps.clone();
        }
        caps
    }

    /// The configured pools, with the create backoff of each from memory.
    async fn pools(&self, elastic: &ElasticConfig) -> Vec<PoolConfig> {
        let backoff = self.mem.lock().await.elastic_backoff.clone();
        elastic
            .pools
            .iter()
            .map(|p| PoolConfig {
                backoff_until: backoff.get(&p.name).copied(),
                ..p.clone()
            })
            .collect()
    }

    /// Reads the inputs of `plan` and keeps the phases that the hub maintains
    /// itself in step: the heartbeat, the busy time, and Booting to Ready.
    async fn gather(
        &self,
        pools: &[PoolConfig],
        caps: &BTreeMap<String, ProvisionerCapabilities>,
        now: Timestamp,
    ) -> Result<Pass, ApiError> {
        let (pending, seen, paused) = {
            let mem = self.mem.lock().await;
            let pending: Vec<(AgentId, PendingMove)> = mem
                .pending_moves
                .iter()
                .map(|(id, move_)| (id.clone(), move_.clone()))
                .collect();
            // A session that the host paused during an outage is waiting,
            // not lost: no pool host goes up for it.
            let paused: HashSet<AgentId> = mem
                .host_stats
                .values()
                .flat_map(|stats| stats.sessions.iter())
                .filter(|s| s.paused)
                .map(|s| s.agent.clone())
                .collect();
            (pending, mem.hosts_seen.clone(), paused)
        };
        let live = {
            let mem = self.mem.lock().await;
            self.views(&mem, None).await?
        };

        let mut hosts = Vec::new();
        for doc in self.store.list::<ElasticHost>(keys::ELASTIC_PREFIX).await? {
            let mut host = doc.value;
            if !pools.iter().any(|p| p.name == host.pool) {
                // A pool that left the config: its key goes.
                self.store.delete(&doc.key).await?;
                continue;
            }
            let view = live.iter().find(|v| v.info.name == host.name);
            let mut changed = false;
            if let Some(at) = seen.get(&host.name)
                && host.last_seen != *at
            {
                host.last_seen = *at;
                changed = true;
            }
            if view.is_some_and(|v| v.active_agents > 0) && host.last_busy != now {
                host.last_busy = now;
                changed = true;
            }
            if host.phase == Phase::Booting && view.is_some_and(|v| v.online) {
                host.phase = Phase::Ready;
                changed = true;
            }
            if host.billing_step_end.0 == 0
                && let Some(pool) = pools.iter().find(|p| p.name == host.pool)
            {
                host.billing_step_end = now.plus(step(pool, caps));
                changed = true;
            }
            if changed {
                self.store.put(&doc.key, &host, Some(doc.revision)).await?;
            }
            hosts.push(host);
        }

        let mut records: BTreeMap<PoolName, Vec<_>> = BTreeMap::new();
        for name in elastic_provisioners(pools) {
            let pool_names: Vec<&PoolName> = pools
                .iter()
                .filter(|p| p.provisioner == name)
                .map(|p| &p.name)
                .collect();
            let Some(provisioner) = self.provisioner(&name) else {
                continue;
            };
            for pool in pool_names {
                match provisioner.list(pool).await {
                    Ok(found) => {
                        records.insert(pool.clone(), found);
                    }
                    Err(e) => {
                        // A pool that the hub could not list is unknown, not
                        // empty: `plan` forgets nothing for it.
                        tracing::warn!(pool = %pool, "list failed: {e}");
                    }
                }
            }
        }

        let mut spend = self
            .store
            .get::<Spend>(&keys::spend(&month(now)))
            .await?
            .map(|d| d.value)
            .unwrap_or_default();
        spend.budget_usd = self.cfg.elastic.as_ref().and_then(|e| e.monthly_budget_usd);
        spend.live_usd = hosts
            .iter()
            .filter_map(|h| {
                let pool = pools.iter().find(|p| p.name == h.pool)?;
                Some(hourly(pool, caps) * hours(now.since(h.created)))
            })
            .sum();

        let unplaced = self.unplaced(&pending, &live, &paused).await?;

        Ok(Pass {
            pools: pools.to_vec(),
            hosts,
            live,
            unplaced,
            records,
            spend,
        })
    }

    /// The agents that wait for a host and that no existing host can take.
    async fn unplaced(
        &self,
        pending: &[(AgentId, PendingMove)],
        live: &[HostView],
        paused: &HashSet<AgentId>,
    ) -> Result<Vec<PlacementNeed>, ApiError> {
        let mut needs = Vec::new();
        for (id, move_) in pending {
            // A move that names its host stays there; a pool host would not
            // help it.
            if move_.target.host.is_some() {
                continue;
            }
            if let Some(need) = self.need_of(id, move_.target.harness, live, paused).await? {
                needs.push(need);
            }
        }
        // A queued spawn is demand too, before the tick places it.
        for doc in self.store.list::<SpawnRecord>(keys::SPAWN_PREFIX).await? {
            let record = doc.value;
            if record.state != SpawnState::Queued || record.spec.host.is_some() {
                continue;
            }
            if let Some(need) = self
                .need_of(&record.agent, record.spec.harness, live, paused)
                .await?
            {
                needs.push(need);
            }
        }
        Ok(needs)
    }

    /// The agent, when it waits and no existing host can take it.
    async fn need_of(
        &self,
        id: &AgentId,
        harness: Option<Harness>,
        live: &[HostView],
        paused: &HashSet<AgentId>,
    ) -> Result<Option<PlacementNeed>, ApiError> {
        // A paused session waits for the hub: it needs no host of its own.
        if paused.contains(id) {
            return Ok(None);
        }
        let lease = self.store.get::<Lease>(&keys::lease_agent(id)).await?;
        if lease.is_some_and(|d| {
            matches!(
                d.value.state,
                LeaseState::Held { .. } | LeaseState::Reserved { .. }
            )
        }) {
            return Ok(None);
        }
        let Some(agent) = self.store.get::<Agent>(&keys::agent(id)).await? else {
            return Ok(None);
        };
        let agent = agent.value;
        if agent.pinned || agent.retired || agent.paused.is_some() {
            return Ok(None);
        }
        let harness = harness.unwrap_or(agent.preferred_harness);
        let req = self.placement_req(&agent, None).await;
        let bundles = self.launch_bundles(&agent);
        if schedule::place(agent.preferred_harness, Some(harness), &req, &bundles, live).is_ok() {
            return Ok(None);
        }
        Ok(Some(PlacementNeed {
            agent: id.clone(),
            harness,
        }))
    }

    fn provisioner(&self, name: &str) -> Option<Arc<dyn Provisioner>> {
        self.cfg
            .elastic
            .as_ref()
            .and_then(|e| e.provisioners.get(name))
            .cloned()
    }

    async fn execute(
        &self,
        elastic: &ElasticConfig,
        pass: &mut Pass,
        caps: &BTreeMap<String, ProvisionerCapabilities>,
        action: Action,
        now: Timestamp,
    ) -> Result<(), ApiError> {
        match action {
            Action::Adopt { pool, record } => {
                let Some(config) = pass.pools.iter().find(|p| p.name == pool) else {
                    return Ok(());
                };
                let host = ElasticHost {
                    name: record.name.clone(),
                    pool,
                    provider_id: record.id,
                    phase: Phase::Booting,
                    created: now,
                    last_busy: now,
                    billing_step_end: now.plus(step(config, caps)),
                    last_seen: now,
                };
                self.store.upsert(&keys::elastic(&host.name), &host).await?;
            }
            Action::Forget { host } => self.store.delete(&keys::elastic(&host)).await?,
            Action::MarkLost { host } => self.set_phase(&host, Phase::Lost).await?,
            Action::MarkFailed { host } => self.set_phase(&host, Phase::Failed).await?,
            Action::Drain { host } => {
                let drained = {
                    let mut mem = self.mem.lock().await;
                    self.drain(&mut mem, &host).await
                };
                if let Err(e) = drained {
                    tracing::warn!(%host, "drain failed: {e}");
                }
                self.set_phase(&host, Phase::Draining).await?;
            }
            Action::Undrain { host } => {
                if let Some(mut info) = self.store.get::<HostInfo>(&keys::host(&host)).await?
                    && info.value.draining
                {
                    info.value.draining = false;
                    self.store
                        .put(&info.key, &info.value, Some(info.revision))
                        .await?;
                }
                self.set_phase(&host, Phase::Ready).await?;
            }
            Action::Delete { host, id } => {
                if !id.0.is_empty() {
                    let Some(pool) = pass
                        .hosts
                        .iter()
                        .find(|h| h.name == host)
                        .and_then(|h| pass.pools.iter().find(|p| p.name == h.pool))
                    else {
                        return Ok(());
                    };
                    let Some(provisioner) = elastic.provisioners.get(&pool.provisioner) else {
                        tracing::warn!(pool = %pool.name, "no provisioner for the delete");
                        return Ok(());
                    };
                    if let Err(e) = provisioner.delete(&id).await {
                        // The key stays, so the next tick tries again.
                        self.note_refusal(&pool.name, "delete", &e).await;
                        tracing::warn!(%host, "delete failed: {e}");
                        return Ok(());
                    }
                    self.clear_refusal(&pool.name).await;
                }
                if let Some(config) = pass
                    .hosts
                    .iter()
                    .find(|h| h.name == host)
                    .and_then(|h| pass.pools.iter().find(|p| p.name == h.pool))
                {
                    let life = pass
                        .hosts
                        .iter()
                        .find(|h| h.name == host)
                        .map(|h| now.since(h.created))
                        .unwrap_or_default();
                    pass.spend.usd += billed(config, caps, life);
                }
                let pool = pass
                    .hosts
                    .iter()
                    .find(|h| h.name == host)
                    .map(|h| h.pool.clone());
                // A pool host leaves no record behind: the box is gone, so
                // `borant hosts` must not name it. Only a host with an
                // `elastic.*` key is the hub's; a static host keeps its
                // record.
                if pool.is_some() {
                    self.forget_host(&host).await?;
                }
                self.store.delete(&keys::elastic(&host)).await?;
                // A host that goes takes an unspent token with it.
                self.store.delete(&keys::join(&host)).await?;
                if let Some(pool) = pool {
                    let event = self
                        .event(now, Severity::Info, SwarmEventBody::ElasticDeleted { pool })
                        .about_host(host.clone());
                    self.publish(event).await;
                }
            }
            Action::Create { pool, name } => {
                self.create(elastic, pass, caps, &pool, &name, now).await?;
            }
        }
        Ok(())
    }

    /// Deletes the record of a pool host that the provider deleted, and drops
    /// the host from the live view, so `borant hosts`, the view, and placement
    /// stop naming a box that is gone. The pool rule deletes only a host with
    /// no agent, so this needs none of the checks of `remove_host`. The record
    /// goes before the `elastic.*` key, so a store error here leaves the key
    /// for the next tick to retry, as a failed provider delete does.
    async fn forget_host(&self, host: &HostName) -> Result<(), ApiError> {
        self.store.delete(&keys::host(host)).await?;
        let mut mem = self.mem.lock().await;
        mem.hosts_seen.remove(host);
        mem.host_stats.remove(host);
        tracing::info!(host = %host, "pool host record removed");
        Ok(())
    }

    /// Orders one host. The key is written first, in `Requested`, so the next
    /// tick counts it and does not order it again.
    async fn create(
        &self,
        elastic: &ElasticConfig,
        pass: &mut Pass,
        caps: &BTreeMap<String, ProvisionerCapabilities>,
        pool: &PoolName,
        name: &HostName,
        now: Timestamp,
    ) -> Result<(), ApiError> {
        let Some(config) = pass.pools.iter().find(|p| p.name == *pool) else {
            return Ok(());
        };
        let Some(provisioner) = elastic.provisioners.get(&config.provisioner) else {
            tracing::warn!(pool = %pool, "no provisioner for the create");
            return Ok(());
        };
        let key = keys::elastic(name);
        let requested = ElasticHost {
            name: name.clone(),
            pool: pool.clone(),
            // The backend has no id for the host yet.
            provider_id: ProviderHostId(String::new()),
            phase: Phase::Requested,
            created: now,
            last_busy: now,
            billing_step_end: now.plus(step(config, caps)),
            last_seen: now,
        };
        self.store.upsert(&key, &requested).await?;
        // The boot values carry the vars of bundle `host-boot` and one join
        // token. Both leave the hub in the user data of this create only.
        let boot = self.boot_with_secrets(config, name, now).await;
        let spec = HostSpec {
            pool: pool.clone(),
            name: name.clone(),
            size: config.size.clone(),
            region: config.region.clone(),
            image: config.image.clone(),
            // The adapter adds its ownership labels; these are the hub's.
            provider_labels: BTreeMap::new(),
            tags: config.tags.clone(),
            boot,
        };
        match provisioner.create(&spec).await {
            Ok(record) => {
                let booting = ElasticHost {
                    provider_id: record.id,
                    phase: Phase::Booting,
                    ..requested
                };
                self.store.upsert(&key, &booting).await?;
                self.clear_refusal(pool).await;
                tracing::info!(pool = %pool, %name, "pool host ordered");
                let event = self
                    .event(
                        now,
                        Severity::Info,
                        SwarmEventBody::ElasticCreated { pool: pool.clone() },
                    )
                    .about_host(name.clone());
                self.publish(event).await;
            }
            Err(e) => {
                // No host, no key: the name is free for the next try, and the
                // pool waits `create_backoff`.
                self.store.delete(&key).await?;
                // The token of an order that made no host goes too, so the
                // KV holds no token longer than the order.
                self.store.delete(&keys::join(name)).await?;
                self.mem
                    .lock()
                    .await
                    .elastic_backoff
                    .insert(pool.clone(), now.plus(elastic.create_backoff));
                // A plugin that echoes its stdin must not put the token or a
                // bundle value in the refusal, the log, or an alert.
                let clean = PortError {
                    kind: e.kind,
                    message: redact(&e.message, &spec.boot),
                };
                self.note_refusal(pool, "create", &clean).await;
                tracing::warn!(pool = %pool, "create failed: {clean}");
            }
        }
        Ok(())
    }

    /// The boot values of a create: its name, pool, and tags, the vars of
    /// bundle `host-boot`, and a join token when the hub has its secrets.
    ///
    /// A hub with no `[secrets]`, no ready bundle, or no entropy creates as
    /// before, without a join. Nothing here logs a value or a token.
    async fn boot_with_secrets(
        &self,
        config: &PoolConfig,
        name: &HostName,
        now: Timestamp,
    ) -> BTreeMap<String, String> {
        let mut boot = boot_values(config, name);
        let Some(secrets) = &self.cfg.secrets else {
            return boot;
        };
        if !secrets.joins() {
            return boot;
        }
        match secrets.vars(&host_boot()).await {
            Ok(vars) => boot.extend(vars),
            Err(e) => {
                tracing::warn!(
                    pool = %config.name,
                    %name,
                    "the host-boot bundle did not resolve, so this create has no join: {e}"
                );
                return boot;
            }
        }
        let Some(token) = crate::join::token() else {
            tracing::warn!(%name, "no entropy for a join token, so this create has no join");
            return boot;
        };
        let doc = JoinToken {
            host: name.clone(),
            digest: crate::join::digest(&token),
            expires: now.plus(config.boot_timeout),
        };
        if let Err(e) = self.store.upsert(&keys::join(name), &doc).await {
            tracing::warn!(%name, "the join key did not write, so this create has no join: {e}");
            return boot;
        }
        // The token wins over a var of the bundle with the same name.
        boot.insert("BORANT_JOIN".to_owned(), token);
        boot
    }

    /// Deletes the join keys whose expiry passed, so a create that left no
    /// host behind leaves no token behind either. The conditional delete
    /// keeps a fresh token of a later create for the same name.
    async fn reap_joins(&self, now: Timestamp) -> Result<(), ApiError> {
        for doc in self.store.list::<JoinToken>(keys::JOIN_PREFIX).await? {
            if doc.value.expires > now {
                continue;
            }
            match self.store.delete_at(&doc.key, doc.revision).await {
                Ok(()) => {
                    tracing::debug!(host = %doc.value.host, "expired join key deleted");
                }
                Err(e) if e.code == borant_core::proto::ApiErrorCode::Conflict => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn set_phase(&self, host: &HostName, phase: Phase) -> Result<(), ApiError> {
        let key = keys::elastic(host);
        let Some(mut doc) = self.store.get::<ElasticHost>(&key).await? else {
            return Ok(());
        };
        if doc.value.phase == phase {
            return Ok(());
        }
        doc.value.phase = phase;
        self.store.put(&key, &doc.value, Some(doc.revision)).await?;
        Ok(())
    }

    async fn write_spend(&self, spend: &Spend, now: Timestamp) -> Result<(), ApiError> {
        self.store.upsert(&keys::spend(&month(now)), spend).await?;
        Ok(())
    }

    /// The pools that an agent waits for and whose budget holds the host
    /// back, for the alert pass. The demand is a pending move; the spend key
    /// is the month. A pool with no budget never blocks.
    pub(crate) async fn budget_blocked(&self) -> Result<Vec<PoolName>, ApiError> {
        let Some(elastic) = &self.cfg.elastic else {
            return Ok(Vec::new());
        };
        if elastic.pools.is_empty() || self.mem.lock().await.pending_moves.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.now();
        let spend = self
            .store
            .get::<Spend>(&keys::spend(&month(now)))
            .await?
            .map(|d| d.value)
            .unwrap_or_default();
        let total = spend.total();
        Ok(elastic
            .pools
            .iter()
            .filter(|pool| {
                elastic
                    .monthly_budget_usd
                    .is_some_and(|budget| total >= budget)
                    || pool
                        .monthly_budget_usd
                        .is_some_and(|budget| total >= budget)
            })
            .map(|pool| pool.name.clone())
            .collect())
    }

    /// Notes a provisioner refusal that the alerts report. Only `denied` and
    /// `invalid` are alerts; a retry handles the other errors.
    pub(crate) async fn note_refusal(&self, pool: &PoolName, op: &str, e: &PortError) {
        if matches!(e.kind, PortErrorKind::Denied | PortErrorKind::Invalid) {
            self.mem.lock().await.elastic_refused.insert(
                pool.clone(),
                Refusal {
                    pool: pool.clone(),
                    op: op.to_owned(),
                    message: e.message.clone(),
                },
            );
        }
    }

    /// Clears the refusal of a pool after a call that succeeded.
    pub(crate) async fn clear_refusal(&self, pool: &PoolName) {
        self.mem.lock().await.elastic_refused.remove(pool);
    }
}

/// The boot values of a new pool host: its name, its pool, and its tags.
fn boot_values(pool: &PoolConfig, name: &HostName) -> BTreeMap<String, String> {
    let tags = pool
        .tags
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(",");
    BTreeMap::from([
        ("BORANT_HOST_NAME".to_owned(), name.to_string()),
        ("BORANT_POOL".to_owned(), pool.name.to_string()),
        ("BORANT_TAGS".to_owned(), tags),
    ])
}

/// A provisioner message with every secret boot value replaced. The three
/// values that the hub writes itself stay, so an error still names the host
/// and its pool.
fn redact(message: &str, boot: &BTreeMap<String, String>) -> String {
    const PUBLIC: [&str; 3] = ["BORANT_HOST_NAME", "BORANT_POOL", "BORANT_TAGS"];
    let mut out = message.to_owned();
    for (key, value) in boot {
        if PUBLIC.contains(&key.as_str()) || value.is_empty() {
            continue;
        }
        if out.contains(value.as_str()) {
            out = out.replace(value.as_str(), "[redacted]");
        }
    }
    out
}

/// The distinct provisioner names of the pools.
fn elastic_provisioners(pools: &[PoolConfig]) -> Vec<String> {
    let mut names: Vec<String> = pools.iter().map(|p| p.provisioner.clone()).collect();
    names.sort();
    names.dedup();
    names
}

/// The billing step of a pool's backend, an hour when the hub knows nothing.
fn step(pool: &PoolConfig, caps: &BTreeMap<String, ProvisionerCapabilities>) -> Duration {
    caps.get(&pool.provisioner)
        .map(|c| c.billing_step)
        .filter(|s| !s.is_zero())
        .unwrap_or(Duration::from_secs(3600))
}

/// The price of one host-hour: the price of the pool's size, else the price
/// of the pool, else nothing.
fn hourly(pool: &PoolConfig, caps: &BTreeMap<String, ProvisionerCapabilities>) -> f64 {
    caps.get(&pool.provisioner)
        .and_then(|c| c.sizes.iter().find(|s| s.id == pool.size))
        .and_then(|s| s.price_hour_usd)
        .or(pool.price_hour_usd)
        .unwrap_or(0.0)
}

/// The bill of a host that lived `life`: whole billing steps, because a
/// backend bills the step that it started.
fn billed(
    pool: &PoolConfig,
    caps: &BTreeMap<String, ProvisionerCapabilities>,
    life: Duration,
) -> f64 {
    let step = step(pool, caps);
    let steps = (life.as_secs_f64() / step.as_secs_f64()).ceil().max(1.0);
    hourly(pool, caps) * steps * step.as_secs_f64() / 3600.0
}

fn hours(life: Duration) -> f64 {
    life.as_secs_f64() / 3600.0
}

/// The `yyyy-mm` of a timestamp, the suffix of the `spend.<yyyy-mm>` key.
fn month(now: Timestamp) -> String {
    let days = now.0.div_euclid(86_400_000);
    let (year, month) = civil_month(days);
    format!("{year:04}-{month:02}")
}

/// The civil year and month of a count of days since 1970-01-01.
fn civil_month(days: i64) -> (i64, u32) {
    // Howard Hinnant's `civil_from_days`, without the day.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 {
        yoe + era * 400 + 1
    } else {
        yoe + era * 400
    };
    (year, month as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::model::{
        Harness, HostInfo, HostStats, MoveReason, SessionStats, Task, TaskKind, TaskStatus,
    };
    use borant_core::proto::{CtlRequest, MoveTarget, ToolCall};
    use borant_core::{ChannelName, FencingToken, ManualClock, SessionId, TaskId};
    use borant_memory::{Call, MemoryBus, MemoryProvisioner, MemorySession, MemoryState};

    use crate::HubConfig;
    use crate::join::JoinToken;
    use crate::secrets::HubSecrets;

    const NOW: Timestamp = Timestamp(1_000_000_000);

    struct Fx {
        hub: Arc<Hub>,
        prov: Arc<MemoryProvisioner>,
        clock: Arc<ManualClock>,
    }

    fn name(host: &str) -> HostName {
        host.parse().unwrap()
    }

    fn pool() -> PoolConfig {
        PoolConfig {
            name: "workers".parse().unwrap(),
            provisioner: "cloud".to_owned(),
            size: "small".to_owned(),
            region: "r1".to_owned(),
            image: None,
            name_prefix: "e".to_owned(),
            tags: vec!["vps".parse().unwrap()],
            harnesses: vec![Harness::ClaudeCode],
            agents_per_host: 2,
            max_hosts: 2,
            idle_timeout: Duration::from_secs(20 * 60),
            boot_timeout: Duration::from_secs(10 * 60),
            lost_timeout: Duration::from_secs(10 * 60),
            backoff_until: None,
            monthly_budget_usd: None,
            price_hour_usd: Some(0.1),
        }
    }

    fn elastic(pools: Vec<PoolConfig>, prov: &Arc<MemoryProvisioner>) -> ElasticConfig {
        ElasticConfig {
            tick: Duration::from_secs(5),
            lost_timeout: Duration::from_secs(10 * 60),
            create_backoff: Duration::from_secs(5 * 60),
            monthly_budget_usd: None,
            pools,
            provisioners: BTreeMap::from([(
                "cloud".to_owned(),
                prov.clone() as Arc<dyn Provisioner>,
            )]),
        }
    }

    fn hub_with(elastic: Option<ElasticConfig>, prov: Arc<MemoryProvisioner>) -> Fx {
        build(elastic, prov, None)
    }

    /// A hub whose `[secrets]` open, so a create makes a join token.
    fn hub_joined(elastic: Option<ElasticConfig>, prov: Arc<MemoryProvisioner>) -> Fx {
        build(elastic, prov, Some(crate::testkit::hub_secrets()))
    }

    fn build(
        elastic: Option<ElasticConfig>,
        prov: Arc<MemoryProvisioner>,
        secrets: Option<HubSecrets>,
    ) -> Fx {
        let clock = Arc::new(ManualClock::new(NOW));
        let hub = Arc::new(Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock.clone(),
            HubConfig {
                elastic,
                secrets,
                ..HubConfig::default()
            },
        ));
        Fx { hub, prov, clock }
    }

    /// An agent that waits for a host, with a released lease so the lease
    /// tick looks at it.
    async fn pending(fx: &Fx, agent: &str) {
        let id: AgentId = agent.parse().unwrap();
        let a = Agent {
            id: id.clone(),
            spawn_key: None,
            task: None,
            stop_on_close: false,
            continues: None,
            retired: false,
            preferred_harness: Harness::ClaudeCode,
            repo: None,
            movable: true,
            channels: Vec::new(),
            session: None,
            host: None,
            harness_session_id: None,
            created: NOW,
            model: None,
            model_harness: None,
            project: None,
            members: Default::default(),
            pinned: false,
            role: None,
            collection: None,
            bundles: Vec::new(),
            parent: None,
            paused: None,
        };
        fx.hub.store.upsert(&keys::agent(&id), &a).await.unwrap();
        let lease = Lease {
            agent: id.clone(),
            token: FencingToken(1),
            state: LeaseState::Released {
                since: NOW.minus(Duration::from_secs(300)),
                parked: false,
            },
        };
        fx.hub
            .store
            .upsert(&keys::lease_agent(&id), &lease)
            .await
            .unwrap();
        fx.hub.mem.lock().await.pending_moves.insert(
            id,
            PendingMove {
                target: MoveTarget::default(),
                reason: MoveReason::Spawn,
                prompt: Some("work".to_owned()),
            },
        );
    }

    /// Registers a host of the `workers` pool and marks it seen now.
    async fn register(fx: &Fx, host: &str, max_agents: u32) {
        register_in(fx, host, Some("workers"), max_agents).await;
    }

    /// An agent whose session ended with the host that ran it (a killed pool
    /// host), with a claim unless `claim` is false.
    async fn lost_agent(fx: &Fx, agent: &str, host: &str, claim: bool) {
        let id: AgentId = agent.parse().unwrap();
        let channel: ChannelName = "api".parse().unwrap();
        let a = Agent {
            id: id.clone(),
            spawn_key: None,
            task: None,
            stop_on_close: false,
            continues: None,
            retired: false,
            preferred_harness: Harness::ClaudeCode,
            repo: None,
            movable: true,
            channels: vec![channel.clone()],
            session: Some(SessionId::new()),
            host: Some(name(host)),
            harness_session_id: None,
            created: NOW.minus(Duration::from_secs(3600)),
            model: None,
            model_harness: None,
            project: None,
            members: Default::default(),
            pinned: false,
            role: None,
            collection: None,
            bundles: Vec::new(),
            parent: None,
            paused: None,
        };
        fx.hub.store.upsert(&keys::agent(&id), &a).await.unwrap();
        let lease = Lease {
            agent: id.clone(),
            token: FencingToken(1),
            state: LeaseState::Expired {
                since: NOW.minus(Duration::from_secs(200)),
                last_session: None,
            },
        };
        fx.hub
            .store
            .upsert(&keys::lease_agent(&id), &lease)
            .await
            .unwrap();
        if claim {
            let task = Task {
                id: TaskId::new(),
                title: "t".to_owned(),
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
                created: NOW,
                updated: NOW,
                create_key: None,
            };
            fx.hub
                .store
                .upsert(&keys::task(&channel, &task.id), &task)
                .await
                .unwrap();
        }
    }

    /// The hosts that the provisioner was asked to create.
    async fn created(fx: &Fx) -> Vec<HostSpec> {
        fx.prov
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Create(spec) => Some(spec),
                _ => None,
            })
            .collect()
    }

    /// Registers a host of `pool` and marks it seen now. `None` is a static
    /// host, one that belongs to no pool.
    async fn register_in(fx: &Fx, host: &str, pool: Option<&str>, max_agents: u32) {
        let info = HostInfo {
            name: name(host),
            harnesses: vec![Harness::ClaudeCode],
            launcher: "process".to_owned(),
            max_agents,
            always_on: false,
            draining: false,
            ssh: None,
            tmux_socket: None,
            members: Default::default(),
            pool: pool.map(|p| p.parse().unwrap()),
            tags: vec!["vps".parse().unwrap()],
            bundles: Vec::new(),
            reserve_mem_mb: None,
            sandboxes: Vec::new(),
        };
        fx.hub
            .handle(CtlRequest::RegisterHost { info })
            .await
            .into_result()
            .expect("register");
    }

    /// An agent that holds a session on `host` and has been idle since
    /// `since`, as the host last reported at `at`.
    async fn idle_agent(fx: &Fx, agent: &str, host: &str, since: Timestamp, at: Timestamp) {
        let id: AgentId = agent.parse().unwrap();
        let session = SessionId::new();
        let a = Agent {
            id: id.clone(),
            spawn_key: None,
            task: None,
            stop_on_close: false,
            continues: None,
            retired: false,
            preferred_harness: Harness::ClaudeCode,
            repo: None,
            movable: true,
            channels: vec!["api".parse().unwrap()],
            session: Some(session),
            host: Some(name(host)),
            harness_session_id: None,
            created: at.minus(Duration::from_secs(3600)),
            model: None,
            model_harness: None,
            project: None,
            members: Default::default(),
            pinned: false,
            role: None,
            collection: None,
            bundles: Vec::new(),
            parent: None,
            paused: None,
        };
        fx.hub.store.upsert(&keys::agent(&id), &a).await.unwrap();
        let lease = Lease {
            agent: id.clone(),
            token: FencingToken(1),
            state: LeaseState::Held {
                session,
                host: name(host),
                expires: at.plus(Duration::from_secs(60)),
            },
        };
        fx.hub
            .store
            .upsert(&keys::lease_agent(&id), &lease)
            .await
            .unwrap();
        fx.hub.mem.lock().await.idle.insert(
            id,
            crate::IdleReport {
                session,
                host: name(host),
                since: Some(since),
                at,
            },
        );
    }

    /// A provisioner that refuses a create and echoes its boot values, as a
    /// chatty plugin that prints its stdin could.
    #[derive(Debug, Default)]
    struct Echoing {
        seen: std::sync::Mutex<Vec<HostSpec>>,
    }

    #[async_trait::async_trait]
    impl Provisioner for Echoing {
        async fn describe(&self) -> borant_core::ports::PortResult<ProvisionerCapabilities> {
            use borant_core::ports::provision::SizeInfo;
            Ok(ProvisionerCapabilities {
                backend: "echo".to_owned(),
                sizes: vec![SizeInfo {
                    id: "small".to_owned(),
                    vcpus: 1,
                    mem_mb: 1024,
                    price_hour_usd: Some(0.1),
                    stopped_price_hour_usd: None,
                }],
                regions: vec!["r1".to_owned()],
                typical_start: Duration::from_secs(60),
                typical_resume: None,
                billing_step: Duration::from_secs(3600),
                one_host_per_agent: false,
                stop_modes: Vec::new(),
            })
        }

        async fn create(
            &self,
            spec: &HostSpec,
        ) -> borant_core::ports::PortResult<borant_core::ports::provision::HostRecord> {
            self.seen.lock().expect("the seen specs").push(spec.clone());
            // A plugin that prints what it was given in its refusal.
            Err(PortError::denied(format!(
                "the backend refused {} with boot {:?}",
                spec.name, spec.boot
            )))
        }

        async fn delete(&self, _id: &ProviderHostId) -> borant_core::ports::PortResult<()> {
            Ok(())
        }

        async fn list(
            &self,
            _pool: &PoolName,
        ) -> borant_core::ports::PortResult<Vec<borant_core::ports::provision::HostRecord>>
        {
            Ok(Vec::new())
        }
    }

    fn provider_spec(host: &str) -> HostSpec {
        HostSpec {
            pool: "workers".parse().unwrap(),
            name: name(host),
            size: "small".to_owned(),
            region: "r1".to_owned(),
            image: None,
            provider_labels: BTreeMap::new(),
            tags: vec!["vps".parse().unwrap()],
            boot: BTreeMap::new(),
        }
    }

    async fn key(fx: &Fx, host: &str) -> Option<ElasticHost> {
        fx.hub
            .store
            .get::<ElasticHost>(&keys::elastic(&name(host)))
            .await
            .unwrap()
            .map(|d| d.value)
    }

    #[tokio::test]
    async fn elastic_an_unplaced_agent_creates_a_pool_host() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        fx.hub.tick_elastic().await.unwrap();

        let created: Vec<HostSpec> = fx
            .prov
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Create(spec) => Some(spec),
                _ => None,
            })
            .collect();
        assert_eq!(created.len(), 1, "one host for one agent");
        let spec = &created[0];
        assert_eq!(spec.pool, "workers".parse().unwrap());
        assert_eq!(spec.name, name("e1"));
        assert_eq!(spec.size, "small");
        assert_eq!(spec.region, "r1");
        assert_eq!(spec.tags, vec!["vps".parse().unwrap()]);
        // The boot values that the host reads.
        assert_eq!(spec.boot["BORANT_HOST_NAME"], "e1");
        assert_eq!(spec.boot["BORANT_POOL"], "workers");
        assert_eq!(spec.boot["BORANT_TAGS"], "vps");

        let host = key(&fx, "e1").await.expect("the elastic key");
        assert_eq!(host.phase, Phase::Booting);
        assert_eq!(host.provider_id.0, "workers-e1");
        // The provider holds the host.
        let listed = fx.prov.list(&"workers".parse().unwrap()).await.unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn elastic_a_create_with_secrets_adds_the_boot_vars_and_a_join_token() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_joined(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        fx.hub.tick_elastic().await.unwrap();

        let specs = created(&fx).await;
        let spec = &specs[0];
        // The vars of bundle `host-boot` ride in the boot values.
        assert_eq!(spec.boot["TAILSCALE_AUTHKEY"], crate::testkit::BOOT_VALUE);
        assert_eq!(spec.boot["BORANT_HOST_NAME"], "e1");
        let token = spec.boot["BORANT_JOIN"].clone();
        assert_eq!(token.len(), 64, "256 bits of hex");

        // The key holds the digest, the host, and the expiry of the pool.
        let doc = fx
            .hub
            .store
            .get::<JoinToken>(&keys::join(&name("e1")))
            .await
            .unwrap()
            .expect("the join key");
        assert_eq!(doc.value.host, name("e1"));
        assert_eq!(doc.value.digest, crate::join::digest(&token));
        assert_eq!(doc.value.expires, NOW.plus(Duration::from_secs(10 * 60)));
        let stored = serde_json::to_string(&doc.value).unwrap();
        assert!(
            !stored.contains(token.as_str()),
            "the KV holds no token: {stored}"
        );
    }

    #[tokio::test]
    async fn elastic_a_create_without_secrets_has_no_join_token() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        fx.hub.tick_elastic().await.unwrap();

        let specs = created(&fx).await;
        let spec = &specs[0];
        assert_eq!(spec.boot.len(), 3, "{:?}", spec.boot);
        assert!(
            !spec.boot.contains_key("BORANT_JOIN"),
            "no secrets, no join: {:?}",
            spec.boot
        );
        assert!(
            fx.hub
                .store
                .get::<JoinToken>(&keys::join(&name("e1")))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// The boot values are the one place that a token or a bundle value
    /// leaves the hub: no key of the store and no log line holds one.
    #[tokio::test]
    async fn elastic_a_create_and_the_store_hold_no_token_or_bundle_value() {
        use crate::testkit::{BOOT_VALUE, NATS_VALUE, STORE_TOKEN_VALUE, log_mark, log_since};

        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_joined(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        let mark = log_mark();
        fx.hub.tick_elastic().await.unwrap();
        let lines = log_since(mark);

        let specs = created(&fx).await;
        let token = specs[0].boot["BORANT_JOIN"].clone();
        assert_eq!(specs[0].boot["TAILSCALE_AUTHKEY"], BOOT_VALUE);

        let secrets = [token.as_str(), BOOT_VALUE, STORE_TOKEN_VALUE, NATS_VALUE];
        for entry in fx.hub.store.0.list("").await.unwrap() {
            let text = String::from_utf8_lossy(&entry.value).into_owned();
            for value in secrets {
                assert!(
                    !text.contains(value),
                    "the key {} holds {value:?}: {text}",
                    entry.key
                );
            }
        }
        assert!(lines.contains("pool host ordered"), "the capture works");
        for value in secrets {
            assert!(!lines.contains(value), "the log holds {value:?}: {lines}");
        }
    }

    #[tokio::test]
    async fn elastic_a_failed_create_leaves_no_join_key() {
        let prov = Arc::new(MemoryProvisioner::new());
        prov.set_create_failure(true);
        let fx = hub_joined(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        fx.hub.tick_elastic().await.unwrap();

        assert!(
            fx.hub
                .store
                .get::<JoinToken>(&keys::join(&name("e1")))
                .await
                .unwrap()
                .is_none(),
            "the token of an order that made no host goes"
        );
    }

    /// A provisioner that echoes the boot values in its refusal must not put a
    /// token or a bundle value in the refusal that the alerts report, or in
    /// the log.
    #[tokio::test]
    async fn elastic_a_provisioner_error_holds_no_token_or_bundle_value() {
        use crate::testkit::{BOOT_VALUE, NATS_VALUE, STORE_TOKEN_VALUE, log_mark, log_since};

        let backend = Arc::new(Echoing::default());
        let fx = build(
            Some(ElasticConfig {
                tick: Duration::from_secs(5),
                lost_timeout: Duration::from_secs(10 * 60),
                create_backoff: Duration::from_secs(5 * 60),
                monthly_budget_usd: None,
                pools: vec![pool()],
                provisioners: BTreeMap::from([(
                    "cloud".to_owned(),
                    backend.clone() as Arc<dyn Provisioner>,
                )]),
            }),
            Arc::new(MemoryProvisioner::new()),
            Some(crate::testkit::hub_secrets()),
        );
        pending(&fx, "a1").await;

        let mark = log_mark();
        fx.hub.tick_elastic().await.unwrap();
        let lines = log_since(mark);

        let (token, echoed) = {
            let seen = backend.seen.lock().expect("the seen specs");
            (
                seen[0].boot["BORANT_JOIN"].clone(),
                seen[0].boot.values().any(|v| v == BOOT_VALUE),
            )
        };
        assert!(echoed, "the fake echoed its spec");

        let refused = fx
            .hub
            .mem
            .lock()
            .await
            .elastic_refused
            .get(&"workers".parse::<PoolName>().unwrap())
            .map(|r| r.message.clone())
            .expect("the refusal of the alerts");
        assert!(
            refused.contains("[redacted]"),
            "the refusal is redacted: {refused}"
        );
        assert!(
            refused.contains("e1"),
            "the refusal names the host: {refused}"
        );
        for value in [token.as_str(), BOOT_VALUE, STORE_TOKEN_VALUE, NATS_VALUE] {
            assert!(
                !refused.contains(value),
                "the refusal holds {value:?}: {refused}"
            );
            assert!(!lines.contains(value), "the log holds {value:?}: {lines}");
        }
    }

    #[tokio::test]
    async fn elastic_a_delete_removes_the_unspent_join_key() {
        let prov = Arc::new(MemoryProvisioner::new());
        let record = prov.create(&provider_spec("e1")).await.unwrap();
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        register(&fx, "e1", 2).await;
        let host = ElasticHost {
            name: name("e1"),
            pool: "workers".parse().unwrap(),
            provider_id: record.id,
            phase: Phase::Ready,
            created: NOW.minus(Duration::from_secs(3600)),
            last_busy: NOW.minus(Duration::from_secs(21 * 60)),
            billing_step_end: NOW.plus(Duration::from_secs(60)),
            last_seen: NOW,
        };
        fx.hub
            .store
            .upsert(&keys::elastic(&name("e1")), &host)
            .await
            .unwrap();
        fx.hub
            .store
            .upsert(
                &keys::join(&name("e1")),
                &JoinToken {
                    host: name("e1"),
                    digest: "aa".repeat(32),
                    // Live until after the delete, so only the delete takes it.
                    expires: NOW.plus(Duration::from_secs(600)),
                },
            )
            .await
            .unwrap();

        fx.hub.tick_elastic().await.unwrap();
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick_elastic().await.unwrap();

        assert!(key(&fx, "e1").await.is_none(), "the host key goes");
        assert!(
            fx.hub
                .store
                .get::<JoinToken>(&keys::join(&name("e1")))
                .await
                .unwrap()
                .is_none(),
            "the unspent token goes with the host"
        );
    }

    #[tokio::test]
    async fn elastic_a_reap_deletes_an_expired_join_key() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        for (host, expires) in [
            ("e7", NOW.minus(Duration::from_secs(1))),
            ("e8", NOW.plus(Duration::from_secs(60))),
        ] {
            fx.hub
                .store
                .upsert(
                    &keys::join(&name(host)),
                    &JoinToken {
                        host: name(host),
                        digest: "bb".repeat(32),
                        expires,
                    },
                )
                .await
                .unwrap();
        }

        fx.hub.tick_elastic().await.unwrap();

        assert!(
            fx.hub
                .store
                .get::<JoinToken>(&keys::join(&name("e7")))
                .await
                .unwrap()
                .is_none(),
            "the expired token goes"
        );
        assert!(
            fx.hub
                .store
                .get::<JoinToken>(&keys::join(&name("e8")))
                .await
                .unwrap()
                .is_some(),
            "a live token stays"
        );
    }

    #[tokio::test]
    async fn elastic_a_registered_host_is_ready_and_places_the_agent() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;
        fx.hub.tick_elastic().await.unwrap();
        assert_eq!(key(&fx, "e1").await.unwrap().phase, Phase::Booting);

        // The host boots and registers.
        register(&fx, "e1", 2).await;
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick_elastic().await.unwrap();
        assert_eq!(
            key(&fx, "e1").await.unwrap().phase,
            Phase::Ready,
            "a registered host is ready"
        );

        // The lease tick places the waiting agent on the pool host.
        fx.hub.tick().await.unwrap();
        let lease = fx
            .hub
            .store
            .get::<Lease>(&keys::lease_agent(&"a1".parse().unwrap()))
            .await
            .unwrap()
            .expect("the lease");
        match lease.value.state {
            LeaseState::Reserved { host, .. } => assert_eq!(host, name("e1")),
            other => panic!("the agent is not placed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn elastic_a_takeover_that_finds_no_host_creates_one() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        // The host of the agent is gone and no other pool host runs.
        lost_agent(&fx, "w1", "gone", true).await;
        let w: AgentId = "w1".parse().unwrap();

        // The lease tick fits no host and records the demand; the elastic
        // pass of the same tick creates a host for it.
        fx.hub.tick().await.unwrap();
        assert!(
            fx.hub.mem.lock().await.pending_moves.contains_key(&w),
            "the takeover that fits no host is a demand"
        );
        let made = created(&fx).await;
        assert_eq!(made.len(), 1, "one host for the takeover: {made:?}");
        let host = made[0].name.to_string();

        // The host boots and registers, and the waiting agent is placed.
        register(&fx, &host, 2).await;
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick().await.unwrap();
        let lease = fx
            .hub
            .store
            .get::<Lease>(&keys::lease_agent(&w))
            .await
            .unwrap()
            .expect("the lease");
        match lease.value.state {
            LeaseState::Reserved { host: placed, .. } => {
                assert_eq!(placed, name(&host));
            }
            other => panic!("the agent is not placed: {other:?}"),
        }
        // A placed agent is no demand: the executor creates no second host.
        let before = created(&fx).await.len();
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick().await.unwrap();
        assert_eq!(
            created(&fx).await.len(),
            before,
            "a placed agent is no demand"
        );
    }

    #[tokio::test]
    async fn elastic_an_idle_takeover_creates_no_host() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        // No claim, no wake, no undelivered reply: there is no work to place.
        lost_agent(&fx, "w1", "gone", false).await;
        let w: AgentId = "w1".parse().unwrap();

        fx.hub.tick().await.unwrap();
        assert!(
            !fx.hub.mem.lock().await.pending_moves.contains_key(&w),
            "an idle agent is no demand"
        );
        fx.hub.tick_elastic().await.unwrap();
        assert!(created(&fx).await.is_empty(), "no host for an idle agent");
    }

    #[tokio::test]
    async fn elastic_a_retired_agent_creates_no_host() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "w1").await;
        let w: AgentId = "w1".parse().unwrap();
        let doc = fx
            .hub
            .store
            .get::<Agent>(&keys::agent(&w))
            .await
            .unwrap()
            .expect("the agent");
        {
            let mut mem = fx.hub.mem.lock().await;
            fx.hub.retire(&mut mem, &doc.value).await.unwrap();
        }
        assert!(
            !fx.hub.mem.lock().await.pending_moves.contains_key(&w),
            "a retired agent has no demand"
        );

        fx.hub.tick().await.unwrap();
        assert!(created(&fx).await.is_empty(), "no host for a retired agent");
    }

    #[tokio::test]
    async fn elastic_a_paused_session_creates_no_host() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "w1").await;
        let w: AgentId = "w1".parse().unwrap();
        let mut stats = HostStats::default();
        stats.sessions.push(SessionStats {
            agent: w.clone(),
            busy: false,
            paused: true,
            turn: 0,
            last_event_at: None,
            wakes: 0,
            wake_limited: 0,
            ack_failed: 0,
            append_failed: 0,
            redelivered: 0,
            bad_envelope: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        });
        fx.hub
            .mem
            .lock()
            .await
            .host_stats
            .insert(name("gone"), stats);

        fx.hub.tick().await.unwrap();
        assert!(
            created(&fx).await.is_empty(),
            "a paused session waits for the hub, not for a host"
        );

        // The session resumes: the demand counts again.
        if let Some(stats) = fx.hub.mem.lock().await.host_stats.get_mut(&name("gone"))
            && let Some(s) = stats.sessions.first_mut()
        {
            s.paused = false;
        }
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick().await.unwrap();
        assert_eq!(created(&fx).await.len(), 1, "a resumed demand gets a host");
    }

    #[tokio::test]
    async fn elastic_an_idle_host_is_drained_and_deleted() {
        let prov = Arc::new(MemoryProvisioner::new());
        let record = prov.create(&provider_spec("e1")).await.unwrap();
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        register(&fx, "e1", 2).await;
        // A static host of the same hub keeps its record: the hub never made
        // its box, so only the pool's delete may forget a record.
        register_in(&fx, "h1", None, 2).await;
        let host = ElasticHost {
            name: name("e1"),
            pool: "workers".parse().unwrap(),
            provider_id: record.id,
            phase: Phase::Ready,
            created: NOW.minus(Duration::from_secs(3600)),
            last_busy: NOW.minus(Duration::from_secs(21 * 60)),
            billing_step_end: NOW.plus(Duration::from_secs(60)),
            last_seen: NOW,
        };
        fx.hub
            .store
            .upsert(&keys::elastic(&name("e1")), &host)
            .await
            .unwrap();

        fx.hub.tick_elastic().await.unwrap();
        assert_eq!(key(&fx, "e1").await.unwrap().phase, Phase::Draining);
        let info = fx
            .hub
            .store
            .get::<HostInfo>(&keys::host(&name("e1")))
            .await
            .unwrap()
            .unwrap();
        assert!(info.value.draining, "the host is set to draining");

        // The next tick deletes it: nothing holds a lease on it.
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick_elastic().await.unwrap();
        assert!(key(&fx, "e1").await.is_none(), "the key goes");
        assert!(
            fx.prov
                .list(&"workers".parse().unwrap())
                .await
                .unwrap()
                .is_empty(),
            "the host is deleted"
        );
        // The box is gone, so its record goes with it: `borant hosts` must not
        // name a host that the provider deleted.
        assert!(
            fx.hub
                .store
                .get::<HostInfo>(&keys::host(&name("e1")))
                .await
                .unwrap()
                .is_none(),
            "the record of the deleted host goes"
        );
        let hosts = fx
            .hub
            .handle(CtlRequest::Call {
                actor: None,
                key: None,
                call: ToolCall::ListHosts,
            })
            .await
            .into_result()
            .unwrap();
        let names: Vec<&str> = hosts
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["host"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["h1"], "`borant hosts` names no box that is gone");
        // The live view drops the deleted host with its stats.
        let mem = fx.hub.mem.lock().await;
        assert!(
            !mem.hosts_seen.contains_key(&name("e1")),
            "the seen time of the deleted host goes"
        );
        assert!(
            !mem.host_stats.contains_key(&name("e1")),
            "the stats of the deleted host go"
        );
        drop(mem);
        // The bill of the finished host is on record.
        let spend = fx
            .hub
            .store
            .get::<Spend>(&keys::spend(&month(NOW)))
            .await
            .unwrap()
            .expect("a spend key");
        assert!(spend.value.usd > 0.0, "{:?}", spend.value);
    }

    #[tokio::test]
    async fn elastic_never_above_max_hosts() {
        let mut one = pool();
        one.max_hosts = 1;
        let prov = Arc::new(MemoryProvisioner::new());
        let record = prov.create(&provider_spec("e1")).await.unwrap();
        let fx = hub_with(Some(elastic(vec![one], &prov)), prov.clone());
        // A full host: two agents on it.
        register(&fx, "e1", 2).await;
        for agent in ["a1", "a2"] {
            let lease = Lease {
                agent: agent.parse().unwrap(),
                token: FencingToken(1),
                state: LeaseState::Held {
                    session: borant_core::SessionId::new(),
                    host: name("e1"),
                    expires: NOW.plus(Duration::from_secs(60)),
                },
            };
            fx.hub
                .store
                .upsert(&keys::lease_agent(&agent.parse().unwrap()), &lease)
                .await
                .unwrap();
        }
        let host = ElasticHost {
            name: name("e1"),
            pool: "workers".parse().unwrap(),
            provider_id: record.id,
            phase: Phase::Ready,
            created: NOW,
            last_busy: NOW,
            billing_step_end: NOW.plus(Duration::from_secs(3600)),
            last_seen: NOW,
        };
        fx.hub
            .store
            .upsert(&keys::elastic(&name("e1")), &host)
            .await
            .unwrap();
        pending(&fx, "a3").await;
        // The log of the setup create is not the hub's.
        fx.prov.take_calls();

        fx.hub.tick_elastic().await.unwrap();

        assert!(
            !fx.prov.calls().iter().any(|c| matches!(c, Call::Create(_))),
            "{:?}",
            fx.prov.calls()
        );
        assert_eq!(
            fx.prov
                .list(&"workers".parse().unwrap())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn elastic_a_restart_adopts_a_host_with_no_key() {
        let prov = Arc::new(MemoryProvisioner::new());
        // The provider holds a host from a create that the hub did not record.
        prov.create(&provider_spec("e9")).await.unwrap();
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());

        fx.hub.tick_elastic().await.unwrap();

        let host = key(&fx, "e9").await.expect("the adopted key");
        assert_eq!(host.phase, Phase::Booting);
        assert_eq!(host.provider_id.0, "workers-e9");
        assert_eq!(host.pool, "workers".parse().unwrap());
    }

    #[tokio::test]
    async fn elastic_a_failed_create_sets_the_backoff() {
        let prov = Arc::new(MemoryProvisioner::new());
        prov.set_create_failure(true);
        let fx = hub_with(Some(elastic(vec![pool()], &prov)), prov.clone());
        pending(&fx, "a1").await;

        fx.hub.tick_elastic().await.unwrap();
        assert!(key(&fx, "e1").await.is_none(), "no key without a host");
        assert!(
            fx.hub
                .mem
                .lock()
                .await
                .elastic_backoff
                .contains_key(&"workers".parse().unwrap()),
            "the pool backs off"
        );

        // The next tick in the backoff window orders nothing.
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick_elastic().await.unwrap();
        let creates = fx
            .prov
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Create(_)))
            .count();
        assert_eq!(creates, 1, "no second create during the backoff");

        // After the backoff, with the backend working again, the create lands.
        prov.set_create_failure(false);
        fx.clock.advance(Duration::from_secs(5 * 60 + 6));
        fx.hub.tick_elastic().await.unwrap();
        assert_eq!(key(&fx, "e1").await.unwrap().phase, Phase::Booting);
    }

    #[tokio::test]
    async fn elastic_without_pools_does_nothing() {
        let prov = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(None, prov.clone());
        fx.hub.tick_elastic().await.unwrap();
        fx.hub.tick().await.unwrap();
        assert!(fx.prov.calls().is_empty(), "{:?}", fx.prov.calls());

        // A config with no pool is the same.
        let prov2 = Arc::new(MemoryProvisioner::new());
        let fx = hub_with(Some(elastic(Vec::new(), &prov2)), prov2.clone());
        fx.hub.tick_elastic().await.unwrap();
        assert!(fx.prov.calls().is_empty());
    }

    #[tokio::test]
    async fn elastic_parks_with_the_timeout_and_hosts_of_each_pool() {
        let prov = Arc::new(MemoryProvisioner::new());
        let mut fast = pool();
        fast.name = "fast".parse().unwrap();
        fast.idle_timeout = Duration::from_secs(5 * 60);
        let mut slow = pool();
        slow.name = "slow".parse().unwrap();
        slow.idle_timeout = Duration::from_secs(20 * 60);
        let fx = hub_with(Some(elastic(vec![fast, slow], &prov)), prov.clone());
        register_in(&fx, "f1", Some("fast"), 2).await;
        register_in(&fx, "s1", Some("slow"), 2).await;
        // One agent on each host, each idle for six minutes: past the timeout
        // of the fast pool, not of the slow one.
        let six = NOW.minus(Duration::from_secs(6 * 60));
        idle_agent(&fx, "wf", "f1", six, NOW).await;
        idle_agent(&fx, "ws", "s1", six, NOW).await;

        fx.hub.tick_elastic().await.unwrap();

        let parked: Vec<AgentId> = fx.hub.mem.lock().await.park_sent.keys().cloned().collect();
        assert_eq!(
            parked,
            vec!["wf".parse::<AgentId>().unwrap()],
            "the fast pool's timeout parks its own agent, and only that one"
        );

        // Past the slow pool's own timeout, its agent is parked too.
        let long = NOW.minus(Duration::from_secs(21 * 60));
        idle_agent(&fx, "ws", "s1", long, NOW).await;
        fx.clock.advance(Duration::from_secs(6));
        fx.hub.tick_elastic().await.unwrap();
        let parked: Vec<AgentId> = fx.hub.mem.lock().await.park_sent.keys().cloned().collect();
        assert!(
            parked.contains(&"ws".parse::<AgentId>().unwrap()),
            "the slow pool's agent is parked by its own timeout: {parked:?}"
        );
    }

    #[test]
    fn month_of_a_timestamp() {
        assert_eq!(month(Timestamp(0)), "1970-01");
        assert_eq!(month(NOW), "1970-01");
        // 2026-10-25T00:00:00Z
        assert_eq!(month(Timestamp(1_793_116_800_000)), "2026-10");
    }
}
