//! The end of a session must reach the hub even when the hub is down: the
//! host keeps it in the outbox and sends it again after the hub returns.

use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{Harness, Session};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, LocalRegister, LocalState, Reply};
use borant_core::{ManualClock, SystemClock, Timestamp, keys, subjects};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// Serves hub requests on the bus until the token is cancelled.
async fn serve_hub(bus: &MemoryBus, hub: Arc<Hub>) -> CancellationToken {
    let stop = CancellationToken::new();
    let mut reqs = bus.serve(subjects::CTL).await.unwrap();
    let s = stop.clone();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = s.cancelled() => return,
                n = reqs.next() => n,
            };
            let Some(req) = next else { return };
            let reply = match serde_json::from_slice::<CtlRequest>(&req.payload) {
                Ok(r) => hub.handle(r).await,
                Err(e) => Reply::Error(borant_core::proto::ApiError::invalid(e.to_string())),
            };
            let _ = req
                .responder
                .respond(serde_json::to_vec(&reply).unwrap().into())
                .await;
        }
    });
    stop
}

/// An end that the hub misses waits in the outbox, and the hub learns of it
/// when it answers again.
#[tokio::test]
async fn an_end_the_hub_misses_is_sent_again() {
    let bus = MemoryBus::new();
    let state = MemoryState::new();
    let sessions = MemorySession::new();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let hub = Arc::new(Hub::new(
        Arc::new(state.clone()),
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
        clock,
        HubConfig::default(),
    ));
    let stop = serve_hub(&bus, hub).await;

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.hub_timeout = Duration::from_millis(500);
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(sessions),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some("w1".parse().unwrap()),
            ..Default::default()
        })
        .await
        .expect("the registration");
    assert_eq!(s.state, LocalState::Attached);

    // The hub goes down, then the session ends.
    bus.set_available(false);
    host.end_session(s.session, "the harness process exited")
        .await;
    bus.set_available(true);
    host.flush_outbox().await;

    let doc = state
        .get(&keys::session(&s.session))
        .await
        .unwrap()
        .expect("the hub knows the session");
    let saved: Session = serde_json::from_slice(&doc.value).unwrap();
    assert!(
        saved.ended.is_some(),
        "the retried end reached the hub: {saved:?}"
    );

    stop.cancel();
}
