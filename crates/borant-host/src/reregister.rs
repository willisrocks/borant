//! The gate on the host registration after a failed heartbeat (spec section
//! 16). Every failed beat clears `host_registered`, so without a gate each
//! host re-registers on its next beat and a fleet that lost the hub comes
//! back as a burst: the hub OOM investigation saw 202 registrations in the
//! minute after a recovery.
//!
//! The gate gives each failed attempt a jittered deadline. The window grows
//! with the run of failures, so a long outage retries less often, and the
//! offset spreads the hosts of a fleet over the window, so their
//! registrations do not land together.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use borant_core::{HostName, Timestamp};

/// The longest window of the backoff.
const CAP: Duration = Duration::from_secs(60);
/// The doublings between the first window, one beat, and the cap.
const DOUBLINGS: u32 = 6;

/// When the host may register again after a failed attempt.
#[derive(Debug)]
pub(crate) struct RegisterGate {
    /// The beat interval: the first window.
    beat: Duration,
    /// The cap of the window, at least one beat.
    cap: Duration,
    /// The failed attempts since the last registration.
    failures: u32,
    /// When the next attempt may go, on the host clock.
    due: Option<Timestamp>,
}

impl RegisterGate {
    pub(crate) fn new(beat: Duration) -> Self {
        Self {
            beat,
            cap: CAP.max(beat),
            failures: 0,
            due: None,
        }
    }

    /// True when a registration may go at `now`.
    pub(crate) fn due(&self, now: Timestamp) -> bool {
        self.due.is_none_or(|at| now >= at)
    }

    /// One failed attempt at `now`: the next one waits a jittered backoff.
    pub(crate) fn failed(&mut self, host: &HostName, now: Timestamp) {
        self.failures = self.failures.saturating_add(1);
        let window = self.window();
        self.due = Some(now.plus(jitter(host, self.failures, window)));
    }

    /// One registration that got through: the next one may go at once.
    pub(crate) fn registered(&mut self) {
        self.failures = 0;
        self.due = None;
    }

    /// When the next attempt may go. Public for tests.
    pub fn deadline(&self) -> Option<Timestamp> {
        self.due
    }

    /// The window of the current run of failures: one beat, then double per
    /// failure, up to the cap.
    fn window(&self) -> Duration {
        let doublings = self.failures.saturating_sub(1).min(DOUBLINGS);
        self.beat.saturating_mul(1u32 << doublings).min(self.cap)
    }
}

/// A delay in `0..=window`, fixed by the host name and the failure count: the
/// hosts of a fleet spread over the window, and a retry of the same host
/// moves to another point of it.
fn jitter(host: &HostName, failures: u32, window: Duration) -> Duration {
    let mut h = DefaultHasher::new();
    host.hash(&mut h);
    failures.hash(&mut h);
    let ms = u64::try_from(window.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(h.finish() % ms.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Duration = Duration::from_secs(10);
    const START: Timestamp = Timestamp(1_000_000);

    fn host(name: &str) -> HostName {
        name.parse().unwrap()
    }

    /// The wait of `n` failures of `name`, from the last one.
    fn waits(name: &str, n: u32) -> Vec<u64> {
        let mut gate = RegisterGate::new(BEAT);
        let mut now = START;
        let mut out = Vec::new();
        for _ in 0..n {
            gate.failed(&host(name), now);
            let at = gate.deadline().unwrap();
            out.push(u64::try_from(at.since(now).as_millis()).unwrap());
            // The attempt went at its deadline and failed again.
            now = at;
        }
        out
    }

    #[test]
    fn a_host_with_no_failure_registers_at_once() {
        let gate = RegisterGate::new(BEAT);
        assert!(gate.due(START));
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn a_failure_gates_the_next_attempt_until_its_deadline() {
        let mut gate = RegisterGate::new(BEAT);
        gate.failed(&host("h1"), START);
        let at = gate.deadline().unwrap();
        assert!(!gate.due(START));
        assert!(!gate.due(at.minus(Duration::from_millis(1))));
        assert!(gate.due(at));
        gate.registered();
        assert!(gate.due(START));
        assert_eq!(gate.deadline(), None);
    }

    #[test]
    fn the_window_grows_to_the_cap() {
        for name in ["h1", "h2", "h3"] {
            let waits = waits(name, 8);
            assert!(waits[0] <= 10_000, "{waits:?}");
            assert!(waits[1] <= 20_000, "{waits:?}");
            assert!(waits[2] <= 40_000, "{waits:?}");
            for wait in &waits[3..] {
                assert!(*wait <= 60_000, "{waits:?}");
            }
            // The cap holds at the last attempts, not the doubling past it.
            assert!(waits[7] <= 60_000, "{waits:?}");
        }
    }

    #[test]
    fn a_fleet_spreads_over_the_cap() {
        let mut waits = Vec::new();
        for i in 0..64 {
            let name = format!("host-{i}");
            waits.push(waits_of(&name));
        }
        let min = *waits.iter().min().unwrap();
        let max = *waits.iter().max().unwrap();
        let distinct: std::collections::HashSet<_> = waits.iter().collect();
        assert!(max <= 60_000, "{max}");
        // A fleet that failed six times spreads over most of the window, so
        // the registrations of a recovery do not land in one second.
        assert!(max - min > 30_000, "{min}..{max}");
        assert!(distinct.len() > 50, "{} distinct of {}", distinct.len(), waits.len());
    }

    /// The wait of the sixth failure of `name`: a long outage.
    fn waits_of(name: &str) -> u64 {
        waits(name, 6).pop().unwrap()
    }
}
