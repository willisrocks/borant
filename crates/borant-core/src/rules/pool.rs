//! The rules of the elastic pools: which pool gets a new host, and which
//! hosts the hub drains and deletes (elastic doc, "Pools"). Pure: the hub
//! gathers the inputs, calls `plan` at each `elastic.tick`, and executes the
//! actions.
//!
//! Two fields of `[elastic]` reach the rules through `PoolConfig`, because the
//! signatures of `choose` and `plan` are fixed: `lost_timeout`, folded into
//! each pool, and `backoff_until`, which the hub sets in memory after a failed
//! create. `Spend.budget_usd` carries `monthly_budget_usd` the same way. None
//! of them are keys of `[[pools]]`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, HostName, PoolName, Tag};
use crate::model::Harness;
use crate::ports::provision::{HostRecord, ProviderHostId};
use crate::rules::schedule::HostView;
use crate::time::Timestamp;

/// How close to the end of its billing step an idle host must be before the
/// hub drains it. A backend that bills a whole hour charges it anyway.
pub const BILLING_SLACK: Duration = Duration::from_secs(5 * 60);

/// The parsed `[[pools]]` entry of `hub.toml`, plus the two values that the
/// hub folds in from `[elastic]` (`lost_timeout`, `backoff_until`).
#[derive(Debug, Clone, PartialEq)]
pub struct PoolConfig {
    pub name: PoolName,
    /// The `[provisioners.<name>]` entry that makes the hosts.
    pub provisioner: String,
    /// Opaque: only the backend reads it.
    pub size: String,
    /// Opaque: only the backend reads it.
    pub region: String,
    /// Opaque: only the backend reads it.
    pub image: Option<String>,
    /// The prefix of the host names that the pool creates.
    pub name_prefix: String,
    /// The tags of the hosts that the pool creates.
    pub tags: Vec<Tag>,
    /// The harnesses that the pool hosts run.
    pub harnesses: Vec<Harness>,
    /// The slots of one host.
    pub agents_per_host: u32,
    pub max_hosts: usize,
    /// How long a ready host with no agent stays before it goes.
    pub idle_timeout: Duration,
    /// How long a create may take before the host is failed.
    pub boot_timeout: Duration,
    /// `[elastic] lost_timeout`. Not a `[[pools]]` key: the hub folds it in.
    pub lost_timeout: Duration,
    /// `None` unless a create in this pool failed: then `now` plus
    /// `[elastic] create_backoff`. Not a `[[pools]]` key: the hub keeps it in
    /// memory.
    pub backoff_until: Option<Timestamp>,
    /// The cap of this pool for the month.
    pub monthly_budget_usd: Option<f64>,
    /// The price of one host-hour, when the backend reports no price.
    pub price_hour_usd: Option<f64>,
}

/// One agent that `place` did not put on a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementNeed {
    pub agent: AgentId,
    pub harness: Harness,
}

/// The hub's view of one pool host, at the key `elastic.<name>`. Written only
/// by the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElasticHost {
    /// The host name, the suffix of the key. The value repeats it so that the
    /// rules need no map from the keys.
    pub name: HostName,
    pub pool: PoolName,
    pub provider_id: ProviderHostId,
    pub phase: Phase,
    /// When the key was written.
    pub created: Timestamp,
    /// The last time the host held an agent; `created` until then.
    pub last_busy: Timestamp,
    /// The end of the billing step that the host is in.
    pub billing_step_end: Timestamp,
    /// The last heartbeat that the hub saw. The hub writes it with each
    /// heartbeat, so the rules can find a lost host without a clock of their
    /// own.
    pub last_seen: Timestamp,
}

/// The phase of one pool host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The hub recorded the demand and ordered the host.
    Requested,
    /// The backend accepted the order; the host has not registered yet.
    Booting,
    /// The host registered and holds no reservation.
    Ready,
    /// The hub set `draining` and waits for the last agent to leave.
    Draining,
    /// A create or a boot missed its deadline.
    Failed,
    /// No heartbeat for `lost_timeout`.
    Lost,
    /// The hub ordered the delete.
    Deleting,
}

/// What the month has cost, at the key `spend.<yyyy-mm>`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    /// What the finished hosts cost, added at each delete.
    #[serde(default)]
    pub usd: f64,
    /// The estimated cost of the hosts that are live now.
    #[serde(default)]
    pub live_usd: f64,
    /// `[elastic] monthly_budget_usd`, `None` for no cap. Not a key of
    /// `spend.<yyyy-mm>`: the hub fills it at each tick.
    #[serde(skip)]
    pub budget_usd: Option<f64>,
}

impl Spend {
    /// The month's cost so far, plus the estimate of the live hosts.
    pub fn total(&self) -> f64 {
        self.usd + self.live_usd
    }
}

/// One step that the hub executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create { pool: PoolName, name: HostName },
    Drain { host: HostName },
    Undrain { host: HostName },
    Delete { host: HostName, id: ProviderHostId },
    Adopt { pool: PoolName, record: HostRecord },
    Forget { host: HostName },
    MarkLost { host: HostName },
    MarkFailed { host: HostName },
}

/// Picks the pool for one unplaced agent: the cheapest slot-hour among the
/// pools that run its harness, stay under `max_hosts` and the budgets, and
/// are not in a create backoff. A pool with no price sorts last.
pub fn choose(
    pools: &[PoolConfig],
    hosts: &[ElasticHost],
    spend: &Spend,
    need: &PlacementNeed,
    now: Timestamp,
) -> Option<PoolName> {
    pick(pools, &count_hosts(hosts), spend, need, now).map(|p| p.name.clone())
}

/// The actions of one `elastic.tick`: reconcile the `elastic.*` keys with the
/// provider's hosts, move each host through its phases, and create the hosts
/// that the unplaced agents need.
pub fn plan(
    pools: &[PoolConfig],
    hosts: &[ElasticHost],
    live: &[HostView],
    unplaced: &[PlacementNeed],
    records: &BTreeMap<PoolName, Vec<HostRecord>>,
    spend: &Spend,
    now: Timestamp,
) -> Vec<Action> {
    let by_name: BTreeMap<&PoolName, &PoolConfig> = pools.iter().map(|p| (&p.name, p)).collect();
    let views: BTreeMap<&HostName, &HostView> = live.iter().map(|v| (&v.info.name, v)).collect();
    let mut actions = Vec::new();

    // Reconcile: a provider host without a key is adopted, because a hub that
    // restarted during a create must find it again. A key whose host the
    // backend does not list, or whose pool is gone, is forgotten.
    let mut forgotten: BTreeSet<HostName> = BTreeSet::new();
    for host in hosts {
        if !by_name.contains_key(&host.pool) {
            forgotten.insert(host.name.clone());
            continue;
        }
        if !matches!(
            host.phase,
            Phase::Booting | Phase::Ready | Phase::Draining | Phase::Lost
        ) {
            continue;
        }
        // A pool that the hub did not list is unknown, not empty: forget
        // nothing on a failed list.
        let Some(found) = records.get(&host.pool) else {
            continue;
        };
        if !found.iter().any(|r| r.id == host.provider_id) {
            forgotten.insert(host.name.clone());
        }
    }
    for (pool, found) in records {
        for record in found {
            if !hosts.iter().any(|h| h.provider_id == record.id) {
                actions.push(Action::Adopt {
                    pool: pool.clone(),
                    record: record.clone(),
                });
            }
        }
    }
    for host in &forgotten {
        actions.push(Action::Forget { host: host.clone() });
    }

    // The phases.
    for host in hosts {
        if forgotten.contains(&host.name) {
            continue;
        }
        let Some(pool) = by_name.get(&host.pool) else {
            continue;
        };
        let view = views.get(&host.name).copied();
        let online = view.is_some_and(|v| v.online);
        let lost = now.since(host.last_seen) >= pool.lost_timeout;
        let boot_expired = now.since(host.created) >= pool.boot_timeout;
        match host.phase {
            Phase::Requested => {
                // The record of a create appears on a later tick; a request
                // that never became a host fails.
                if boot_expired {
                    actions.push(Action::MarkFailed {
                        host: host.name.clone(),
                    });
                }
            }
            Phase::Booting => {
                if online {
                    // The host registered. The hub sets the phase to Ready.
                } else if view.is_none() {
                    // It never registered.
                    if boot_expired {
                        actions.push(Action::MarkFailed {
                            host: host.name.clone(),
                        });
                    }
                } else if lost {
                    actions.push(Action::MarkLost {
                        host: host.name.clone(),
                    });
                }
            }
            Phase::Ready => {
                if !online && lost {
                    actions.push(Action::MarkLost {
                        host: host.name.clone(),
                    });
                } else if is_idle(host, view, pool, now) {
                    actions.push(Action::Drain {
                        host: host.name.clone(),
                    });
                }
            }
            Phase::Draining => {
                // A lease that appeared after the drain wins: never delete a
                // host that holds an agent.
                if view.is_some_and(|v| v.active_agents > 0) {
                    actions.push(Action::Undrain {
                        host: host.name.clone(),
                    });
                } else if !online && lost {
                    actions.push(Action::MarkLost {
                        host: host.name.clone(),
                    });
                } else {
                    actions.push(Action::Delete {
                        host: host.name.clone(),
                        id: host.provider_id.clone(),
                    });
                }
            }
            Phase::Failed | Phase::Lost => {
                actions.push(Action::Delete {
                    host: host.name.clone(),
                    id: host.provider_id.clone(),
                });
            }
            Phase::Deleting => {}
        }
    }

    actions.extend(creates(
        pools, hosts, &views, unplaced, &by_name, spend, now,
    ));
    actions
}

/// A host with no agent that waited `idle_timeout` and is near the end of its
/// billing step.
fn is_idle(host: &ElasticHost, view: Option<&HostView>, pool: &PoolConfig, now: Timestamp) -> bool {
    let empty = view.is_none_or(|v| v.active_agents == 0);
    empty
        && now.since(host.last_busy) >= pool.idle_timeout
        && host.billing_step_end.since(now) <= BILLING_SLACK
}

/// Creates one host for each unplaced agent that no free slot covers.
fn creates(
    pools: &[PoolConfig],
    hosts: &[ElasticHost],
    views: &BTreeMap<&HostName, &HostView>,
    unplaced: &[PlacementNeed],
    by_name: &BTreeMap<&PoolName, &PoolConfig>,
    spend: &Spend,
    now: Timestamp,
) -> Vec<Action> {
    // The free slots of the online pool hosts, plus the slots that the hosts
    // in Requested or Booting will offer.
    let mut free: BTreeMap<PoolName, u32> = pools.iter().map(|p| (p.name.clone(), 0)).collect();
    for view in views.values() {
        let Some(pool) = view.info.pool.as_ref() else {
            // A static host: `place` already put an agent there.
            continue;
        };
        if !view.online || view.info.draining {
            continue;
        }
        if let Some(slots) = free.get_mut(pool) {
            *slots += view.info.max_agents.saturating_sub(view.active_agents);
        }
    }
    for host in hosts {
        if matches!(host.phase, Phase::Requested | Phase::Booting)
            && !views.contains_key(&host.name)
            && let Some(pool) = by_name.get(&host.pool)
            && let Some(slots) = free.get_mut(&host.pool)
        {
            *slots += pool.agents_per_host;
        }
    }

    let mut counts = count_hosts(hosts);
    let mut taken: BTreeSet<HostName> = hosts.iter().map(|h| h.name.clone()).collect();
    let mut actions = Vec::new();
    for need in unplaced {
        let served = pools.iter().position(|p| {
            p.harnesses.contains(&need.harness) && free.get(&p.name).copied().unwrap_or(0) > 0
        });
        if let Some(at) = served {
            let name = pools[at].name.clone();
            if let Some(slots) = free.get_mut(&name) {
                *slots -= 1;
            }
            continue;
        }
        let Some(pool) = pick(pools, &counts, spend, need, now) else {
            continue;
        };
        let count = counts.get(&pool.name).copied().unwrap_or(0);
        let name = next_name(&pool.name_prefix, &taken, count + 1);
        taken.insert(name.clone());
        *counts.entry(pool.name.clone()).or_default() += 1;
        // The new host takes this agent; its other slots are free.
        *free.entry(pool.name.clone()).or_default() += pool.agents_per_host.saturating_sub(1);
        actions.push(Action::Create {
            pool: pool.name.clone(),
            name,
        });
    }
    actions
}

fn count_hosts(hosts: &[ElasticHost]) -> BTreeMap<PoolName, usize> {
    let mut counts: BTreeMap<PoolName, usize> = BTreeMap::new();
    for host in hosts {
        *counts.entry(host.pool.clone()).or_default() += 1;
    }
    counts
}

/// The pool that `choose` picks for one agent.
fn pick<'a>(
    pools: &'a [PoolConfig],
    counts: &BTreeMap<PoolName, usize>,
    spend: &Spend,
    need: &PlacementNeed,
    now: Timestamp,
) -> Option<&'a PoolConfig> {
    pools
        .iter()
        .filter(|p| p.harnesses.contains(&need.harness))
        .filter(|p| counts.get(&p.name).copied().unwrap_or(0) < p.max_hosts)
        .filter(|p| p.backoff_until.is_none_or(|until| until <= now))
        .filter(|p| within_budget(p, spend))
        .min_by(|a, b| order(a, b))
}

/// The cheapest slot-hour, then the name. A pool with no reported price sorts
/// last, because a price that the backend will not name is not a bargain.
fn order(a: &PoolConfig, b: &PoolConfig) -> Ordering {
    let order = match (slot_price(a), slot_price(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    order.then_with(|| a.name.cmp(&b.name))
}

fn slot_price(pool: &PoolConfig) -> Option<f64> {
    pool.price_hour_usd
        .map(|hour| hour / f64::from(pool.agents_per_host.max(1)))
}

/// Under the pool's cap for the month and under the month's cap.
///
/// The month's spend is not split per pool, so a pool budget is measured
/// against the whole month, which creates less, never more.
fn within_budget(pool: &PoolConfig, spend: &Spend) -> bool {
    let total = spend.total();
    spend.budget_usd.is_none_or(|budget| total < budget)
        && pool.monthly_budget_usd.is_none_or(|budget| total < budget)
}

/// A name for a new host of a pool: the prefix and a number that no live host
/// or provider record uses.
fn next_name(prefix: &str, taken: &BTreeSet<HostName>, from: usize) -> HostName {
    let mut index = from;
    loop {
        let name = format!("{prefix}{index}").parse::<HostName>();
        match name {
            Ok(name) if !taken.contains(&name) => return name,
            // A prefix that cannot be part of a name is a config error that
            // the hub refuses before it reaches the rules; the index alone
            // keeps this rule total.
            Err(_) => {
                let fallback = format!("e{index}").parse::<HostName>();
                if let Ok(name) = fallback
                    && !taken.contains(&name)
                {
                    return name;
                }
            }
            Ok(_) => {}
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HostInfo;

    const NOW: Timestamp = Timestamp(1_800_000_000_000);
    const MIN: i64 = 60_000;

    fn name(s: &str) -> HostName {
        s.parse().unwrap()
    }

    fn pool(pool: &str) -> PoolConfig {
        PoolConfig {
            name: pool.parse().unwrap(),
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
            monthly_budget_usd: Some(60.0),
            price_hour_usd: Some(0.1),
        }
    }

    fn need(agent: &str) -> PlacementNeed {
        PlacementNeed {
            agent: agent.parse().unwrap(),
            harness: Harness::ClaudeCode,
        }
    }

    /// A key of the simplest pool, `workers`, that saw a heartbeat `ago`.
    fn key(host: &str, phase: Phase) -> ElasticHost {
        let ago = Timestamp(NOW.0 - 60 * MIN);
        ElasticHost {
            name: name(host),
            pool: "workers".parse().unwrap(),
            provider_id: ProviderHostId(format!("id-{host}")),
            phase,
            created: ago,
            last_busy: ago,
            billing_step_end: ago,
            last_seen: NOW,
        }
    }

    fn record(host: &str) -> HostRecord {
        HostRecord {
            id: ProviderHostId(format!("id-{host}")),
            pool: "workers".parse().unwrap(),
            name: name(host),
            state: crate::ports::provision::ProviderState::Running,
            created: Timestamp(NOW.0 - 60 * MIN),
            provider_labels: Default::default(),
        }
    }

    /// The view of a live pool host with `active` agents.
    fn view(host: &str, active: u32, online: bool) -> HostView {
        HostView {
            info: HostInfo {
                name: name(host),
                harnesses: vec![Harness::ClaudeCode],
                launcher: "process".to_owned(),
                max_agents: 2,
                always_on: false,
                draining: false,
                ssh: None,
                members: Default::default(),
                pool: Some("workers".parse().unwrap()),
                tags: Vec::new(),
            },
            online,
            active_agents: active,
            has_repo_access: true,
            pool: Some("workers".parse().unwrap()),
        }
    }

    fn records(hosts: &[&str]) -> BTreeMap<PoolName, Vec<HostRecord>> {
        BTreeMap::from([(
            "workers".parse().unwrap(),
            hosts.iter().map(|h| record(h)).collect(),
        )])
    }

    fn idle_key(host: &str) -> ElasticHost {
        // Ready, no agent for 21 minutes, and the billing step ends in a
        // minute.
        ElasticHost {
            phase: Phase::Ready,
            last_busy: Timestamp(NOW.0 - 21 * MIN),
            billing_step_end: Timestamp(NOW.0 + MIN),
            ..key(host, Phase::Ready)
        }
    }

    #[test]
    fn demand_above_supply_creates() {
        let pools = [pool("workers")];
        let records = records(&[]);
        let unplaced = [need("a")];
        let actions = plan(
            &pools,
            &[],
            &[],
            &unplaced,
            &records,
            &Spend::default(),
            NOW,
        );
        assert_eq!(
            actions,
            vec![Action::Create {
                pool: "workers".parse().unwrap(),
                name: name("e1"),
            }]
        );
    }

    #[test]
    fn demand_within_the_free_slots_creates_nothing() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            phase: Phase::Ready,
            last_busy: NOW,
            ..key("e1", Phase::Ready)
        }];
        let live = [HostView {
            active_agents: 1,
            ..view("e1", 1, true)
        }];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[need("a")],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Create { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn never_above_max_hosts() {
        let mut workers = pool("workers");
        workers.max_hosts = 1;
        // The one host is full, so the free slots do not cover the need.
        let hosts = [ElasticHost {
            phase: Phase::Ready,
            last_busy: NOW,
            billing_step_end: Timestamp(NOW.0 + 3600 * 1000),
            ..key("e1", Phase::Ready)
        }];
        let live = [view("e1", 2, true)];
        let actions = plan(
            &[workers],
            &hosts,
            &live,
            &[need("a")],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Create { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn never_above_the_pool_budget() {
        let pools = [pool("workers")];
        let spend = Spend {
            usd: 60.0,
            ..Spend::default()
        };
        let actions = plan(&pools, &[], &[], &[need("a")], &records(&[]), &spend, NOW);
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Create { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn never_above_the_monthly_budget() {
        let mut pools = [pool("workers")];
        pools[0].monthly_budget_usd = None;
        let spend = Spend {
            usd: 79.0,
            live_usd: 1.0,
            budget_usd: Some(80.0),
        };
        let actions = plan(&pools, &[], &[], &[need("a")], &records(&[]), &spend, NOW);
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Create { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn no_create_during_the_create_backoff() {
        let mut pools = [pool("workers")];
        pools[0].backoff_until = Some(Timestamp(NOW.0 + MIN));
        let actions = plan(
            &pools,
            &[],
            &[],
            &[need("a")],
            &records(&[]),
            &Spend::default(),
            NOW,
        );
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Create { .. })),
            "{actions:?}"
        );

        // The backoff is over, so the create happens.
        pools[0].backoff_until = Some(NOW);
        let actions = plan(
            &pools,
            &[],
            &[],
            &[need("a")],
            &records(&[]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions.len(), 1, "{actions:?}");
    }

    #[test]
    fn idle_host_waits_for_idle_timeout() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            last_busy: Timestamp(NOW.0 - 19 * MIN),
            billing_step_end: Timestamp(NOW.0 + MIN),
            ..key("e1", Phase::Ready)
        }];
        let live = [view("e1", 0, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn idle_host_waits_for_the_end_of_its_billing_step() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            billing_step_end: Timestamp(NOW.0 + 6 * MIN),
            ..idle_key("e1")
        }];
        let live = [view("e1", 0, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn an_idle_host_past_both_drains() {
        let pools = [pool("workers")];
        let hosts = [idle_key("e1")];
        let live = [view("e1", 0, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions, vec![Action::Drain { host: name("e1") }]);
    }

    #[test]
    fn a_drain_that_holds_a_lease_undrains_and_never_deletes() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            phase: Phase::Draining,
            ..idle_key("e1")
        }];
        let live = [view("e1", 1, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions, vec![Action::Undrain { host: name("e1") }]);
    }

    #[test]
    fn a_draining_host_with_no_lease_deletes() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            phase: Phase::Draining,
            ..idle_key("e1")
        }];
        let live = [view("e1", 0, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(
            actions,
            vec![Action::Delete {
                host: name("e1"),
                id: ProviderHostId("id-e1".to_owned()),
            }]
        );
    }

    #[test]
    fn a_host_with_no_key_is_adopted() {
        let pools = [pool("workers")];
        let actions = plan(
            &pools,
            &[],
            &[],
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(
            actions,
            vec![Action::Adopt {
                pool: "workers".parse().unwrap(),
                record: record("e1"),
            }]
        );
    }

    #[test]
    fn a_key_with_no_host_is_forgotten() {
        let pools = [pool("workers")];
        let hosts = [idle_key("e1")];
        let live = [view("e1", 0, true)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&[]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions, vec![Action::Forget { host: name("e1") }]);
    }

    #[test]
    fn no_heartbeat_for_lost_timeout_marks_lost_then_deletes() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            phase: Phase::Ready,
            last_seen: Timestamp(NOW.0 - 11 * MIN),
            last_busy: NOW,
            ..key("e1", Phase::Ready)
        }];
        let live = [view("e1", 0, false)];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions, vec![Action::MarkLost { host: name("e1") }]);

        // The next tick deletes the lost host.
        let hosts = [ElasticHost {
            phase: Phase::Lost,
            ..hosts[0].clone()
        }];
        let actions = plan(
            &pools,
            &hosts,
            &live,
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(
            actions,
            vec![Action::Delete {
                host: name("e1"),
                id: ProviderHostId("id-e1".to_owned()),
            }]
        );
    }

    #[test]
    fn a_host_that_misses_boot_timeout_marks_failed_then_deletes() {
        let pools = [pool("workers")];
        let hosts = [ElasticHost {
            phase: Phase::Booting,
            created: Timestamp(NOW.0 - 11 * MIN),
            billing_step_end: Timestamp(NOW.0 + 49 * MIN),
            ..key("e1", Phase::Booting)
        }];
        let actions = plan(
            &pools,
            &hosts,
            &[],
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(actions, vec![Action::MarkFailed { host: name("e1") }]);

        let hosts = [ElasticHost {
            phase: Phase::Failed,
            ..hosts[0].clone()
        }];
        let actions = plan(
            &pools,
            &hosts,
            &[],
            &[],
            &records(&["e1"]),
            &Spend::default(),
            NOW,
        );
        assert_eq!(
            actions,
            vec![Action::Delete {
                host: name("e1"),
                id: ProviderHostId("id-e1".to_owned()),
            }]
        );
    }

    #[test]
    fn choose_skips_a_pool_that_cannot_run_the_harness() {
        let mut pi = pool("pi-pool");
        pi.harnesses = vec![Harness::Pi];
        let pools = [pi, pool("workers")];
        let chosen = choose(&pools, &[], &Spend::default(), &need("a"), NOW);
        assert_eq!(chosen, Some("workers".parse().unwrap()));
    }

    #[test]
    fn choose_takes_the_cheapest_slot_hour() {
        let mut dear = pool("dear");
        // 0.3 an hour over 2 slots is 0.15 a slot-hour.
        dear.price_hour_usd = Some(0.3);
        let mut cheap = pool("cheap");
        // 0.12 an hour over 1 slot is 0.12 a slot-hour.
        cheap.price_hour_usd = Some(0.12);
        cheap.agents_per_host = 1;
        let pools = [dear, cheap.clone()];
        let chosen = choose(&pools, &[], &Spend::default(), &need("a"), NOW);
        assert_eq!(chosen, Some("cheap".parse().unwrap()));

        // A pool that reports no price sorts last.
        let mut unknown = pool("unknown");
        unknown.price_hour_usd = None;
        let pools = [unknown, cheap];
        let chosen = choose(&pools, &[], &Spend::default(), &need("a"), NOW);
        assert_eq!(chosen, Some("cheap".parse().unwrap()));
    }

    #[test]
    fn a_pool_host_is_named_with_the_prefix_and_no_collision() {
        let mut pools = [pool("workers")];
        pools[0].max_hosts = 3;
        pools[0].agents_per_host = 1;
        // The provider holds `e9`, so the names skip nothing under `e1`, `e2`.
        let found = records(&["e9"]);
        let actions = plan(
            &pools,
            &[],
            &[],
            &[need("a"), need("b")],
            &found,
            &Spend::default(),
            NOW,
        );
        let created: Vec<HostName> = actions
            .into_iter()
            .filter_map(|a| match a {
                Action::Create { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(created, vec![name("e1"), name("e2")]);
    }
}
