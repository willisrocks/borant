//! Placement of agents on hosts (spec section 14).

use std::cmp::Ordering;
use std::fmt;

use crate::ids::{BundleName, HostName, PoolName, Tag};
use crate::model::{Harness, HostInfo, SandboxKind};

/// The memory that a host keeps free when `reserve_mem_mb` is unset, and the
/// reserve of a v0.7.0 host that reports none.
pub const DEFAULT_RESERVE_MEM_MB: u32 = 1024;

/// The disk percentage at which a host takes no new agent, when the request
/// does not set its own.
pub const DEFAULT_DISK_STOP_PCT: u8 = 95;

#[derive(Debug, Clone, PartialEq)]
pub struct HostView {
    pub info: HostInfo,
    pub online: bool,
    pub active_agents: u32,
    /// False if the host reported that it cannot reach the git remote of the
    /// agent. True when the agent has no repository.
    pub has_repo_access: bool,
    /// True if a move to this host failed recently, for example because its
    /// clone could not reach the remote.
    pub recently_failed: bool,
    /// The pool that created the host. `None` for a static host.
    pub pool: Option<PoolName>,
    /// The memory available on the host, from its last heartbeat. `None` for a
    /// host that reports no stats, which passes the memory rule.
    pub mem_avail_mb: Option<u64>,
    /// How full the disk of the host is, from its last heartbeat. `None` for a
    /// host that reports no stats, which passes the disk rule.
    pub disk_used_pct: Option<f64>,
}

/// Why a host did not take an agent. The first failed rule of a host is its
/// reason, and `Display` is what `borant ps --queued` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    Offline,
    Draining,
    Full {
        active: u32,
        max: u32,
    },
    NoRepo,
    NoHarness,
    MissingTag(Tag),
    MissingBundle(BundleName),
    LowMemory {
        avail_mb: u64,
        reserve_mb: u32,
    },
    /// The disk of the host is at or above the stop point, a whole percent.
    LowDisk {
        used_pct: u8,
    },
    NoSandbox,
    RecentlyFailed,
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("offline"),
            Self::Draining => f.write_str("draining"),
            Self::Full { active, max } => write!(f, "full ({active}/{max})"),
            Self::NoRepo => f.write_str("cannot reach the repo"),
            Self::NoHarness => f.write_str("no harness"),
            Self::MissingTag(tag) => write!(f, "missing tag {tag}"),
            Self::MissingBundle(bundle) => write!(f, "missing bundle {bundle}"),
            Self::LowMemory {
                avail_mb,
                reserve_mb,
            } => write!(f, "low memory ({avail_mb} MB free, {reserve_mb} reserved)"),
            Self::LowDisk { used_pct } => write!(f, "disk full ({used_pct}%)"),
            Self::NoSandbox => f.write_str("no sandbox"),
            Self::RecentlyFailed => f.write_str("failed recently"),
        }
    }
}

/// What a placement asks of a host: one host by name, the tags that a host
/// must have, the tags that make a host more or less suitable, and the sandbox
/// that the launch needs.
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
    /// The sandbox that the launch needs. `None` needs none.
    pub sandbox: Option<SandboxKind>,
    /// The disk percentage at which a host takes no new agent. `None` uses
    /// [`DEFAULT_DISK_STOP_PCT`].
    pub disk_stop_pct: Option<u8>,
}

/// How many of `tags` the host carries.
fn held(h: &HostView, tags: &[Tag]) -> usize {
    tags.iter().filter(|t| h.info.tags.contains(t)).count()
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

/// The first rule that `h` fails, in the order of the rules: the reason that
/// `borant ps --queued` prints for it.
fn reject(
    h: &HostView,
    req: &PlacementReq,
    bundles: &[BundleName],
    forced_harness: Option<Harness>,
) -> Option<Reject> {
    if !h.online {
        return Some(Reject::Offline);
    }
    if h.info.draining {
        return Some(Reject::Draining);
    }
    if h.active_agents >= h.info.max_agents {
        return Some(Reject::Full {
            active: h.active_agents,
            max: h.info.max_agents,
        });
    }
    if !h.has_repo_access {
        return Some(Reject::NoRepo);
    }
    if h.recently_failed {
        return Some(Reject::RecentlyFailed);
    }
    if forced_harness.is_some_and(|f| !h.info.harnesses.contains(&f)) {
        return Some(Reject::NoHarness);
    }
    if let Some(tag) = req.require.iter().find(|t| !h.info.tags.contains(t)) {
        return Some(Reject::MissingTag(tag.clone()));
    }
    if let Some(bundle) = bundles.iter().find(|b| !h.info.bundles.contains(b)) {
        return Some(Reject::MissingBundle(bundle.clone()));
    }
    // A host that reports no memory passes: a v0.7.0 host keeps working.
    if let Some(avail_mb) = h.mem_avail_mb {
        let reserve_mb = h.info.reserve_mem_mb.unwrap_or(DEFAULT_RESERVE_MEM_MB);
        if avail_mb < u64::from(reserve_mb) {
            return Some(Reject::LowMemory {
                avail_mb,
                reserve_mb,
            });
        }
    }
    // A host that reports no disk passes, as with the memory rule.
    if let Some(used) = h.disk_used_pct {
        let stop = req.disk_stop_pct.unwrap_or(DEFAULT_DISK_STOP_PCT);
        if used >= f64::from(stop) {
            let used_pct = used.round().clamp(0.0, 100.0) as u8;
            return Some(Reject::LowDisk { used_pct });
        }
    }
    if let Some(kind) = req.sandbox
        && !h.info.sandboxes.contains(&kind)
    {
        return Some(Reject::NoSandbox);
    }
    None
}

/// Picks a host and a harness. A host qualifies when no rule of `reject`
/// fails, that is: it is online, not draining, below its limit, can reach the
/// repository, was not recently failed, has the forced harness, every
/// `require` tag of `req`, every bundle of the launch, enough free memory for
/// its reserve, enough free disk, and the sandbox of the request. The score is
/// `order`.
///
/// The error lists every host that was asked, with its first failed rule, so
/// that a refused placement can name them all.
pub fn place(
    preferred: Harness,
    forced_harness: Option<Harness>,
    req: &PlacementReq,
    bundles: &[BundleName],
    hosts: &[HostView],
) -> Result<(HostName, Harness), Vec<(HostName, Reject)>> {
    let mut rejected: Vec<(HostName, Reject)> = Vec::new();
    let candidates: Vec<(&HostView, Harness)> = hosts
        .iter()
        .filter(|h| req.host.as_ref().is_none_or(|f| *f == h.info.name))
        .filter_map(|h| match reject(h, req, bundles, forced_harness) {
            Some(reason) => {
                rejected.push((h.info.name.clone(), reason));
                None
            }
            None => pick_harness(h, preferred, forced_harness).map(|harness| (h, harness)),
        })
        .collect();
    candidates
        .into_iter()
        .min_by(|a, b| order(*a, *b, preferred, req))
        .map(|(h, harness)| (h.info.name.clone(), harness))
        .ok_or(rejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HostStats, SandboxKind, Sender, SpawnRecord, SpawnState};
    use crate::rules::roles::{Resolved, RoleConfig};
    use crate::time::Timestamp;

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
            recently_failed: false,
            pool: None,
            mem_avail_mb: None,
            disk_used_pct: None,
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

    /// `place` as an option, where a test only asks which host wins: `Some` is
    /// a placement and `None` a refusal. The tests of the reject reasons call
    /// `super::place` for the reasons themselves.
    fn place(
        preferred: Harness,
        forced: Option<Harness>,
        req: &PlacementReq,
        bundles: &[BundleName],
        hosts: &[HostView],
    ) -> Option<(HostName, Harness)> {
        super::place(preferred, forced, req, bundles, hosts).ok()
    }

    /// A placement with no tags, no bundles, and no named host, as an option:
    /// the score tests ask which host wins, not why the others lost.
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
        let hosts = [draining, full, offline, no_repo];
        assert_eq!(pick(Harness::Pi, &hosts), None);
        // Each host names its first failed rule.
        let err =
            super::place(Harness::Pi, None, &PlacementReq::default(), &[], &hosts).unwrap_err();
        let reasons: Vec<String> = err.iter().map(|(h, r)| format!("{h}: {r}")).collect();
        assert_eq!(
            reasons,
            vec![
                "a: draining",
                "b: full (4/4)",
                "c: offline",
                "d: cannot reach the repo"
            ]
        );
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

    /// A host that passes every rule of the reject chain of the tests below.
    fn ready(host_name: &str) -> HostView {
        let mut h = tagged(host(host_name, &[Harness::Omp], 0, true), &["vps"]);
        h.info.bundles = vec!["github-work".parse().unwrap()];
        h.info.reserve_mem_mb = Some(DEFAULT_RESERVE_MEM_MB);
        h.info.sandboxes = vec![SandboxKind::Namespace];
        h.mem_avail_mb = Some(4096);
        h
    }

    /// What the tests of the reject reasons ask: a vps host with the bundle,
    /// the forced harness, and the namespace sandbox.
    fn ready_req() -> PlacementReq {
        PlacementReq {
            require: tags(&["vps"]),
            sandbox: Some(SandboxKind::Namespace),
            ..Default::default()
        }
    }

    fn ready_bundles() -> Vec<BundleName> {
        vec!["github-work".parse().unwrap()]
    }

    /// Rule: a reject names the first failed rule of each host, in the order
    /// of the rules.
    #[test]
    fn a_reject_lists_every_host_in_the_order_of_the_rules() {
        let mut offline = ready("offline");
        offline.online = false;
        let mut draining = ready("draining");
        draining.info.draining = true;
        let mut full = ready("full");
        full.active_agents = 4;
        let mut no_repo = ready("no-repo");
        no_repo.has_repo_access = false;
        let mut failed = ready("failed");
        failed.recently_failed = true;
        let no_harness = tagged(host("no-harness", &[Harness::Pi], 0, true), &["vps"]);
        let no_tag = {
            let mut h = ready("no-tag");
            h.info.tags = Vec::new();
            h
        };
        let no_bundle = {
            let mut h = ready("no-bundle");
            h.info.bundles = Vec::new();
            h
        };
        let low_memory = {
            let mut h = ready("low-memory");
            h.mem_avail_mb = Some(700);
            h
        };
        let full_disk = {
            let mut h = ready("full-disk");
            h.disk_used_pct = Some(96.0);
            h
        };
        let no_sandbox = {
            let mut h = ready("no-sandbox");
            h.info.sandboxes = Vec::new();
            h
        };
        let hosts = [
            offline, draining, full, no_repo, failed, no_harness, no_tag, no_bundle, low_memory,
            full_disk, no_sandbox,
        ];
        let err = super::place(
            Harness::Omp,
            Some(Harness::Omp),
            &ready_req(),
            &ready_bundles(),
            &hosts,
        )
        .unwrap_err();
        let reasons: Vec<String> = err.iter().map(|(h, r)| format!("{h}: {r}")).collect();
        assert_eq!(
            reasons,
            vec![
                "offline: offline",
                "draining: draining",
                "full: full (4/4)",
                "no-repo: cannot reach the repo",
                "failed: failed recently",
                "no-harness: no harness",
                "no-tag: missing tag vps",
                "no-bundle: missing bundle github-work",
                "low-memory: low memory (700 MB free, 1024 reserved)",
                "full-disk: disk full (96%)",
                "no-sandbox: no sandbox",
            ]
        );
    }

    /// Rule: a host with too little free memory takes no new agent, and a
    /// host with no stats passes, so a v0.7.0 host keeps working.
    #[test]
    fn low_memory_needs_stats() {
        let mut low = ready("low");
        low.mem_avail_mb = Some(700);
        let place_one = |h: HostView| {
            super::place(
                Harness::Omp,
                Some(Harness::Omp),
                &ready_req(),
                &ready_bundles(),
                &[h],
            )
        };
        let err = place_one(low).expect_err("700 MB is under the reserve");
        assert_eq!(
            err[0].1.to_string(),
            "low memory (700 MB free, 1024 reserved)"
        );

        // Enough memory takes the agent.
        let mut enough = ready("enough");
        enough.mem_avail_mb = Some(2048);
        assert!(place_one(enough).is_ok(), "2 GB is above the reserve");

        // No stats: the memory rule passes.
        let mut old = ready("old");
        old.mem_avail_mb = None;
        assert!(place_one(old).is_ok(), "unknown memory passes");

        // The reserve of the host is what counts.
        let mut big = ready("big");
        big.mem_avail_mb = Some(2048);
        big.info.reserve_mem_mb = Some(4096);
        let err = place_one(big).expect_err("2 GB is under a 4 GB reserve");
        assert_eq!(
            err[0].1.to_string(),
            "low memory (2048 MB free, 4096 reserved)"
        );
    }

    /// Rule: a host at or above the disk stop point takes no new agent, and a
    /// host with no stats passes.
    #[test]
    fn low_disk_needs_stats() {
        let place_one = |h: HostView, req: &PlacementReq| {
            super::place(
                Harness::Omp,
                Some(Harness::Omp),
                req,
                &ready_bundles(),
                &[h],
            )
        };
        // 96% is over the default stop point of 95%.
        let mut full = ready("full");
        full.disk_used_pct = Some(96.0);
        let err = place_one(full, &ready_req()).expect_err("96% is full");
        assert_eq!(err[0].1.to_string(), "disk full (96%)");

        // 94% still takes the agent.
        let mut nearly = ready("nearly");
        nearly.disk_used_pct = Some(94.0);
        assert!(place_one(nearly, &ready_req()).is_ok(), "94% is not full");

        // No stats: the disk rule passes.
        let old = ready("old");
        assert!(place_one(old, &ready_req()).is_ok(), "unknown disk passes");

        // A request can set its own stop point.
        let mut high = ready("high");
        high.disk_used_pct = Some(92.0);
        let strict = PlacementReq {
            disk_stop_pct: Some(90),
            ..ready_req()
        };
        let err = place_one(high, &strict).expect_err("92% is over a 90% stop");
        assert_eq!(err[0].1, Reject::LowDisk { used_pct: 92 });
    }

    /// Rule: a launch that asks for a sandbox needs a host that offers it.
    #[test]
    fn a_sandbox_must_be_offered() {
        let mut plain = ready("plain");
        plain.info.sandboxes = Vec::new();
        let req = ready_req();
        let err = super::place(
            Harness::Omp,
            Some(Harness::Omp),
            &req,
            &ready_bundles(),
            &[plain.clone()],
        )
        .expect_err("the host offers no sandbox");
        assert_eq!(err[0].1, Reject::NoSandbox);
        // A launch that asks for none lands there.
        assert!(
            super::place(
                Harness::Omp,
                Some(Harness::Omp),
                &PlacementReq::default(),
                &[],
                &[plain]
            )
            .is_ok()
        );
    }

    /// Rule: the new fields are optional, so a v0.7.0 daemon and a v0.8.0 hub
    /// keep talking during a deploy.
    #[test]
    fn old_records_parse_without_the_new_fields() {
        let stats: HostStats = serde_json::from_value(serde_json::json!({
            "version": "0.7.0",
            "load1": 0.2,
            "cpus": 4,
            "mem_used_pct": 31.0,
            "outbox_depth": 0,
            "nats_reconnects": 0,
            "sessions": []
        }))
        .expect("a v0.7.0 heartbeat");
        assert_eq!(stats.mem_avail_mb, None);
        assert_eq!(stats.disk_avail_mb, None);
        assert_eq!(stats.disk_used_pct, None);

        let info: HostInfo = serde_json::from_value(serde_json::json!({
            "name": "h1",
            "harnesses": ["pi"],
            "launcher": "tmux",
            "max_agents": 4,
            "always_on": true,
            "draining": false,
            "ssh": null
        }))
        .expect("a v0.7.0 host");
        assert_eq!(info.reserve_mem_mb, None);
        assert!(info.sandboxes.is_empty());

        let role: RoleConfig =
            serde_json::from_value(serde_json::json!({ "spawns": [] })).expect("a v0.7.0 role");
        assert_eq!(role.sandbox, None);

        // A v0.7.0 queue record holds one reason string and no host.
        let mut record = SpawnRecord {
            agent: "w1".parse().unwrap(),
            spec: serde_json::from_value(serde_json::json!({ "channel": "api" }))
                .expect("a spawn spec"),
            resolved: Resolved::default(),
            by: Sender::Human,
            queued_at: Timestamp(1),
            priority: 0,
            state: SpawnState::Queued,
            attempts: 1,
            last_reject: vec![(name("h1"), "full (4/4)".to_owned())],
            queue_reason: None,
        };
        let mut json = serde_json::to_value(&record).expect("a queue record");
        json["last_reject"] = serde_json::json!("no host qualifies");
        let old: SpawnRecord = serde_json::from_value(json).expect("a v0.7.0 queue record");
        assert!(old.last_reject.is_empty(), "the one string has no host");
        // The list of this version round-trips.
        record.attempts = 2;
        let round: SpawnRecord =
            serde_json::from_value(serde_json::to_value(&record).expect("a record"))
                .expect("a queue record");
        assert_eq!(round.last_reject, record.last_reject);
    }
}
