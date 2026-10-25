//! The launch watchdog: a launch that never registers is stopped and
//! reported, a registered one is left alone, and a relaunch kills the stale
//! session of the agent first.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, MessageBus, PortError, PortResult};
use borant_core::proto::{CtlRequest, LocalRegister, Reply};
use borant_core::{AgentId, SystemClock, subjects};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_memory::{MemoryBus, MemorySession};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// A launcher with a screen, for a host without tmux: it refuses a second
/// session of one agent, as tmux does, and remembers what the tests ask.
#[derive(Debug, Clone, Default)]
struct FakeLauncher {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Debug, Default)]
struct FakeInner {
    /// The session of each agent that still runs.
    running: BTreeMap<String, LaunchHandle>,
    screen: Vec<String>,
    calls: Vec<String>,
}

impl FakeLauncher {
    fn set_screen(&self, lines: &[&str]) {
        let mut inner = self.inner.lock().unwrap();
        inner.screen = lines.iter().map(|l| (*l).to_owned()).collect();
    }

    fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }
}

#[async_trait::async_trait]
impl Launcher for FakeLauncher {
    fn kind(&self) -> &'static str {
        "fake"
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let mut inner = self.inner.lock().unwrap();
        let key = spec.agent.to_string();
        if inner.running.contains_key(&key) {
            return Err(PortError::conflict(format!(
                "a fake session of {key} already exists"
            )));
        }
        let handle = LaunchHandle {
            kind: "fake".to_owned(),
            id: format!("fake-{key}"),
        };
        inner.calls.push(format!("launch {}", handle.id));
        inner.running.insert(key, handle.clone());
        Ok(handle)
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(format!("stop {}", handle.id));
        inner.running.retain(|_, h| h.id != handle.id);
        Ok(())
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.running.values().any(|h| h.id == handle.id))
    }

    fn attach_command(&self, _handle: &LaunchHandle) -> Option<Vec<String>> {
        None
    }

    fn capture(&self, _handle: &LaunchHandle, lines: usize) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        if inner.screen.is_empty() {
            return None;
        }
        let from = inner.screen.len().saturating_sub(lines);
        Some(inner.screen[from..].join("\n"))
    }
}

/// A hub that answers registrations and records every request.
async fn serve_hub(bus: &MemoryBus) -> (Arc<Mutex<Vec<CtlRequest>>>, CancellationToken) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = CancellationToken::new();
    let mut reqs = bus.serve(subjects::CTL).await.expect("serve the hub");
    let recorded = seen.clone();
    let token = stop.clone();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = token.cancelled() => return,
                n = reqs.next() => n,
            };
            let Some(req) = next else { return };
            let parsed = serde_json::from_slice::<CtlRequest>(&req.payload);
            let reply = match &parsed {
                // The registration echoes the session that the host chose, so
                // the launch counts as registered.
                Ok(CtlRequest::RegisterSession(r)) => Reply::ok(&json!({
                    "agent": r.agent.clone().unwrap_or_else(|| "a1".parse().unwrap()),
                    "session": r.session,
                    "token": 1,
                    "movable": false,
                    "channels": [],
                    "pinned": false,
                })),
                _ => Reply::ok(&json!(null)),
            };
            if let Ok(req) = parsed {
                recorded.lock().unwrap().push(req);
            }
            let _ = req
                .responder
                .respond(serde_json::to_vec(&reply).unwrap().into())
                .await;
        }
    });
    (seen, stop)
}

struct Fixture {
    host: Host,
    seen: Arc<Mutex<Vec<CtlRequest>>>,
    launcher: FakeLauncher,
    dir: tempfile::TempDir,
    stop: CancellationToken,
}

/// A host with the fake launcher, a hub on the bus, and a short watchdog.
async fn fixture(timeout: Duration) -> Fixture {
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.hub_timeout = Duration::from_millis(500);
    cfg.launch_timeout = timeout;
    let launcher = FakeLauncher::default();
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
        Arc::new(SystemClock),
        Box::new(launcher.clone()),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );
    let (seen, stop) = serve_hub(&bus).await;
    Fixture {
        host,
        seen,
        launcher,
        dir,
        stop,
    }
}

fn order(agent: &AgentId) -> borant_core::proto::LaunchOrder {
    borant_core::proto::LaunchOrder {
        agent: agent.clone(),
        harness: Harness::Fake,
        ..Default::default()
    }
}

fn launch_failures(seen: &Arc<Mutex<Vec<CtlRequest>>>) -> Vec<(AgentId, String)> {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|r| match r {
            CtlRequest::LaunchFailed { agent, error, .. } => Some((agent.clone(), error.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn watchdog_stops_a_launch_that_never_registers() {
    let f = fixture(Duration::from_millis(50)).await;
    f.launcher
        .set_screen(&["Do you trust this folder?", "token=abc123"]);
    let agent: AgentId = "a1".parse().unwrap();
    f.host.launch(order(&agent)).await.unwrap();
    assert!(
        launch_failures(&f.seen).is_empty(),
        "no failure before the timeout"
    );

    tokio::time::sleep(Duration::from_millis(80)).await;
    f.host.check_liveness().await;

    assert_eq!(
        f.launcher.calls(),
        vec!["launch fake-a1".to_owned(), "stop fake-a1".to_owned()],
        "the watchdog stops the launch"
    );
    let failures = launch_failures(&f.seen);
    assert_eq!(failures.len(), 1, "{failures:?}");
    let (failed_agent, screen) = &failures[0];
    assert_eq!(failed_agent, &agent);
    assert!(screen.contains("Do you trust this folder?"), "{screen}");
    assert!(
        !screen.contains("abc123"),
        "the screen is redacted: {screen}"
    );

    f.stop.cancel();
    drop(f.dir);
}

#[tokio::test]
async fn watchdog_leaves_a_registered_launch_alone() {
    let f = fixture(Duration::from_millis(50)).await;
    f.launcher.set_screen(&["working"]);
    let agent: AgentId = "a1".parse().unwrap();
    f.host.launch(order(&agent)).await.unwrap();
    let cwd = f.dir.path().join("work/a1");
    let registered = f
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent.clone()),
            ..Default::default()
        })
        .await
        .expect("the registration");

    tokio::time::sleep(Duration::from_millis(80)).await;
    f.host.check_liveness().await;

    assert_eq!(
        f.launcher.calls(),
        vec!["launch fake-a1".to_owned()],
        "a registered launch is not stopped"
    );
    assert!(
        launch_failures(&f.seen).is_empty(),
        "{:?}",
        launch_failures(&f.seen)
    );
    assert_eq!(registered.agent, Some(agent));

    f.stop.cancel();
    drop(f.dir);
}

#[tokio::test]
async fn watchdog_kills_a_stale_session_before_a_relaunch() {
    let f = fixture(Duration::from_secs(60)).await;
    let agent: AgentId = "a1".parse().unwrap();
    f.host.launch(order(&agent)).await.unwrap();

    // The session ended, but its session still runs on the host: a relaunch
    // must kill it, or the launcher refuses the name.
    f.host.launch(order(&agent)).await.expect("the relaunch");

    assert_eq!(
        f.launcher.calls(),
        vec![
            "launch fake-a1".to_owned(),
            "stop fake-a1".to_owned(),
            "launch fake-a1".to_owned(),
        ],
        "the stale session goes before the new one"
    );
    assert_eq!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| matches!(r, CtlRequest::LaunchFailed { .. }))
            .count(),
        0,
        "the relaunch is no failure"
    );

    f.stop.cancel();
    drop(f.dir);
}
