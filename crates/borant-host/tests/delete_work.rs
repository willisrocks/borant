//! The `DeleteWork` command of a retired agent's host: the clone goes away
//! when no session of the agent runs, and stays while one does.

mod common;

use borant_core::proto::HostCommand;
use common::Fx;

/// A work dir of `agent` with one file in it.
fn work_dir(fx: &Fx, agent: &str) -> std::path::PathBuf {
    let dir = fx.host.config().home.join("work").join(agent);
    std::fs::create_dir_all(dir.join("clone/.git")).unwrap();
    std::fs::write(dir.join("clone/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
    dir
}

#[tokio::test]
async fn delete_work_removes_the_clone_of_a_retired_agent() {
    let fx = Fx::new(false, |_| {}).await;
    let dir = work_dir(&fx, "w1");
    fx.host
        .handle_command(HostCommand::DeleteWork {
            agent: "w1".parse().unwrap(),
        })
        .await;
    assert!(!dir.exists(), "the work dir is gone");
}

#[tokio::test]
async fn delete_work_keeps_the_clone_of_a_live_session() {
    let fx = Fx::new(true, |_| {}).await;
    let dir = work_dir(&fx, "w1");
    // A session of the same agent runs on this host.
    fx.session("w1", "work/w1").await;
    fx.host
        .handle_command(HostCommand::DeleteWork {
            agent: "w1".parse().unwrap(),
        })
        .await;
    assert!(dir.exists(), "the work dir stays while a session runs");
}
