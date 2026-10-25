//! Placement of agents on hosts (spec section 14).

use std::cmp::Ordering;

use crate::ids::{BundleName, HostName, PoolName, Tag};
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

/// What a placement asks of a host: one host by name, the tags that a host
/// must have, and the tags that make a host more or less suitable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementReq {
    /// A host that the hub names. No other host qualifies.
    pub host: Option<HostName>,
    /// Tags that a host must have.
    pub require: Vec<Tag>,
    /// Tags that make a host more suitable.
    pub prefer: Vec<Tag>,
    /// Tags that make a host less suitable. A soft rule.
    pub avoid: Vec<Tag>,
}

fn qualifies(h: &HostView) -> bool {
    h.online && !h.info.draining && h.active_agents < h.info.max_agents && h.has_repo_access
}

/// How many of `tags` the host carries.
fn held(h: &HostView, tags: &[Tag]) -> usize {
    tags.iter().filter(|t| h.info.tags.contains(t)).count()
}

/// True if a host has every `tag` and defines every `bundle` of the launch.
fn fits(h: &HostView, req: &PlacementReq, bundles: &[BundleName]) -> bool {
    held(h, &req.require) == req.require.len() && bundles.iter().all(|b| h.info.bundles.contains(b))
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
/// more than a pool slot. Then the tags: a host with fewer `avoid` tags, then
/// one with more `prefer` tags. Then fill first: a static host (no pool) wins
/// over a pool host, and among pool hosts the one with more active agents
/// wins, so that a pool host goes idle and can be deleted. Among static hosts
/// the order is the one of before: fewer active agents, then always-on hosts,
/// then the name.
fn order(
    a: (&HostView, Harness),
    b: (&HostView, Harness),
    preferred: Harness,
    req: &PlacementReq,
) -> Ordering {
    let (x, y) = (a.0, b.0);
    let harness = (a.1 != preferred).cmp(&(b.1 != preferred));
    if harness != Ordering::Equal {
        return harness;
    }
    let avoid = held(x, &req.avoid).cmp(&held(y, &req.avoid));
    if avoid != Ordering::Equal {
        return avoid;
    }
    let prefer = held(y, &req.prefer).cmp(&held(x, &req.prefer));
    if prefer != Ordering::Equal {
        return prefer;
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
/// draining, below their limit, that can reach the repository, that have
/// every `require` tag of `req`, and that define every bundle of the launch.
/// The score is `order`.
pub fn place(
    preferred: Harness,
    forced_harness: Option<Harness>,
    req: &PlacementReq,
    bundles: &[BundleName],
    hosts: &[HostView],
) -> Option<(HostName, Harness)> {
    hosts
        .iter()
        .filter(|h| req.host.as_ref().is_none_or(|f| *f == h.info.name))
        .filter(|h| qualifies(h))
        .filter(|h| fits(h, req, bundles))
        .filter_map(|h| pick_harness(h, preferred, forced_harness).map(|harness| (h, harness)))
        .min_by(|a, b| order(*a, *b, preferred, req))
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
                ..Default::default()
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

    /// Tag names, for example `["vps"]`.
    fn tags(tags: &[&str]) -> Vec<Tag> {
        tags.iter().map(|t| t.parse().unwrap()).collect()
    }

    /// The host with these tags.
    fn tagged(mut h: HostView, held: &[&str]) -> HostView {
        h.info.tags = tags(held);
        h
    }

    /// The host with these bundles.
    fn bundled(mut h: HostView, held: &[&str]) -> HostView {
        h.info.bundles = held.iter().map(|b| b.parse().unwrap()).collect();
        h
    }

    /// A placement with no tags, no bundles, and no named host.
    fn pick(preferred: Harness, hosts: &[HostView]) -> Option<(HostName, Harness)> {
        place(preferred, None, &PlacementReq::default(), &[], hosts)
    }

    #[test]
    fn prefers_same_harness() {
        let hosts = [
            host("a", &[Harness::Pi], 0, true),
            host("b", &[Harness::ClaudeCode], 3, false),
        ];
        assert_eq!(
            pick(Harness::ClaudeCode, &hosts),
            Some((name("b"), Harness::ClaudeCode))
        );
        assert_eq!(pick(Harness::Omp, &hosts), Some((name("a"), Harness::Pi)));
    }

    #[test]
    fn prefers_fewer_agents_then_always_on() {
        let hosts = [
            host("a", &[Harness::Pi], 2, true),
            host("b", &[Harness::Pi], 1, false),
            host("c", &[Harness::Pi], 1, true),
        ];
        assert_eq!(pick(Harness::Pi, &hosts), Some((name("c"), Harness::Pi)));
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
        assert_eq!(pick(Harness::Pi, &[draining, full, offline, no_repo]), None);
    }

    #[test]
    fn forced_host_overrides_score() {
        let mut busy = host("a", &[Harness::Pi], 3, false);
        let idle = host("b", &[Harness::Pi], 0, true);
        let only_a = PlacementReq {
            host: Some(name("a")),
            ..Default::default()
        };
        assert_eq!(
            place(
                Harness::Pi,
                None,
                &only_a,
                &[],
                &[busy.clone(), idle.clone()]
            ),
            Some((name("a"), Harness::Pi))
        );
        busy.info.draining = true;
        assert_eq!(place(Harness::Pi, None, &only_a, &[], &[busy, idle]), None);
    }

    #[test]
    fn forced_harness_must_exist_on_host() {
        let hosts = [
            host("a", &[Harness::Pi], 0, true),
            host("b", &[Harness::Omp], 2, true),
        ];
        assert_eq!(
            place(
                Harness::Pi,
                Some(Harness::Omp),
                &PlacementReq::default(),
                &[],
                &hosts
            ),
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
            pick(Harness::ClaudeCode, &hosts),
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
            pick(Harness::ClaudeCode, &hosts),
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
        assert_eq!(pick(Harness::Pi, &hosts), Some((name("b"), Harness::Pi)));
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
        assert_eq!(pick(Harness::Pi, &hosts), Some((name("c"), Harness::Pi)));
        assert_eq!(
            pick(Harness::Pi, &hosts[..2]),
            Some((name("a"), Harness::Pi))
        );
    }

    #[test]
    fn a_require_tag_filters_hosts() {
        let hosts = [
            host("a", &[Harness::Pi], 0, true),
            tagged(host("b", &[Harness::Pi], 3, true), &["vps"]),
        ];
        let vps = PlacementReq {
            require: tags(&["vps"]),
            ..Default::default()
        };
        assert_eq!(
            place(Harness::Pi, None, &vps, &[], &hosts),
            Some((name("b"), Harness::Pi))
        );
        // A host must have every require tag.
        let both = PlacementReq {
            require: tags(&["vps", "gpu"]),
            ..Default::default()
        };
        assert_eq!(place(Harness::Pi, None, &both, &[], &hosts), None);
    }

    #[test]
    fn an_avoid_tag_is_soft() {
        let laptop = tagged(host("a", &[Harness::Pi], 0, true), &["laptop"]);
        let clean = tagged(host("b", &[Harness::Pi], 3, true), &["vps"]);
        let req = PlacementReq {
            avoid: tags(&["laptop"]),
            ..Default::default()
        };
        // The emptier host loses its place because of the tag.
        assert_eq!(
            place(Harness::Pi, None, &req, &[], &[laptop.clone(), clean]),
            Some((name("b"), Harness::Pi))
        );
        // Soft: a host that carries an avoid tag still qualifies alone.
        assert_eq!(
            place(Harness::Pi, None, &req, &[], &[laptop]),
            Some((name("a"), Harness::Pi))
        );
    }

    #[test]
    fn a_prefer_tag_orders_hosts() {
        // The pool host wins: prefer comes before the fill-first order.
        let plain = host("s", &[Harness::Pi], 0, true);
        let gpu = tagged(pool_host("p", &[Harness::Pi], 2), &["gpu"]);
        let req = PlacementReq {
            prefer: tags(&["gpu"]),
            ..Default::default()
        };
        assert_eq!(
            place(Harness::Pi, None, &req, &[], &[plain, gpu]),
            Some((name("p"), Harness::Pi))
        );
    }

    #[test]
    fn bundles_of_the_launch_filter_hosts() {
        let hosts = [
            bundled(host("a", &[Harness::Pi], 0, true), &["github-personal"]),
            bundled(host("b", &[Harness::Pi], 2, true), &["github-work"]),
        ];
        let work = vec!["github-work".parse().unwrap()];
        assert_eq!(
            place(Harness::Pi, None, &PlacementReq::default(), &work, &hosts),
            Some((name("b"), Harness::Pi))
        );
        // A host must define every bundle of the launch.
        let both: Vec<BundleName> = vec![
            "github-work".parse().unwrap(),
            "github-personal".parse().unwrap(),
        ];
        assert_eq!(
            place(Harness::Pi, None, &PlacementReq::default(), &both, &hosts),
            None
        );
    }

    #[test]
    fn an_empty_req_keeps_the_order_of_today() {
        // Tags on the hosts change nothing without a placement rule.
        let hosts = [
            tagged(host("a", &[Harness::Pi], 2, true), &["vps"]),
            pool_host("p", &[Harness::Pi], 0),
            tagged(host("b", &[Harness::Pi], 1, false), &["laptop"]),
            host("c", &[Harness::Pi], 1, true),
        ];
        assert_eq!(pick(Harness::Pi, &hosts), Some((name("c"), Harness::Pi)));
    }
}
