//! Alert rules (phase 2 spec, "Alerts").
//!
//! Pure: the hub gathers the inputs into an [`AlertView`] and compares the
//! alerts that are still true with the ones it has open. A rule with one key
//! per subject fires once and resolves once.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::AlertsConfig;
use crate::ids::{AgentId, BundleName, ChannelName, PoolName, TaskId};
use crate::model::{LeaseState, SwarmView};
use crate::ports::{Notice, Priority};
use crate::time::Timestamp;

/// The rule names. They are the prefix of each alert key.
pub const NEEDS_HUMAN: &str = "needs_human";
pub const AGENT_STALLED: &str = "agent_stalled";
pub const HOST_OFFLINE: &str = "host_offline";
pub const HOST_DISK: &str = "host_disk";
pub const MEMBERS_LIMITED: &str = "members_limited";
pub const BUDGET_STOP: &str = "budget_stop";
pub const PROVISIONER_REFUSED: &str = "provisioner_refused";

/// One fact about an agent that the live view does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAlert {
    pub agent: AgentId,
    /// A pinned agent keeps its tools through an outage and is never replaced,
    /// so a quiet turn is not a stall. The phase 0 pinned-agents feature.
    pub pinned: bool,
}

/// One task in `needs_human`, with the channel that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAlert {
    pub id: TaskId,
    pub channel: ChannelName,
    pub title: String,
}

/// One provisioner refusal that is still current: a create or a delete
/// returned `denied` or `invalid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub pool: PoolName,
    pub op: String,
    pub message: String,
}

/// What the hub passes to the rules: the live view, plus the facts that only
/// the hub keeps. The hub drops a fact when the condition is gone, which is
/// how the matching alert resolves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertView {
    pub swarm: SwarmView,
    /// Every agent with the flags that the exclusions need.
    pub agents: Vec<AgentAlert>,
    /// The tasks that are in `needs_human` now.
    pub needs_human: Vec<TaskAlert>,
    /// The bundles whose every member is limited now.
    pub limited_bundles: Vec<BundleName>,
    /// The pools that need a host and whose budget holds the host back.
    pub budget_blocked: Vec<PoolName>,
    /// The provisioner refusals still in force.
    pub refused: Vec<Refusal>,
    /// True while the hub is in its restart grace. A host that has not sent a
    /// heartbeat yet is expected then, so `host_offline` stays quiet.
    pub restart_grace: bool,
}

/// One change to the open alerts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertAction {
    Fire { key: String, notice: Notice },
    Resolve { key: String },
}

/// The key of one alert: the rule and the subject. One key per subject.
pub fn key(rule: &str, subject: &str) -> String {
    format!("{rule}:{subject}")
}

/// The alerts that are true now.
pub fn evaluate(
    view: &AlertView,
    open_alerts: &BTreeSet<String>,
    cfg: &AlertsConfig,
    now: Timestamp,
) -> Vec<AlertAction> {
    let active = active_alerts(view, cfg, now);
    let mut actions = Vec::new();
    for (key, notice) in &active {
        if !open_alerts.contains(key) {
            actions.push(AlertAction::Fire {
                key: key.clone(),
                notice: notice.clone(),
            });
        }
    }
    for key in open_alerts {
        if !active.contains_key(key) {
            actions.push(AlertAction::Resolve { key: key.clone() });
        }
    }
    actions
}

/// Every alert that is true now, by key, with the notice to send when it has
/// just fired.
pub fn active_alerts(
    view: &AlertView,
    cfg: &AlertsConfig,
    now: Timestamp,
) -> BTreeMap<String, Notice> {
    let mut active = BTreeMap::new();
    if cfg.needs_human {
        for task in &view.needs_human {
            active.insert(
                key(NEEDS_HUMAN, &task.id.to_string()),
                Notice::new(
                    "A task needs a human",
                    format!("{} in #{}", task.title, task.channel),
                )
                .with_priority(Priority::High)
                .with_tags([NEEDS_HUMAN.to_owned(), format!("#{}", task.channel)]),
            );
        }
    }
    let stall = minutes_duration(cfg.stall_minutes);
    let pinned: BTreeSet<&AgentId> = view
        .agents
        .iter()
        .filter(|a| a.pinned)
        .map(|a| &a.agent)
        .collect();
    for host in &view.swarm.hosts {
        let Some(stats) = &host.stats else {
            continue;
        };
        for session in &stats.sessions {
            let Some(last) = session.last_event_at else {
                continue;
            };
            if !session.busy || now.since(last) < stall {
                continue;
            }
            if pinned.contains(&session.agent) || is_parked(&view.swarm, &session.agent) {
                continue;
            }
            active.insert(
                key(AGENT_STALLED, session.agent.as_str()),
                Notice::new(
                    "An agent is stalled",
                    format!(
                        "{} has had no session event for {} minutes",
                        session.agent, cfg.stall_minutes
                    ),
                )
                .with_priority(Priority::High)
                .with_tags([AGENT_STALLED.to_owned()]),
            );
        }
    }
    if !view.restart_grace {
        let offline = minutes_duration(cfg.host_offline_minutes);
        for host in &view.swarm.hosts {
            if !host.info.always_on || host.info.pool.is_some() {
                continue;
            }
            let quiet = match host.last_seen {
                Some(seen) => now.since(seen) >= offline,
                // A host in the store that never sent a heartbeat.
                None => true,
            };
            if !quiet {
                continue;
            }
            active.insert(
                key(HOST_OFFLINE, host.info.name.as_str()),
                Notice::new(
                    "A host is offline",
                    format!(
                        "{} has sent no heartbeat for {} minutes",
                        host.info.name, cfg.host_offline_minutes
                    ),
                )
                .with_priority(Priority::High)
                .with_tags([HOST_OFFLINE.to_owned()]),
            );
        }
    }
    // A full disk stops the whole host, NATS included, so it is a warn at a
    // lower point than the placement refusal.
    for host in &view.swarm.hosts {
        let Some(used) = host.stats.as_ref().and_then(|s| s.disk_used_pct) else {
            continue;
        };
        if used < f64::from(cfg.disk_warn_pct) {
            continue;
        }
        active.insert(
            key(HOST_DISK, host.info.name.as_str()),
            Notice::new(
                "A host is low on disk",
                format!("{} is {used:.0}% full", host.info.name),
            )
            .with_priority(Priority::High)
            .with_tags([HOST_DISK.to_owned()]),
        );
    }
    for bundle in &view.limited_bundles {
        active.insert(
            key(MEMBERS_LIMITED, bundle.as_str()),
            Notice::new(
                "Every member is limited",
                format!("every member of {bundle} is limited"),
            )
            .with_tags([MEMBERS_LIMITED.to_owned()]),
        );
    }
    for pool in &view.budget_blocked {
        active.insert(
            key(BUDGET_STOP, pool.as_str()),
            Notice::new(
                "The budget stops a host",
                format!("{pool} needs a host and its budget is spent"),
            )
            .with_priority(Priority::High)
            .with_tags([BUDGET_STOP.to_owned()]),
        );
    }
    for refusal in &view.refused {
        active.insert(
            key(PROVISIONER_REFUSED, refusal.pool.as_str()),
            Notice::new(
                "A provisioner refused a host",
                format!("{}: {} ({})", refusal.op, refusal.message, refusal.pool),
            )
            .with_priority(Priority::High)
            .with_tags([PROVISIONER_REFUSED.to_owned()]),
        );
    }
    active
}

fn minutes_duration(minutes: u64) -> std::time::Duration {
    std::time::Duration::from_secs(minutes.saturating_mul(60))
}

/// A parked agent ended its session cleanly and sleeps until a message or a
/// claim wakes it, so it is not stalled.
fn is_parked(view: &SwarmView, agent: &AgentId) -> bool {
    view.agents.iter().any(|a| {
        &a.agent == agent && matches!(a.lease, Some(LeaseState::Released { parked: true, .. }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentStatus, HostInfo, HostStats, HostStatus, SessionStats, SwarmView};
    use crate::{AgentId, BundleName, HostName, PoolName, TaskId, Timestamp};

    const NOW: Timestamp = Timestamp(1_000_000_000);

    fn minutes(n: u64) -> Timestamp {
        NOW.minus(std::time::Duration::from_secs(n * 60))
    }

    fn cfg() -> AlertsConfig {
        AlertsConfig::default()
    }

    fn agent(name: &str) -> AgentId {
        name.parse().unwrap()
    }

    fn host(name: &str) -> HostName {
        name.parse().unwrap()
    }

    fn session(name: &str, busy: bool, last: Option<Timestamp>) -> SessionStats {
        SessionStats {
            agent: agent(name),
            busy,
            paused: false,
            turn: 0,
            last_event_at: last,
            wakes: 0,
            wake_limited: 0,
            ack_failed: 0,
            append_failed: 0,
            redelivered: 0,
            bad_envelope: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }
    }

    fn host_status(
        name: &str,
        always_on: bool,
        pool: Option<&str>,
        last_seen: Option<Timestamp>,
        sessions: Vec<SessionStats>,
    ) -> HostStatus {
        HostStatus {
            info: HostInfo {
                name: host(name),
                always_on,
                pool: pool.map(|p| p.parse::<PoolName>().unwrap()),
                ..Default::default()
            },
            online: last_seen.is_some(),
            last_seen,
            stats: Some(HostStats {
                version: "test".into(),
                sessions,
                ..Default::default()
            }),
        }
    }

    /// A host that is reachable now, with a disk at `used_pct`.
    fn host_disk_status(name: &str, used_pct: Option<f64>) -> HostStatus {
        let mut status = host_status(name, true, None, Some(minutes(1)), Vec::new());
        status.stats = Some(HostStats {
            version: "test".into(),
            disk_used_pct: used_pct,
            ..HostStats::default()
        });
        status
    }

    fn view_with(hosts: Vec<HostStatus>) -> AlertView {
        AlertView {
            swarm: SwarmView {
                hosts,
                at: Some(NOW),
                ..SwarmView::default()
            },
            ..AlertView::default()
        }
    }

    fn fired(actions: &[AlertAction]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                AlertAction::Fire { key, .. } => Some(key.as_str()),
                AlertAction::Resolve { .. } => None,
            })
            .collect()
    }

    fn resolved(actions: &[AlertAction]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                AlertAction::Resolve { key } => Some(key.as_str()),
                AlertAction::Fire { .. } => None,
            })
            .collect()
    }

    #[test]
    fn needs_human_fires_once_and_resolves() {
        let mut view = view_with(Vec::new());
        view.needs_human.push(TaskAlert {
            id: TaskId::new(),
            channel: "api".parse().unwrap(),
            title: "decide".into(),
        });
        let actions = evaluate(&view, &BTreeSet::new(), &cfg(), NOW);
        let key = format!("needs_human:{}", view.needs_human[0].id);
        assert_eq!(fired(&actions), vec![key.as_str()]);
        assert!(!actions.is_empty());
        let AlertAction::Fire { notice, .. } = &actions[0] else {
            panic!("a fire")
        };
        assert_eq!(notice.priority, Priority::High);

        // The same state again fires nothing.
        let open = BTreeSet::from([key.clone()]);
        assert!(evaluate(&view, &open, &cfg(), NOW).is_empty());

        // The task left needs_human: one resolve.
        let empty = view_with(Vec::new());
        let actions = evaluate(&empty, &open, &cfg(), NOW);
        assert_eq!(resolved(&actions), vec![key.as_str()]);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn needs_human_can_be_switched_off() {
        let mut view = view_with(Vec::new());
        view.needs_human.push(TaskAlert {
            id: TaskId::new(),
            channel: "api".parse().unwrap(),
            title: "decide".into(),
        });
        let mut cfg = cfg();
        cfg.needs_human = false;
        assert!(evaluate(&view, &BTreeSet::new(), &cfg, NOW).is_empty());
    }

    #[test]
    fn agent_stalled_fires_after_the_stall() {
        let view = view_with(vec![host_status(
            "h1",
            false,
            None,
            Some(NOW),
            vec![session("w1", true, Some(minutes(31)))],
        )]);
        assert_eq!(
            fired(&evaluate(&view, &BTreeSet::new(), &cfg(), NOW)),
            vec!["agent_stalled:w1"]
        );

        // A busy session with a recent event is not stalled.
        let view = view_with(vec![host_status(
            "h1",
            false,
            None,
            Some(NOW),
            vec![session("w1", true, Some(minutes(29)))],
        )]);
        assert!(evaluate(&view, &BTreeSet::new(), &cfg(), NOW).is_empty());

        // Nor is an idle session.
        let view = view_with(vec![host_status(
            "h1",
            false,
            None,
            Some(NOW),
            vec![session("w1", false, Some(minutes(60)))],
        )]);
        assert!(evaluate(&view, &BTreeSet::new(), &cfg(), NOW).is_empty());
    }

    #[test]
    fn agent_stalled_skips_a_pinned_agent() {
        let mut view = view_with(vec![host_status(
            "h1",
            false,
            None,
            Some(NOW),
            vec![session("w1", true, Some(minutes(60)))],
        )]);
        view.agents.push(AgentAlert {
            agent: agent("w1"),
            pinned: true,
        });
        assert!(evaluate(&view, &BTreeSet::new(), &cfg(), NOW).is_empty());
    }

    #[test]
    fn agent_stalled_skips_a_parked_agent() {
        let mut view = view_with(vec![host_status(
            "h1",
            false,
            None,
            Some(NOW),
            vec![session("w1", true, Some(minutes(60)))],
        )]);
        view.swarm.agents.push(AgentStatus {
            agent: agent("w1"),
            harness: None,
            host: None,
            lease: Some(LeaseState::Released {
                since: minutes(5),
                parked: true,
            }),
            session: None,
            busy: true,
            claim: None,
            role: None,
            collection: None,
            pinned: false,
            retired: false,
            model: None,
        });
        assert!(evaluate(&view, &BTreeSet::new(), &cfg(), NOW).is_empty());
    }

    #[test]
    fn host_offline_fires_after_the_timeout() {
        let view = view_with(vec![host_status(
            "vps-1",
            true,
            None,
            Some(minutes(6)),
            Vec::new(),
        )]);
        assert_eq!(
            fired(&evaluate(&view, &BTreeSet::new(), &cfg(), NOW)),
            vec!["host_offline:vps-1"]
        );

        // A recent heartbeat, a host that is not always on, or a pool host
        // stays quiet.
        let recent = view_with(vec![host_status(
            "vps-1",
            true,
            None,
            Some(minutes(1)),
            Vec::new(),
        )]);
        assert!(evaluate(&recent, &BTreeSet::new(), &cfg(), NOW).is_empty());
        let laptop = view_with(vec![host_status(
            "laptop",
            false,
            None,
            Some(minutes(60)),
            Vec::new(),
        )]);
        assert!(evaluate(&laptop, &BTreeSet::new(), &cfg(), NOW).is_empty());
        let pool = view_with(vec![host_status(
            "e-1",
            true,
            Some("workers"),
            Some(minutes(60)),
            Vec::new(),
        )]);
        assert!(evaluate(&pool, &BTreeSet::new(), &cfg(), NOW).is_empty());
    }

    #[test]
    fn host_offline_is_quiet_in_the_restart_grace() {
        let mut view = view_with(vec![host_status(
            "vps-1",
            true,
            None,
            Some(minutes(6)),
            Vec::new(),
        )]);
        view.restart_grace = true;
        assert!(evaluate(&view, &BTreeSet::new(), &cfg(), NOW).is_empty());
    }

    /// Rule: `host_disk` fires at the warn point, once for each host, and
    /// resolves when the disk drops below it.
    #[test]
    fn host_disk_fires_and_resolves() {
        let cfg = cfg();
        let open = BTreeSet::new();
        let at = view_with(vec![host_disk_status("vps-1", Some(85.0))]);
        assert_eq!(
            fired(&evaluate(&at, &open, &cfg, NOW)),
            vec!["host_disk:vps-1"],
            "85% is the default warn point"
        );
        // One notice while the host stays full.
        let already: BTreeSet<String> = ["host_disk:vps-1".to_owned()].into();
        assert!(evaluate(&at, &already, &cfg, NOW).is_empty());

        // Below the point it resolves, once.
        let below = view_with(vec![host_disk_status("vps-1", Some(70.0))]);
        assert_eq!(
            evaluate(&below, &already, &cfg, NOW),
            vec![AlertAction::Resolve {
                key: "host_disk:vps-1".to_owned()
            }]
        );

        // A host with no stats says nothing, and the file sets the point.
        let unknown = view_with(vec![host_disk_status("vps-1", None)]);
        assert!(evaluate(&unknown, &open, &cfg, NOW).is_empty());
        let strict = AlertsConfig {
            disk_warn_pct: 60,
            ..cfg
        };
        let above = view_with(vec![host_disk_status("vps-1", Some(61.0))]);
        assert_eq!(
            fired(&evaluate(&above, &open, &strict, NOW)),
            vec!["host_disk:vps-1"]
        );
    }

    #[test]
    fn members_limited_fires_and_resolves_when_one_resets() {
        let mut view = view_with(Vec::new());
        view.limited_bundles
            .push("github-personal".parse().unwrap());
        let actions = evaluate(&view, &BTreeSet::new(), &cfg(), NOW);
        let key = "members_limited:github-personal".to_owned();
        assert_eq!(fired(&actions), vec![key.as_str()]);

        let open = BTreeSet::from([key.clone()]);
        assert!(evaluate(&view, &open, &cfg(), NOW).is_empty());

        let reset = view_with(Vec::new());
        assert_eq!(
            resolved(&evaluate(&reset, &open, &cfg(), NOW)),
            vec![key.as_str()]
        );
    }

    #[test]
    fn budget_stop_fires_for_a_blocked_pool() {
        let mut view = view_with(Vec::new());
        view.budget_blocked.push("workers".parse().unwrap());
        let actions = evaluate(&view, &BTreeSet::new(), &cfg(), NOW);
        assert_eq!(fired(&actions), vec!["budget_stop:workers"]);
        let AlertAction::Fire { notice, .. } = &actions[0] else {
            panic!("a fire")
        };
        assert_eq!(notice.priority, Priority::High);
    }

    #[test]
    fn provisioner_refused_fires_for_a_current_refusal() {
        let mut view = view_with(Vec::new());
        view.refused.push(Refusal {
            pool: "workers".parse().unwrap(),
            op: "create".into(),
            message: "denied: no quota".into(),
        });
        let actions = evaluate(&view, &BTreeSet::new(), &cfg(), NOW);
        assert_eq!(fired(&actions), vec!["provisioner_refused:workers"]);
        let AlertAction::Fire { notice, .. } = &actions[0] else {
            panic!("a fire")
        };
        assert!(notice.body.contains("no quota"));

        // The hub drops the refusal when a later call succeeds.
        let clear = view_with(Vec::new());
        let open = BTreeSet::from(["provisioner_refused:workers".to_owned()]);
        assert_eq!(
            resolved(&evaluate(&clear, &open, &cfg(), NOW)),
            vec!["provisioner_refused:workers"]
        );
    }

    #[test]
    fn a_bundle_name_is_a_subject_of_members_limited() {
        let bundle: BundleName = "work".parse().unwrap();
        assert_eq!(
            key(MEMBERS_LIMITED, bundle.as_str()),
            "members_limited:work"
        );
    }
}
