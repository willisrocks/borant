//! Placement of agents on hosts (spec section 14).

use std::cmp::Ordering;

use crate::ids::{HostName, PoolName};
use crate::model::{Harness, HostInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostView {
    pub info: HostInfo,
    pub online: bool,
    pub active_agents: u32,
    /// False if the host reported that it cannot reach the git remote of the
    /// agent. True when the agent has no repository.
    pub has_repo_access: bool,
    /// The pool that created the host. `None` for a static host.
    pub pool: Option<PoolName>,
}

fn qualifies(h: &HostView) -> bool {
    h.online && !h.info.draining && h.active_agents < h.info.max_agents && h.has_repo_access
}

fn pick_harness(h: &HostView, preferred: Harness, forced: Option<Harness>) -> Option<Harness> {
    match forced {
        Some(f) => h.info.harnesses.contains(&f).then_some(f),
        None if h.info.harnesses.contains(&preferred) => Some(preferred),
        None => h.info.harnesses.first().copied(),
    }
}

/// Orders two candidates. The requested harness comes first: a host that runs
/// another harness changes the model and the cost of the agent, which costs
/// more than a pool slot. Then fill first: a static host (no pool) wins over a
/// pool host, and among pool hosts the one with more active agents wins, so
/// that a pool host goes idle and can be deleted. Among static hosts the order
/// is the one of before: fewer active agents, then always-on hosts, then the
/// name.
fn order(a: (&HostView, Harness), b: (&HostView, Harness), preferred: Harness) -> Ordering {
    let (x, y) = (a.0, b.0);
    let harness = (a.1 != preferred).cmp(&(b.1 != preferred));
    if harness != Ordering::Equal {
        return harness;
    }
    match (x.pool.is_some(), y.pool.is_some()) {
        (false, true) => return Ordering::Less,
        (true, false) => return Ordering::Greater,
        (true, true) => {
            return y
                .active_agents
                .cmp(&x.active_agents)
                .then_with(|| x.info.name.cmp(&y.info.name));
        }
        (false, false) => {}
    }
    x.active_agents
        .cmp(&y.active_agents)
        .then_with(|| (!x.info.always_on).cmp(&(!y.info.always_on)))
        .then_with(|| x.info.name.cmp(&y.info.name))
}

/// Picks a host and a harness. The filter keeps hosts that are online, not
/// draining, below their limit, and that can reach the repository. The score
/// is `order`.
pub fn place(
    preferred: Harness,
    forced_host: Option<&HostName>,
    forced_harness: Option<Harness>,
    hosts: &[HostView],
) -> Option<(HostName, Harness)> {
    hosts
        .iter()
        .filter(|h| forced_host.is_none_or(|f| *f == h.info.name))
        .filter(|h| qualifies(h))
        .filter_map(|h| pick_harness(h, preferred, forced_harness).map(|harness| (h, harness)))
        .min_by(|a, b| order(*a, *b, preferred))
        .map(|(h, harness)| (h.info.name.clone(), harness))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, harnesses: &[Harness], active: u32, always_on: bool) -> HostView {
        HostView {
            info: HostInfo {
                name: name.parse().unwrap(),
                harnesses: harnesses.to_vec(),
                launcher: "tmux".into(),
                max_agents: 4,
                always_on,
                draining: false,
                ssh: None,
                members: Default::default(),
                pool: None,
                tags: Vec::new(),
            },
            online: true,
            active_agents: active,
            has_repo_access: true,
            pool: None,
        }
    }

    /// A host of the `workers` pool.
    fn pool_host(name: &str, harnesses: &[Harness], active: u32) -> HostView {
        HostView {
            pool: Some("workers".parse().unwrap()),
            ..host(name, harnesses, active, false)
        }
    }

    fn name(s: &str) -> HostName {
        s.parse().unwrap()
    }

    #[test]
    fn prefers_same_harness() {
        let hosts = [
            host("a", &[Harness::Pi], 0, true),
            host("b", &[Harness::ClaudeCode], 3, false),
        ];
        assert_eq!(
            place(Harness::ClaudeCode, None, None, &hosts),
            Some((name("b"), Harness::ClaudeCode))
        );
        assert_eq!(
            place(Harness::Omp, None, None, &hosts),
            Some((name("a"), Harness::Pi))
        );
    }

    #[test]
    fn prefers_fewer_agents_then_always_on() {
        let hosts = [
            host("a", &[Harness::Pi], 2, true),
            host("b", &[Harness::Pi], 1, false),
            host("c", &[Harness::Pi], 1, true),
        ];
        assert_eq!(
            place(Harness::Pi, None, None, &hosts),
            Some((name("c"), Harness::Pi))
        );
    }

    #[test]
    fn skips_draining_full_offline_hosts() {
        let mut draining = host("a", &[Harness::Pi], 0, true);
        draining.info.draining = true;
        let full = host("b", &[Harness::Pi], 4, true);
        let mut offline = host("c", &[Harness::Pi], 0, true);
        offline.online = false;
        let mut no_repo = host("d", &[Harness::Pi], 0, true);
        no_repo.has_repo_access = false;
        assert_eq!(
            place(Harness::Pi, None, None, &[draining, full, offline, no_repo]),
            None
        );
    }

    #[test]
    fn forced_host_overrides_score() {
        let mut busy = host("a", &[Harness::Pi], 3, false);
        let idle = host("b", &[Harness::Pi], 0, true);
        assert_eq!(
            place(
                Harness::Pi,
                Some(&name("a")),
                None,
                &[busy.clone(), idle.clone()]
            ),
            Some((name("a"), Harness::Pi))
        );
        busy.info.draining = true;
        assert_eq!(
            place(Harness::Pi, Some(&name("a")), None, &[busy, idle]),
            None
        );
    }

    #[test]
    fn forced_harness_must_exist_on_host() {
        let hosts = [
            host("a", &[Harness::Pi], 0, true),
            host("b", &[Harness::Omp], 2, true),
        ];
        assert_eq!(
            place(Harness::Pi, None, Some(Harness::Omp), &hosts),
            Some((name("b"), Harness::Omp))
        );
    }

    #[test]
    fn static_host_beats_pool_host() {
        // The static host is already paid for, even when it is busier than
        // the pool host.
        let hosts = [
            pool_host("p", &[Harness::ClaudeCode], 0),
            host("s", &[Harness::ClaudeCode], 3, true),
        ];
        assert_eq!(
            place(Harness::ClaudeCode, None, None, &hosts),
            Some((name("s"), Harness::ClaudeCode))
        );
    }

    #[test]
    fn requested_harness_beats_a_static_host() {
        // Another harness changes the model and the cost of the agent, which
        // costs more than a pool slot.
        let hosts = [
            host("s", &[Harness::Pi], 0, true),
            pool_host("p", &[Harness::ClaudeCode], 2),
        ];
        assert_eq!(
            place(Harness::ClaudeCode, None, None, &hosts),
            Some((name("p"), Harness::ClaudeCode))
        );
    }

    #[test]
    fn busier_pool_host_beats_emptier_one() {
        let hosts = [
            pool_host("a", &[Harness::Pi], 0),
            pool_host("b", &[Harness::Pi], 3),
            pool_host("c", &[Harness::Pi], 1),
        ];
        assert_eq!(
            place(Harness::Pi, None, None, &hosts),
            Some((name("b"), Harness::Pi))
        );
    }

    #[test]
    fn static_hosts_keep_todays_order() {
        // Among static hosts the emptiest always-on host still wins; the pool
        // host does not change that.
        let hosts = [
            host("a", &[Harness::Pi], 2, true),
            pool_host("p", &[Harness::Pi], 0),
            host("b", &[Harness::Pi], 1, false),
            host("c", &[Harness::Pi], 1, true),
        ];
        assert_eq!(
            place(Harness::Pi, None, None, &hosts),
            Some((name("c"), Harness::Pi))
        );
        assert_eq!(
            place(Harness::Pi, None, None, &hosts[..2]),
            Some((name("a"), Harness::Pi))
        );
    }
}
