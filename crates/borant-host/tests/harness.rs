//! A host offers only the harnesses whose program it finds.
//!
//! The borant-3 incident of 2026-10-25: the daemon announced `claude-code` and
//! `omp` on a box that had neither, so the hub placed workers there and every
//! launch of theirs failed. The check runs at every heartbeat, so a program
//! that appears (or goes away) while the daemon runs needs no restart, and a
//! launch of a harness whose program vanished fails at once with the program
//! named.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use borant_core::SystemClock;
use borant_core::model::Harness;
use borant_core::ports::MessageBus;
use borant_core::proto::{CtlRequest, LaunchOrder, Reply};
use borant_core::subjects;
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// A host with the fake harness on `PATH` (`true`), and whatever the test
/// declares on top.
fn host(dir: &Path, tweak: impl FnOnce(&mut HostConfig)) -> Host {
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.join(".borant"));
    cfg.harnesses = vec![Harness::Fake];
    cfg.commands.fake = Some(vec!["true".to_owned()]);
    cfg.hub_timeout = Duration::from_millis(500);
    tweak(&mut cfg);
    Host::new(
        cfg,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(dir.join("logs"))),
        None,
        None,
        Outbox::in_memory().unwrap(),
    )
}

/// A program that `host` announces for the omp harness, at an absolute path.
fn omp_program(dir: &Path) -> PathBuf {
    dir.join("bin").join("omp")
}

/// A hub that answers every request and records the registrations.
async fn record_hub(bus: &MemoryBus) -> (Arc<Mutex<Vec<Vec<Harness>>>>, CancellationToken) {
    let registered = Arc::new(Mutex::new(Vec::new()));
    let stop = CancellationToken::new();
    let mut reqs = bus.serve(subjects::CTL).await.expect("serve the hub");
    let seen = registered.clone();
    let token = stop.clone();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = token.cancelled() => return,
                n = reqs.next() => n,
            };
            let Some(req) = next else { return };
            if let Ok(CtlRequest::RegisterHost { info }) =
                serde_json::from_slice::<CtlRequest>(&req.payload)
            {
                seen.lock().unwrap().push(info.harnesses);
            }
            let reply = Reply::ok(&serde_json::json!(null));
            let _ = req
                .responder
                .respond(serde_json::to_vec(&reply).unwrap().into())
                .await;
        }
    });
    (registered, stop)
}

thread_local! {
    /// The log lines that the test on this thread caused. The subscriber of
    /// this test binary is global, so the writer sorts the lines by thread.
    static LINES: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Mounts the one subscriber of this test binary and empties the lines of this
/// thread, for the test that runs on it.
///
/// The subscriber is the global default, not a scoped one: `tracing` caches the
/// interest of a callsite for the whole process, and the first test of this
/// binary that reaches the harness warning registers it. A `with_default`
/// subscriber that appears later cannot reliably undo a cached `never`, because
/// its rebuild races with that registration and a stale rebuild wins in about
/// one run in six. Every test that logs, or that reaches the harness check,
/// calls this first, so the callsite is only ever registered with this
/// subscriber in the process.
fn warn_logs() {
    static MOUNTED: Once = Once::new();
    MOUNTED.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(ThreadLog)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("this test binary sets one subscriber");
    });
    LINES.with(|lines| lines.borrow_mut().clear());
}

/// The log lines of this thread since [`warn_logs`].
fn warn_text() -> String {
    LINES.with(|lines| String::from_utf8_lossy(&lines.borrow()).into_owned())
}

/// The writer of [`warn_logs`]: the lines of a test go to its own thread.
struct ThreadLog;

impl Write for ThreadLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LINES.with(|lines| lines.borrow_mut().extend_from_slice(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLog {
    type Writer = ThreadLog;

    fn make_writer(&'a self) -> Self::Writer {
        ThreadLog
    }
}

/// A declared harness whose program is not installed is not offered, and the
/// declaration stays, so a later install needs no restart.
#[test]
fn harness_missing_program_is_not_offered() {
    warn_logs();
    let dir = tempfile::tempdir().unwrap();
    let omp = omp_program(dir.path());
    let host = host(dir.path(), |cfg| {
        cfg.harnesses = vec![Harness::Fake, Harness::Omp];
        cfg.commands.omp = Some(vec![omp.to_string_lossy().into_owned()]);
    });

    assert_eq!(host.offered_harnesses(), vec![Harness::Fake]);
    assert_eq!(host.offered_info().harnesses, vec![Harness::Fake]);
    assert_eq!(host.config().harnesses, vec![Harness::Fake, Harness::Omp]);
}

/// One warning names the harness and its program, and the next check of the
/// same absence stays quiet.
#[test]
fn harness_missing_program_warns_once() {
    warn_logs();
    let dir = tempfile::tempdir().unwrap();
    let omp = omp_program(dir.path());
    let host = host(dir.path(), |cfg| {
        cfg.harnesses = vec![Harness::Fake, Harness::Omp];
        cfg.commands.omp = Some(vec![omp.to_string_lossy().into_owned()]);
    });
    host.offered_harnesses();
    host.offered_harnesses();

    let text = warn_text();
    assert_eq!(text.matches("is not installed").count(), 1, "{text}");
    assert!(text.contains("harness=omp"), "{text}");
    assert!(text.contains(omp.to_str().unwrap()), "{text}");
    // The harness that was found is not in the warning.
    assert!(!text.contains("harness=fake"), "{text}");
}

/// A program that is installed while the daemon runs reaches the hub in the
/// next registration, without a restart.
#[tokio::test]
async fn harness_program_that_appears_is_offered_on_the_next_heartbeat() {
    warn_logs();
    let dir = tempfile::tempdir().unwrap();
    let omp = omp_program(dir.path());
    let bus = MemoryBus::new();
    let (registered, stop) = record_hub(&bus).await;
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().join(".borant"));
    cfg.harnesses = vec![Harness::Fake, Harness::Omp];
    cfg.commands.fake = Some(vec!["true".to_owned()]);
    cfg.commands.omp = Some(vec![omp.to_string_lossy().into_owned()]);
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );

    host.heartbeat_once().await;
    assert_eq!(*registered.lock().unwrap(), vec![vec![Harness::Fake]]);

    std::fs::create_dir_all(omp.parent().unwrap()).unwrap();
    std::fs::write(&omp, "#!/bin/sh\n").unwrap();
    host.heartbeat_once().await;
    assert_eq!(
        *registered.lock().unwrap(),
        vec![vec![Harness::Fake], vec![Harness::Fake, Harness::Omp]],
        "the next heartbeat offers the harness"
    );
    stop.cancel();
}

/// A program that goes away is dropped from the next registration, so the hub
/// stops placing agents there.
#[tokio::test]
async fn harness_program_that_goes_away_leaves_the_next_registration() {
    warn_logs();
    let dir = tempfile::tempdir().unwrap();
    let omp = omp_program(dir.path());
    std::fs::create_dir_all(omp.parent().unwrap()).unwrap();
    std::fs::write(&omp, "#!/bin/sh\n").unwrap();
    let bus = MemoryBus::new();
    let (registered, stop) = record_hub(&bus).await;
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().join(".borant"));
    cfg.harnesses = vec![Harness::Fake, Harness::Omp];
    cfg.commands.fake = Some(vec!["true".to_owned()]);
    cfg.commands.omp = Some(vec![omp.to_string_lossy().into_owned()]);
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );

    host.heartbeat_once().await;
    std::fs::remove_file(&omp).unwrap();
    host.heartbeat_once().await;
    assert_eq!(
        *registered.lock().unwrap(),
        vec![vec![Harness::Fake, Harness::Omp], vec![Harness::Fake]]
    );
    stop.cancel();
}

/// A launch of a harness whose program is gone fails at once, with the program
/// named, and nothing is prepared for the agent.
#[tokio::test]
async fn harness_launch_without_a_program_fails_at_once() {
    warn_logs();
    let dir = tempfile::tempdir().unwrap();
    let omp = omp_program(dir.path());
    let host = host(dir.path(), |cfg| {
        cfg.harnesses = vec![Harness::Fake, Harness::Omp];
        cfg.commands.omp = Some(vec![omp.to_string_lossy().into_owned()]);
    });

    let started = std::time::Instant::now();
    let err = host
        .launch(LaunchOrder {
            agent: "w1".parse().unwrap(),
            harness: Harness::Omp,
            ..Default::default()
        })
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    let msg = format!("{err:#}");
    assert!(
        elapsed < Duration::from_secs(2),
        "the launch waited {elapsed:?}: {msg}"
    );
    assert!(msg.contains("is not installed on host h1"), "{msg}");
    assert!(
        msg.contains("omp") && msg.contains(omp.to_str().unwrap()),
        "{msg}"
    );
    // The check comes before the work dir and the secrets of the launch.
    assert!(!dir.path().join(".borant/work/w1").exists());
}
