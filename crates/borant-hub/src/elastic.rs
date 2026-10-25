//! The elastic executor: one pass per `elastic.tick`. It gathers the inputs
//! of [`rules::pool::plan`], runs it, and executes the actions on the
//! provisioner and on the `elastic.*` keys.
//!
//! The pass never holds the `mem` mutex across a provisioner call, because a
//! create can take two minutes. It reads `mem` briefly at the start, drops it
//! for the provider ops, and takes it again for the steps that need it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, HostInfo, Lease, LeaseState};
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
        let (pending, seen) = {
            let mem = self.mem.lock().await;
            let pending: Vec<(AgentId, PendingMove)> = mem
                .pending_moves
                .iter()
                .map(|(id, move_)| (id.clone(), move_.clone()))
                .collect();
            (pending, mem.hosts_seen.clone())
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

        let unplaced = self.unplaced(&pending, &live).await?;

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
    ) -> Result<Vec<PlacementNeed>, ApiError> {
        let mut needs = Vec::new();
        for (id, move_) in pending {
            // A spawn or a move that names its host stays there; a pool host
            // would not help it.
            if move_.target.host.is_some() {
                continue;
            }
            let lease = self.store.get::<Lease>(&keys::lease_agent(id)).await?;
            if lease.is_some_and(|d| {
                matches!(
                    d.value.state,
                    LeaseState::Held { .. } | LeaseState::Reserved { .. }
                )
            }) {
                continue;
            }
            let Some(agent) = self.store.get::<Agent>(&keys::agent(id)).await? else {
                continue;
            };
            let agent = agent.value;
            if agent.pinned {
                continue;
            }
            let harness = move_.target.harness.unwrap_or(agent.preferred_harness);
            let req = self.placement_req(&agent, None).await;
            let bundles = self.launch_bundles(&agent);
            if schedule::place(agent.preferred_harness, Some(harness), &req, &bundles, live)
                .is_some()
            {
                continue;
            }
            needs.push(PlacementNeed {
                agent: id.clone(),
                harness,
            });
        }
        Ok(needs)
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
                self.store.delete(&keys::elastic(&host)).await?;
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
        let spec = HostSpec {
            pool: pool.clone(),
            name: name.clone(),
            size: config.size.clone(),
            region: config.region.clone(),
            image: config.image.clone(),
            // The adapter adds its ownership labels; these are the hub's.
            provider_labels: BTreeMap::new(),
            tags: config.tags.clone(),
            boot: boot_values(config, name),
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
                self.mem
                    .lock()
                    .await
                    .elastic_backoff
                    .insert(pool.clone(), now.plus(elastic.create_backoff));
                self.note_refusal(pool, "create", &e).await;
                tracing::warn!(pool = %pool, "create failed: {e}");
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
    use borant_core::model::{Harness, HostInfo, MoveReason};
    use borant_core::proto::{CtlRequest, MoveTarget};
    use borant_core::{FencingToken, ManualClock, SessionId};
    use borant_memory::{Call, MemoryBus, MemoryProvisioner, MemorySession, MemoryState};

    use crate::HubConfig;

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
        let clock = Arc::new(ManualClock::new(NOW));
        let hub = Arc::new(Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock.clone(),
            HubConfig {
                elastic,
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
            parent: None,
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
        register_in(fx, host, "workers", max_agents).await;
    }

    /// Registers a host of `pool` and marks it seen now.
    async fn register_in(fx: &Fx, host: &str, pool: &str, max_agents: u32) {
        let info = HostInfo {
            name: name(host),
            harnesses: vec![Harness::ClaudeCode],
            launcher: "process".to_owned(),
            max_agents,
            always_on: false,
            draining: false,
            ssh: None,
            members: Default::default(),
            pool: Some(pool.parse().unwrap()),
            tags: vec!["vps".parse().unwrap()],
            bundles: Vec::new(),
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
            parent: None,
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
    async fn elastic_an_idle_host_is_drained_and_deleted() {
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
        register_in(&fx, "f1", "fast", 2).await;
        register_in(&fx, "s1", "slow", 2).await;
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
