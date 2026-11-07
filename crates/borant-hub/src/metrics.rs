//! The counters behind `/metrics` that only the hot paths can see: the
//! requests of `borant.ctl`, the tick passes, the store key counts that the
//! tick already holds, and the resident size of this process.
//!
//! The families that `/metrics` builds from the live view are gauges of a
//! scrape. These are the ones that a scrape cannot recover: a request that
//! arrived, a tick that ran, a listing that a tick read, and the memory of the
//! process. Each is a fixed set of atomics, so recording one costs one
//! fetch-and-add on the request or tick path and a scrape never takes a lock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use borant_core::keys;
use borant_core::proto::CtlRequest;

/// The kinds of `borant.ctl` request: one label value per `CtlRequest`
/// variant, in a fixed order.
pub(crate) const CTL_KINDS: [&str; 10] = [
    "register_host",
    "host_heartbeat",
    "register_session",
    "end_session",
    "call",
    "move_ready",
    "launch_failed",
    "maintenance",
    "snapshot",
    "member_limited",
];

/// The index of a request in [`CTL_KINDS`], for the counters of its kind.
pub(crate) fn ctl_kind(req: &CtlRequest) -> usize {
    match req {
        CtlRequest::RegisterHost { .. } => 0,
        CtlRequest::HostHeartbeat { .. } => 1,
        CtlRequest::RegisterSession(_) => 2,
        CtlRequest::EndSession { .. } => 3,
        CtlRequest::Call { .. } => 4,
        CtlRequest::MoveReady { .. } => 5,
        CtlRequest::LaunchFailed { .. } => 6,
        CtlRequest::Maintenance { .. } => 7,
        CtlRequest::Snapshot { .. } => 8,
        CtlRequest::MemberLimited { .. } => 9,
    }
}

/// The key prefixes that `borant_hub_store_keys` reports, each the prefix of
/// one listing that the tick already makes.
pub(crate) const STORE_PREFIXES: [&str; 4] = [
    keys::LEASE_AGENT_PREFIX,
    keys::AGENT_PREFIX,
    keys::HOST_PREFIX,
    keys::SPAWN_PREFIX,
];

/// The bounds of `borant_hub_ctl_latency_seconds`, in seconds: a ctl request
/// answers in milliseconds until a pass or a listing stands in its way.
const CTL_BOUNDS: [f64; 14] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// The bounds of `borant_hub_tick_seconds`, in seconds: a settled tick is
/// milliseconds, and a tick that reads a large store after a restart is
/// seconds.
const TICK_BOUNDS: [f64; 11] = [0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];

/// One histogram: the cumulative bucket counts, the sum, and the count.
///
/// The buckets are counts of observations in each range, and a reader makes
/// them cumulative.
#[derive(Debug)]
pub(crate) struct Histogram {
    bounds: &'static [f64],
    /// One count per bound, plus one for the values above the last bound.
    buckets: Box<[AtomicU64]>,
    /// The sum of the observations, in microseconds.
    micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            buckets: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
            micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Records one observation. A value above the last bound lands in the
    /// overflow bucket that `+Inf` reports.
    pub(crate) fn observe(&self, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        let bucket = self.bounds.partition_point(|bound| *bound < secs);
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.micros.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn bounds(&self) -> &'static [f64] {
        self.bounds
    }

    /// The cumulative count of each bound, the count, and the sum in
    /// seconds. The last cumulative value is the count of every observation,
    /// which is what `+Inf` reports.
    pub(crate) fn read(&self) -> (Vec<u64>, u64, f64) {
        let mut running = 0;
        let mut cumulative = Vec::with_capacity(self.bounds.len());
        for bucket in self.buckets.iter() {
            running += bucket.load(Ordering::Relaxed);
            cumulative.push(running);
        }
        let count = self.count.load(Ordering::Relaxed);
        let sum = self.micros.load(Ordering::Relaxed) as f64 / 1e6;
        (cumulative, count, sum)
    }
}

/// The counters of the hub, recorded by the request and tick paths and read
/// by `/metrics`.
#[derive(Debug)]
pub(crate) struct Metrics {
    ctl_requests: [AtomicU64; CTL_KINDS.len()],
    ctl_errors: [AtomicU64; CTL_KINDS.len()],
    /// One latency histogram per `CTL_KINDS` entry, in that order.
    ctl_latency: Vec<Histogram>,
    tick: Histogram,
    /// The key count of each `STORE_PREFIXES` entry, from the last tick that
    /// listed it.
    store_keys: [AtomicU64; STORE_PREFIXES.len()],
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub(crate) fn new() -> Self {
        Self {
            ctl_requests: std::array::from_fn(|_| AtomicU64::new(0)),
            ctl_errors: std::array::from_fn(|_| AtomicU64::new(0)),
            ctl_latency: CTL_KINDS
                .iter()
                .map(|_| Histogram::new(&CTL_BOUNDS))
                .collect(),
            tick: Histogram::new(&TICK_BOUNDS),
            store_keys: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Records one finished request of `kind`: its count, its error when it
    /// failed, and how long it took.
    pub(crate) fn ctl(&self, kind: usize, elapsed: Duration, failed: bool) {
        self.ctl_requests[kind].fetch_add(1, Ordering::Relaxed);
        if failed {
            self.ctl_errors[kind].fetch_add(1, Ordering::Relaxed);
        }
        self.ctl_latency[kind].observe(elapsed);
    }

    /// Records one finished tick pass that `elapsed` took.
    pub(crate) fn tick_done(&self, elapsed: Duration) {
        self.tick.observe(elapsed);
    }

    /// Records the key count of one listing that a tick just read. A prefix
    /// that `/metrics` does not report is ignored.
    pub(crate) fn keys(&self, prefix: &str, count: usize) {
        if let Some(i) = STORE_PREFIXES.iter().position(|p| *p == prefix) {
            self.store_keys[i].store(count as u64, Ordering::Relaxed);
        }
    }

    /// The requests of each kind, in `CTL_KINDS` order.
    pub(crate) fn ctl_requests(&self) -> Vec<u64> {
        self.ctl_requests
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    /// The failed requests of each kind, in `CTL_KINDS` order.
    pub(crate) fn ctl_errors(&self) -> Vec<u64> {
        self.ctl_errors
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    /// The latency histogram of one kind.
    pub(crate) fn ctl_latency(&self, kind: usize) -> &Histogram {
        &self.ctl_latency[kind]
    }

    /// The histogram of the tick passes.
    pub(crate) fn tick(&self) -> &Histogram {
        &self.tick
    }

    /// The key count of each reported prefix, in `STORE_PREFIXES` order.
    pub(crate) fn store_keys(&self) -> Vec<(&'static str, u64)> {
        STORE_PREFIXES
            .iter()
            .zip(self.store_keys.iter())
            .map(|(prefix, count)| (*prefix, count.load(Ordering::Relaxed)))
            .collect()
    }
}

/// The resident set of this process, in bytes, from `/proc/self/statm`.
/// `None` where the platform has no such file, so the series is absent
/// instead of a made-up zero.
pub(crate) fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // The Linux boxes that run the hub keep 4 KiB pages, as the memory
        // bound of the tick test assumes.
        Some(pages.saturating_mul(4096))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::proto::ToolCall;
    use borant_core::{FencingToken, SessionId};

    #[test]
    fn every_ctl_variant_is_labelled_with_its_wire_name() {
        // One request of each variant: a variant that an arm forgets fails the
        // match, and one with the wrong index names another kind here.
        let requests = [
            CtlRequest::RegisterHost {
                info: Default::default(),
            },
            CtlRequest::HostHeartbeat {
                host: "h1".parse().unwrap(),
                sessions: Vec::new(),
                stats: None,
                launching: Vec::new(),
            },
            CtlRequest::RegisterSession(Default::default()),
            CtlRequest::EndSession {
                session: SessionId::new(),
                agent: "w1".parse().unwrap(),
                token: FencingToken(1),
                reason: String::new(),
            },
            CtlRequest::Call {
                actor: None,
                key: None,
                call: ToolCall::Whoami,
            },
            CtlRequest::MoveReady {
                agent: "w1".parse().unwrap(),
                session: SessionId::new(),
                token: FencingToken(1),
            },
            CtlRequest::LaunchFailed {
                agent: "w1".parse().unwrap(),
                host: "h1".parse().unwrap(),
                error: String::new(),
                timeout_secs: None,
            },
            CtlRequest::Maintenance { on: true },
            CtlRequest::Snapshot { out: "out".into() },
            CtlRequest::MemberLimited {
                agent: "w1".parse().unwrap(),
                session: SessionId::new(),
                token: FencingToken(1),
                bundle: "main".parse().unwrap(),
                member: "claude".parse().unwrap(),
                resets_at: None,
            },
        ];
        assert_eq!(requests.len(), CTL_KINDS.len());
        for request in &requests {
            let wire = serde_json::to_value(request).expect("a request");
            let name = wire["op"].as_str().expect("a command name");
            assert_eq!(CTL_KINDS[ctl_kind(request)], name);
        }
    }

    #[test]
    fn a_histogram_counts_each_observation_once() {
        let h = Histogram::new(&[0.01, 0.1]);
        h.observe(Duration::from_millis(1));
        h.observe(Duration::from_millis(10));
        h.observe(Duration::from_millis(50));
        h.observe(Duration::from_millis(500));
        let (cumulative, count, sum) = h.read();
        // `le=0.01` holds the 1 ms and the 10 ms sample: a bound counts what
        // is at or below it.
        assert_eq!(cumulative, vec![2, 3, 4]);
        assert_eq!(count, 4);
        assert_eq!(sum, 0.561);
    }

    #[test]
    fn a_ctl_records_its_kind_its_error_and_its_time() {
        let metrics = Metrics::new();
        metrics.ctl(4, Duration::from_millis(7), false);
        metrics.ctl(4, Duration::from_millis(300), true);
        let requests = metrics.ctl_requests();
        assert_eq!(requests[4], 2);
        assert_eq!(requests.iter().sum::<u64>(), 2);
        assert_eq!(metrics.ctl_errors()[4], 1);
        let (_, count, sum) = metrics.ctl_latency(4).read();
        assert_eq!(count, 2);
        assert!((sum - 0.307).abs() < 1e-9, "{sum}");
        assert_eq!(metrics.ctl_latency(0).read().1, 0);
    }

    #[test]
    fn the_store_keys_report_the_prefixes_of_the_tick() {
        let metrics = Metrics::new();
        metrics.keys(keys::SPAWN_PREFIX, 3);
        metrics.keys("other.", 9);
        let counts: Vec<(_, _)> = metrics.store_keys();
        assert_eq!(
            counts,
            vec![
                (keys::LEASE_AGENT_PREFIX, 0),
                (keys::AGENT_PREFIX, 0),
                (keys::HOST_PREFIX, 0),
                (keys::SPAWN_PREFIX, 3),
            ]
        );
    }

    #[test]
    fn the_resident_size_is_a_plausible_number() {
        let bytes = resident_bytes();
        if cfg!(target_os = "linux") {
            let bytes = bytes.expect("a resident size on Linux");
            assert!(bytes > 1024 * 1024, "{bytes} bytes");
        } else {
            assert_eq!(bytes, None);
        }
    }
}
