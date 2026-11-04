//! Stop and launch of a session that a sleep left behind: a stop must end the
//! harness even when it ignores SIGTERM, and a launch must end the leftover
//! tmux session of its own name, or `new-session` refuses it forever.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use borant_core::model::{Harness, MoveReason};
use borant_core::proto::{HostCommand, LaunchOrder, LocalRegister, LocalState};
use borant_core::{AgentId, SystemClock};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_launch::{TmuxConfig, TmuxLauncher};
use borant_memory::{MemoryBus, MemorySession};

mod common;
use common::Fx;

/// A pid that no other test or process uses: the name of a tmux session, and of
/// an agent.
fn unique(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{}-{nanos}", std::process::id())
}

fn tmux_available() -> bool {
    if Command::new("tmux").arg("-V").output().is_ok() {
        return true;
    }
    eprintln!("skipping: tmux is not on PATH");
    false
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .expect("run tmux")
}

/// Kills the process when the test ends, even if an assert fails.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// Kills the tmux session when the test ends, on its own socket.
struct KillSessionOnDrop {
    socket: String,
    name: String,
}

impl Drop for KillSessionOnDrop {
    fn drop(&mut self) {
        let _ = tmux(
            &self.socket,
            &["kill-session", "-t", &format!("={}", self.name)],
        );
    }
}

/// A busy session whose harness ignores SIGTERM, as a lid close leaves one: a
/// stop must end it, not only pause the agent.
#[tokio::test]
async fn a_stop_ends_a_busy_session_that_ignores_sigterm() {
    let fx = Fx::new(true, |_| {}).await;
    // The shell ignores SIGTERM and keeps its turn; a stop has to escalate.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; while :; do sleep 1; done")
        .stdin(Stdio::null())
        .spawn()
        .expect("start the harness");
    let pid = child.id();
    let _guard = KillOnDrop(pid);
    let cwd = fx.dir.path().join("busy");
    std::fs::create_dir_all(&cwd).unwrap();
    let session = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            pid: Some(pid),
            agent: Some("w1".parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(session.state, LocalState::Attached, "{session:?}");
    // No turn end and no idle report: the hub's stop waits for a turn that a
    // hung harness never ends, so the host must end the process itself.
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;
    // The child is this test's own, so an exit that the test reaps is the
    // proof: a killed process that nothing reaped still answers a signal.
    let status = child.try_wait().expect("wait for the harness");
    assert!(
        status.is_some(),
        "the harness still runs after the stop: {}",
        session.session
    );
}

/// A host whose launcher starts agents in tmux on its own socket.
fn tmux_host(dir: &std::path::Path, socket: &str) -> Host {
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    // A program that stays in its pane, so the session is there to look at.
    cfg.commands.fake = Some(vec![
        "sh".into(),
        "-c".into(),
        "echo launched; sleep 30".into(),
    ]);
    Host::new(
        cfg,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(TmuxLauncher::new(TmuxConfig {
            socket: socket.to_owned(),
        })),
        None,
        None,
        Outbox::in_memory().unwrap(),
    )
}

fn screen(socket: &str, name: &str) -> String {
    let out = tmux(socket, &["capture-pane", "-p", "-t", &format!("={name}:")]);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A launch of an agent finds its own session still on the host, from a
/// restart or a sleep: the leftover goes and the launch takes its place. This
/// is the "Conflict: tmux session borant-cloud-design already exists" of a
/// resume after a Mac sleep.
#[tokio::test]
async fn a_launch_ends_a_leftover_tmux_session_of_the_agent() {
    if !tmux_available() {
        return;
    }
    let socket = unique("test-stop-launch");
    let agent = unique("w");
    let name = format!("borant-{agent}");
    let _session = KillSessionOnDrop {
        socket: socket.clone(),
        name: name.clone(),
    };
    let leftover = tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-s",
            &name,
            "--",
            "sh",
            "-c",
            "echo leftover; sleep 300",
        ],
    );
    assert!(leftover.status.success(), "tmux new-session: {leftover:?}");

    let dir = tempfile::tempdir().unwrap();
    let host = tmux_host(dir.path(), &socket);
    let agent: AgentId = agent.parse().unwrap();
    let order = LaunchOrder {
        agent: agent.clone(),
        harness: Harness::Fake,
        repo: None,
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        ..Default::default()
    };
    // Without the release of the leftover this fails with a conflict.
    host.launch(order).await.expect("the launch of the agent");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = screen(&socket, &name);
        if text.contains("launched") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the leftover session is still there after 5 s: {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !screen(&socket, &name).contains("leftover"),
        "the new session shows the old program"
    );
    let _ = PathBuf::new();
}
