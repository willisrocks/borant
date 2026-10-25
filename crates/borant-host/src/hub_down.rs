//! The `hub.unreachable` alert of a host that watches the hub (phase 2 spec,
//! "Alerts"): one urgent notice after `hub_down_minutes` of failed heartbeats,
//! and one when the hub answers again.
//!
//! The window runs on the wall clock, which counts the sleep of a laptop, so a
//! machine that just woke must not alert at once. The beat loop does not run
//! while the machine sleeps, so a gap between two beats that is far longer
//! than the heartbeat is a sleep: the window starts again at the wake.

use std::time::Duration;

use borant_core::Timestamp;

/// The alert state of one host.
#[derive(Debug)]
pub struct HubDownWatch {
    /// How long the heartbeats must fail before the notice goes out.
    window: Duration,
    /// A gap between two beats at least this long is a sleep, not an outage.
    sleep_gap: Duration,
    /// When the current run of failed heartbeats began, on the wall clock.
    since: Option<Timestamp>,
    /// The time of the previous beat, successful or not.
    last_beat: Option<Timestamp>,
    /// True while the notice of this run stands: one notice for each run.
    sent: bool,
}

/// What a beat of the heartbeat loop asks the host to tell the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAlert {
    /// The hub has been unreachable for `minutes`: send the notice of this run.
    Down { minutes: u64 },
    /// The hub answers again after a notice.
    Back,
}

impl HubDownWatch {
    /// `heartbeat` is the interval of the beat loop, which sets the gap that
    /// counts as a sleep.
    pub fn new(window: Duration, heartbeat: Duration) -> Self {
        Self {
            window,
            sleep_gap: heartbeat.saturating_mul(3),
            since: None,
            last_beat: None,
            sent: false,
        }
    }

    /// One heartbeat that did not get through at `now`.
    pub fn failed(&mut self, now: Timestamp) -> Option<HubAlert> {
        let slept = self
            .last_beat
            .is_some_and(|last| now.since(last) >= self.sleep_gap);
        self.last_beat = Some(now);
        if slept {
            // The beat loop did not run, so the machine slept. A fresh window
            // keeps a laptop that just woke from alerting at once. A notice
            // that already stands is not sent again.
            self.since = Some(now);
            return None;
        }
        let since = *self.since.get_or_insert(now);
        if self.sent || now.since(since) < self.window {
            return None;
        }
        self.sent = true;
        let minutes = self.window.as_secs().div_ceil(60).max(1);
        Some(HubAlert::Down { minutes })
    }

    /// One heartbeat that got through at `now`.
    pub fn recovered(&mut self, now: Timestamp) -> Option<HubAlert> {
        self.last_beat = Some(now);
        self.since = None;
        std::mem::replace(&mut self.sent, false).then_some(HubAlert::Back)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEAT: Duration = Duration::from_secs(10);
    const WINDOW: Duration = Duration::from_secs(120);

    fn watch() -> HubDownWatch {
        HubDownWatch::new(WINDOW, BEAT)
    }

    /// Rule: one notice after the window, and no second one for that run.
    #[test]
    fn hub_down_fires_once_after_the_window() {
        let mut watch = watch();
        let start = Timestamp(1_000_000);
        let mut now = start;
        let mut fired = Vec::new();
        for _ in 0..20 {
            now = now.plus(BEAT);
            if let Some(alert) = watch.failed(now) {
                fired.push((now.since(start), alert));
            }
        }
        assert_eq!(fired.len(), 1, "one notice for one run: {fired:?}");
        assert_eq!(fired[0].1, HubAlert::Down { minutes: 2 });
        assert!(
            fired[0].0 >= WINDOW,
            "the notice waits for the window: {:?}",
            fired[0].0
        );
    }

    /// Rule: an outage shorter than the window tells nobody.
    #[test]
    fn hub_down_ignores_a_short_outage() {
        let mut watch = watch();
        let start = Timestamp(1_000_000);
        let mut now = start;
        let mut fired = Vec::new();
        for _ in 0..6 {
            now = now.plus(BEAT);
            fired.extend(watch.failed(now));
        }
        assert!(fired.is_empty(), "{fired:?}");
        assert_eq!(watch.recovered(now.plus(BEAT)), None);
        // The next outage starts its own window.
        now = now.plus(BEAT);
        fired.extend(watch.failed(now));
        assert!(fired.is_empty(), "a fresh window after a recovery");
    }

    /// Rule: a machine that just woke does not alert at once.
    #[test]
    fn hub_down_restarts_the_window_after_a_sleep() {
        let mut watch = watch();
        let start = Timestamp(1_000_000);
        watch.failed(start);
        watch.failed(start.plus(BEAT));
        // The laptop sleeps for half an hour; the first beat after the wake
        // fails at once.
        let wake = start.plus(Duration::from_secs(1800));
        assert_eq!(watch.failed(wake), None, "no notice right after a sleep");
        // Thirty seconds after the wake, the window is still young.
        let mut now = wake;
        for _ in 0..3 {
            now = now.plus(BEAT);
            assert_eq!(watch.failed(now), None, "the window restarted at the wake");
        }
        // Two minutes after the wake it fires, once.
        let mut fired = Vec::new();
        for _ in 0..20 {
            now = now.plus(BEAT);
            fired.extend(watch.failed(now));
        }
        assert_eq!(fired, vec![HubAlert::Down { minutes: 2 }]);
        assert!(
            now.since(wake) >= WINDOW,
            "measured from the wake: {:?}",
            now.since(wake)
        );
    }

    /// Rule: one notice on the way back, and only after a notice of the outage.
    #[test]
    fn hub_down_resolves_once() {
        let mut watch = watch();
        let start = Timestamp(1_000_000);
        let mut now = start;
        let mut fired = Vec::new();
        for _ in 0..20 {
            now = now.plus(BEAT);
            fired.extend(watch.failed(now));
        }
        assert_eq!(fired.len(), 1);
        now = now.plus(BEAT);
        assert_eq!(watch.recovered(now), Some(HubAlert::Back));
        now = now.plus(BEAT);
        assert_eq!(watch.recovered(now), None, "one notice for each recovery");
        // A recovery without a notice says nothing.
        now = now.plus(BEAT);
        watch.failed(now);
        assert_eq!(watch.recovered(now.plus(BEAT)), None);
    }
}
