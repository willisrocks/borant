//! A host that misses the hub: a short blip must not pause the tools, a long
//! outage must, and the lease invariant holds across both (spec sections 11
//! and 16). Also the spread of the registrations after an outage.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use borant_core::model::Harness;
use borant_core::ports::MessageBus;
use borant_core::proto::{CtlRequest, LocalEvent, LocalRegister, LocalSession, LocalState, Reply};
use borant_core::{Clock, ManualClock, SessionId, Timestamp, subjects};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use tokio_util::sync::CancellationToken;

const BEAT: Duration = Duration::from_secs(10);
/// The reason the host gives for paused tools.
const PAUSED: &str = "The Borant hub is unreachable, so tools are paused until it returns.";

struct Fx {
    host: Host,
    hub: Arc<Hub>,
    bus: MemoryBus,
    clock: Arc<ManualClock>,
    dir: tempfile::TempDir,
    serve: Option<tokio::task::JoinHandle<()>>,
    stop: CancellationToken,
}

/// Serves hub requests on the bus until the token is cancelled.
async fn serve(bus: &MemoryBus, hub: Arc<Hub>, stop: CancellationToken) -> tokio::task::JoinHandle<()> {
    let mut reqs = bus.serve(subjects::CTL).await.unwrap();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = stop.cancelled() => return,
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
    })
}

impl Fx {
    async fn new(tweak: impl FnOnce(&mut HostConfig)) -> Self {
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let hub = Arc::new(Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(bus.clone()),
            Arc::new(sessions.clone()),
            clock.clone(),
            HubConfig::default(),
        ));
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
        cfg.harnesses = vec![Harness::Fake];
        cfg.hub_timeout = Duration::from_millis(100);
        cfg.heartbeat = BEAT;
        tweak(&mut cfg);
        let host = Host::new(
            cfg,
            Arc::new(bus.clone()),
            Arc::new(sessions),
            clock.clone(),
            Box::new(ProcessLauncher::new(dir.path().join("logs"))),
            None,
            None,
            Outbox::in_memory().unwrap(),
        );
        let stop = CancellationToken::new();
        let serve = Some(serve(&bus, hub.clone(), stop.clone()).await);
        Self {
            host,
            hub,
            bus,
            clock,
            dir,
            serve,
            stop,
        }
    }

    /// One session of `agent`, attached to the hub.
    async fn session(&self, agent: &str, sub: &str) -> LocalSession {
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

    /// Takes the hub away: the next request fails at once.
    async fn hub_down(&mut self) {
        self.stop.cancel();
        if let Some(running) = self.serve.take() {
            running.await.unwrap();
        }
    }

    /// Serves the hub again, with the state it had.
    async fn hub_back(&mut self) {
        self.stop = CancellationToken::new();
        self.serve = Some(serve(&self.bus, self.hub.clone(), self.stop.clone()).await);
    }

    /// `n` beats that fail, one heartbeat each.
    async fn failed_beats(&mut self, n: u32) {
        for _ in 0..n {
            self.clock.advance(BEAT);
            self.host.heartbeat_once().await;
        }
    }

    /// The reason the tools of the session are blocked, or `None`.
    fn fenced(&self, id: SessionId) -> Option<String> {
        self.host.local_view(id).unwrap().fenced
    }

    /// Sends one call to the served hub.
    async fn ctl(&self, req: CtlRequest) -> Result<serde_json::Value, borant_core::proto::ApiError> {
        let payload = serde_json::to_vec(&req).unwrap();
        let bytes = self
            .bus
            .request(subjects::CTL, payload.into(), Duration::from_secs(5))
            .await
            .unwrap();
        serde_json::from_slice::<Reply>(&bytes).unwrap().into_result()
    }
}

#[tokio::test]
async fn a_short_blip_does_not_fence_the_tools() {
    let mut fx = Fx::new(|_| {}).await;
    let s = fx.session("a", "wa").await;
    fx.host.heartbeat_once().await;
    assert!(fx.host.host_registered());

    // 45 s without the hub: past the 30 s lease, inside the two minute
    // window.
    fx.hub_down().await;
    fx.failed_beats(4).await;
    fx.clock.advance(Duration::from_secs(5));
    fx.host.heartbeat_once().await;
    assert_eq!(fx.fenced(s.session), None, "a 45 s blip fences nothing");
    let reply = fx
        .host
        .event(s.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(reply.fenced, None, "the hooks allow the tools");

    // The hub answers again: the session reclaims its lease and goes on.
    fx.hub_back().await;
    fx.clock.advance(BEAT);
    fx.host.heartbeat_once().await;
    assert_eq!(fx.fenced(s.session), None);
    assert_eq!(
        fx.host.local_view(s.session).unwrap().state,
        LocalState::Attached
    );
}

#[tokio::test]
async fn a_long_outage_fences_the_tools_before_a_takeover() {
    let mut fx = Fx::new(|_| {}).await;
    let s = fx.session("a", "wa").await;
    fx.host.heartbeat_once().await;
    let cfg = fx.host.config().clone();
    // The fence happens before the lease and the grace run out at the hub.
    assert!(cfg.fence_after() <= cfg.lease + cfg.lease_grace);

    fx.hub_down().await;
    // 100 s in: inside the window, the tools still run.
    fx.failed_beats(10).await;
    assert_eq!(fx.fenced(s.session), None);
    // 130 s in: past the window, the tools pause.
    fx.failed_beats(3).await;
    assert_eq!(fx.fenced(s.session).as_deref(), Some(PAUSED));
    assert!(
        fx.host
            .event(s.session, LocalEvent::TurnStart)
            .await
            .unwrap()
            .fenced
            .as_deref()
            .is_some_and(|why| why == PAUSED)
    );

    // The lease ended at the hub, and the hub may let another session take the
    // agent over. Such a session runs, so the old one must not: its next beat
    // learns that it lost the lease.
    let before = fx.clock.now();
    fx.clock.advance(cfg.lease + cfg.lease_grace);
    assert!(fx.clock.now().since(before) >= cfg.lease + cfg.lease_grace);
    fx.hub.tick().await.unwrap();
    fx.hub_back().await;
    let taken = fx
        .ctl(CtlRequest::RegisterSession(borant_core::proto::RegisterSession {
            session: SessionId::new(),
            host: "h2".parse().unwrap(),
            harness: Harness::Fake,
            harness_session_id: None,
            pid: None,
            cwd: "/tmp/taken".into(),
            repo: None,
            agent: Some("a".parse().unwrap()),
            channel: None,
            shared_dir: false,
            role: None,
            collection: None,
        }))
        .await
        .expect("the other host takes the expired lease");
    assert_eq!(taken["agent"], "a");

    fx.clock.advance(BEAT);
    fx.host.heartbeat_once().await;
    let why = fx.fenced(s.session).expect("the old session is fenced");
    assert!(why.contains("lost its lease"), "{why}");
    // The hub is back, so the pause would lift; the fence does not.
    fx.clock.advance(BEAT);
    fx.host.heartbeat_once().await;
    assert_eq!(fx.fenced(s.session).as_deref(), Some(why.as_str()));
}

#[tokio::test]
async fn a_short_blip_keeps_the_lease_at_the_hub() {
    // The other side of the invariant: a blip inside the grace leaves the
    // lease to this session, and the hub gives it back instead of a takeover.
    let mut fx = Fx::new(|_| {}).await;
    let s = fx.session("a", "wa").await;
    fx.host.heartbeat_once().await;
    fx.hub_down().await;
    fx.failed_beats(4).await;
    fx.hub_back().await;
    fx.clock.advance(BEAT);
    fx.host.heartbeat_once().await;
    assert_eq!(fx.fenced(s.session), None);
    assert_eq!(
        fx.host.local_view(s.session).unwrap().state,
        LocalState::Attached
    );
}

#[tokio::test]
async fn registrations_after_an_outage_spread_over_a_window() {
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let hub = Arc::new(Hub::new(
        Arc::new(MemoryState::new()),
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
        clock.clone(),
        HubConfig::default(),
    ));
    let stop = CancellationToken::new();
    let running = serve(&bus, hub.clone(), stop.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut hosts = Vec::new();
    for i in 0..16 {
        let mut cfg = HostConfig::new(
            format!("h{i}").parse().unwrap(),
            dir.path().join(format!("h{i}")),
        );
        cfg.harnesses = vec![Harness::Fake];
        cfg.hub_timeout = Duration::from_millis(100);
        cfg.heartbeat = BEAT;
        let host = Host::new(
            cfg,
            Arc::new(bus.clone()),
            Arc::new(sessions.clone()),
            clock.clone(),
            Box::new(ProcessLauncher::new(dir.path().join("logs"))),
            None,
            None,
            Outbox::in_memory().unwrap(),
        );
        host.heartbeat_once().await;
        assert!(host.host_registered(), "h{i} starts registered");
        hosts.push(host);
    }

    // Three failed beats each: one outage, one fleet.
    stop.cancel();
    running.await.unwrap();
    for _ in 0..3 {
        clock.advance(BEAT);
        for host in &hosts {
            host.heartbeat_once().await;
        }
    }
    let now = clock.now();
    let mut due: Vec<i64> = Vec::new();
    for host in &hosts {
        let at = host.register_due_at().expect("a gated registration");
        assert!(at > now, "{} registers again at once", host.config().name);
        // Three failures back off over the third window, 40 s.
        assert!(at.since(now) <= Duration::from_secs(40), "{at}");
        due.push(at.0);
    }
    due.sort_unstable();
    let span = due.last().unwrap() - due.first().unwrap();
    assert!(span > 15_000, "the fleet spreads over {span} ms");

    // The hub is back: each host registers when its own gate opens, so the
    // fleet does not land in one burst.
    let stop = CancellationToken::new();
    let running = serve(&bus, hub.clone(), stop.clone()).await;
    let mut registered: Vec<bool> = hosts.iter().map(|h| h.host_registered()).collect();
    let mut per_second = Vec::new();
    for _second in 0..=41 {
        clock.advance(Duration::from_secs(1));
        for host in &hosts {
            host.heartbeat_once().await;
        }
        let mut n = 0;
        for (i, host) in hosts.iter().enumerate() {
            let up = host.host_registered();
            if up && !registered[i] {
                registered[i] = true;
                n += 1;
            }
        }
        per_second.push(n);
    }
    stop.cancel();
    running.await.unwrap();
    assert!(
        registered.iter().all(|up| *up),
        "every host registers inside the window: {per_second:?}"
    );
    let busy = per_second.iter().filter(|n| **n > 0).count();
    assert!(busy >= 8, "the fleet registers in {busy} seconds: {per_second:?}");
}
