//! Time as Unix milliseconds, and a clock that tests can control.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Unix time in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn plus(self, d: Duration) -> Self {
        Self(
            self.0
                .saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    pub fn minus(self, d: Duration) -> Self {
        Self(
            self.0
                .saturating_sub(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    /// The time from `earlier` to `self`, or zero if `earlier` is later.
    pub fn since(self, earlier: Timestamp) -> Duration {
        Duration::from_millis(u64::try_from(self.0 - earlier.0).unwrap_or(0))
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> Timestamp;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Timestamp(i64::try_from(ms).unwrap_or(i64::MAX))
    }
}

/// A clock that moves only when a test calls [`ManualClock::advance`].
#[derive(Debug)]
pub struct ManualClock(Mutex<Timestamp>);

impl ManualClock {
    pub fn new(start: Timestamp) -> Self {
        Self(Mutex::new(start))
    }

    pub fn advance(&self, d: Duration) {
        let mut t = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *t = t.plus(d);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}
