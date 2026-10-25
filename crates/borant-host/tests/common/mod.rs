#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::MessageBus;
use borant_core::proto::{CtlRequest, LocalRegister, LocalSession, LocalState, Reply};
use borant_core::{ManualClock, SystemClock, Timestamp, subjects};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

pub struct Fx {
    pub host: Host,
    pub hub: Arc<Hub>,
    pub bus: MemoryBus,
    pub sessions: MemorySession,
    pub hub_clock: Arc<ManualClock>,
    pub dir: tempfile::TempDir,
    pub hub_stop: Option<CancellationToken>,
}

/// Serves hub requests on the bus until the token is cancelled.
pub async fn serve_hub(bus: &MemoryBus, hub: Arc<Hub>) -> CancellationToken {
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

impl Fx {
    pub async fn new(with_hub: bool, tweak: impl FnOnce(&mut HostConfig)) -> Self {
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let hub_clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let hub = Arc::new(Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(bus.clone()),
            Arc::new(sessions.clone()),
            hub_clock.clone(),
            HubConfig::default(),
        ));
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
        cfg.harnesses = vec![Harness::Fake];
        cfg.hub_timeout = Duration::from_millis(500);
        tweak(&mut cfg);
        let host = Host::new(
            cfg,
            Arc::new(bus.clone()),
            Arc::new(sessions.clone()),
            Arc::new(SystemClock),
            Box::new(ProcessLauncher::new(dir.path().join("logs"))),
            None,
            Outbox::in_memory().unwrap(),
        );
        let hub_stop = if with_hub {
            Some(serve_hub(&bus, hub.clone()).await)
        } else {
            None
        };
        Self {
            host,
            hub,
            bus,
            sessions,
            hub_clock,
            dir,
            hub_stop,
        }
    }

    pub async fn session(&self, agent: &str, sub: &str) -> LocalSession {
        let cwd = self.dir.path().join(sub);
        std::fs::create_dir_all(&cwd).unwrap();
        let s = self
            .host
            .register(LocalRegister {
                harness: Harness::Fake,
                cwd: cwd.to_string_lossy().into_owned(),
                agent: Some(agent.parse().unwrap()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(s.state, LocalState::Attached, "{s:?}");
        s
    }
}

pub async fn eventually<F, Fut>(mut f: F, what: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}
