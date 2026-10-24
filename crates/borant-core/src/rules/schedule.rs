//! Placement of agents on hosts (spec section 14).

use crate::ids::HostName;
use crate::model::{Harness, HostInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostView {
    pub info: HostInfo,
    pub online: bool,
    pub active_agents: u32,
    /// False if the host reported that it cannot reach the git remote of the
    /// agent. True when the agent has no repository.
    pub has_repo_access: bool,
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

/// Picks a host and a harness. The filter keeps hosts that are online, not
/// draining, below their limit, and that can reach the repository. The score
/// prefers the same harness, then fewer active agents, then always-on hosts.
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
        .min_by_key(|(h, harness)| {
            (
                *harness != preferred,
                h.active_agents,
                !h.info.always_on,
                h.info.name.clone(),
            )
        })
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
            },
            online: true,
            active_agents: active,
            has_repo_access: true,
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
}
