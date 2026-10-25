//! Parking a session from the hub: the final checkpoint, the stop, and the
//! refusals (spec "Parking").

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use borant_core::model::{Harness, SessionEventBody};
use borant_core::ports::SessionStore;
use borant_core::proto::{HostCommand, LocalEvent, LocalRegister};
use borant_core::{AgentId, SessionId};
use common::Fx;

fn agent() -> AgentId {
    "w1".parse().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare remote and a clone with one commit on `main` under `root`. The
/// `origin` of the clone is `origin`, or the bare remote.
fn clone(root: &Path, origin: Option<&str>) -> (PathBuf, PathBuf) {
    let remote = root.join("remote.git");
    let status = Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&remote)
        .status()
        .unwrap();
    assert!(status.success());
    let work = root.join("work");
    let status = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&work)
        .status()
        .unwrap();
    assert!(status.success());
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "-m", "init"]);
    let url = origin
        .map(str::to_owned)
        .unwrap_or_else(|| remote.to_string_lossy().into_owned());
    git(&work, &["remote", "add", "origin", &url]);
    (remote, work)
}

/// Registers a session in `cwd` with the hub of the fixture.
async fn register(fx: &Fx, cwd: &Path) -> SessionId {
    fx.host
        .register(LocalRegister {
            harness: Harness::Fake,
            harness_session_id: None,
            pid: None,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent()),
            channel: None,
            role: None,
            collection: None,
        })
        .await
        .unwrap()
        .session
}

/// The reason of the last `SessionEnd` of the agent, if the session ended.
async fn end_reason(fx: &Fx) -> Option<String> {
    fx.sessions
        .read(&agent())
        .await
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find_map(|(_, e)| match e.body {
            SessionEventBody::SessionEnd { reason } => Some(reason),
            _ => None,
        })
}

#[tokio::test]
async fn does_not_park_a_session_in_a_turn() {
    let fx = Fx::new(true, |_| {}).await;
    let (_remote, work) = clone(fx.dir.path(), None);
    let session = register(&fx, &work).await;
    fx.host.event(session, LocalEvent::TurnStart).await.unwrap();
    fx.host.handle_command(HostCommand::Park { session }).await;
    assert!(
        fx.host.local_view(session).is_ok(),
        "a session in a turn keeps running"
    );
    assert_eq!(end_reason(&fx).await, None);
}

#[tokio::test]
async fn parks_an_idle_session_after_a_final_checkpoint() {
    let fx = Fx::new(true, |_| {}).await;
    let (remote, work) = clone(fx.dir.path(), None);
    let session = register(&fx, &work).await;
    fx.host.event(session, LocalEvent::TurnEnd).await.unwrap();
    std::fs::write(work.join("b.txt"), "parked work\n").unwrap();
    fx.host.handle_command(HostCommand::Park { session }).await;
    let files = git(&remote, &["ls-tree", "--name-only", "refs/borant/w1/wip"]);
    assert!(files.contains("b.txt"), "the final checkpoint: {files}");
    assert!(
        fx.host.local_view(session).is_err(),
        "the host stopped the session"
    );
    assert_eq!(end_reason(&fx).await.as_deref(), Some("park"));
}

#[tokio::test]
async fn keeps_a_session_when_the_park_checkpoint_fails() {
    let fx = Fx::new(true, |_| {}).await;
    let gone = fx.dir.path().join("gone.git");
    let (_remote, work) = clone(fx.dir.path(), Some(gone.to_str().unwrap()));
    let session = register(&fx, &work).await;
    fx.host.event(session, LocalEvent::TurnEnd).await.unwrap();
    std::fs::write(work.join("b.txt"), "no remote to push to\n").unwrap();
    fx.host.handle_command(HostCommand::Park { session }).await;
    assert!(
        fx.host.local_view(session).is_ok(),
        "a failed checkpoint keeps the session running"
    );
    assert_eq!(end_reason(&fx).await, None);
}
