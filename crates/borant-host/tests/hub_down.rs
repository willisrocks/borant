//! The `hub.unreachable` notice of a host with `[alerts] watch_hub`: one
//! urgent notice after the window of failed heartbeats, and one when the hub
//! answers again.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::{Notice, Notifier, PortResult, Priority};
use borant_core::{ManualClock, Timestamp};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};

/// A notifier that keeps what it was told.
#[derive(Debug, Clone, Default)]
struct Recording(Arc<Mutex<Vec<Notice>>>);

impl Recording {
    fn notices(&self) -> Vec<Notice> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl Notifier for Recording {
    async fn notify(&self, notice: &Notice) -> PortResult<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notice.clone());
        Ok(())
    }
}

fn host(dir: &Path, clock: Arc<ManualClock>, bus: &MemoryBus, rec: &Recording) -> Host {
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.heartbeat = Duration::from_secs(10);
    cfg.hub_timeout = Duration::from_millis(100);
    // The laptop of the phase 2 spec.
    cfg.watch_hub = true;
    cfg.notify = true;
    cfg.hub_down = Duration::from_secs(120);
    Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(MemorySession::new()),
        clock,
        Box::new(ProcessLauncher::new(dir.join("logs"))),
        Some(Box::new(rec.clone())),
        None,
        Outbox::in_memory().expect("an in-memory outbox"),
    )
}

#[tokio::test]
async fn hub_down_notice_waits_for_the_window_and_resolves_once() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let bus = MemoryBus::new();
    let rec = Recording::default();
    let host = host(dir.path(), clock.clone(), &bus, &rec);

    // A minute of failed heartbeats tells nobody.
    for _ in 0..6 {
        host.heartbeat_once().await;
        clock.advance(Duration::from_secs(10));
    }
    assert!(rec.notices().is_empty(), "{:?}", rec.notices());

    // Past two minutes: one urgent notice, and no second one.
    for _ in 0..8 {
        host.heartbeat_once().await;
        clock.advance(Duration::from_secs(10));
    }
    let notices = rec.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].title, "Borant hub unreachable for 2 minutes");
    assert_eq!(notices[0].priority, Priority::Urgent);
    assert!(notices[0].tags.contains(&"hub.unreachable".to_owned()));

    // The hub answers again: one notice of the recovery.
    let sessions = Arc::new(MemorySession::new());
    let hub = Arc::new(Hub::new(
        Arc::new(MemoryState::new()),
        Arc::new(bus.clone()),
        sessions,
        clock.clone(),
        HubConfig::default(),
    ));
    let stop = common::serve_hub(&bus, hub).await;
    host.heartbeat_once().await;
    clock.advance(Duration::from_secs(10));
    host.heartbeat_once().await;
    let notices = rec.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[1].title, "Borant hub is back");
    stop.cancel();
}

#[tokio::test]
async fn hub_down_stays_quiet_without_watch_hub() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let bus = MemoryBus::new();
    let rec = Recording::default();
    let mut cfg = HostConfig::new("h2".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.hub_timeout = Duration::from_millis(100);
    cfg.watch_hub = false;
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(MemorySession::new()),
        clock.clone(),
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        Some(Box::new(rec.clone())),
        None,
        Outbox::in_memory().expect("an in-memory outbox"),
    );
    for _ in 0..20 {
        host.heartbeat_once().await;
        clock.advance(Duration::from_secs(10));
    }
    assert!(rec.notices().is_empty(), "{:?}", rec.notices());
}
