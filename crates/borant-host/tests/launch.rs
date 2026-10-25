mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use borant_core::model::{Harness, MoveReason, Project, RepoInfo};
use borant_core::proto::LaunchOrder;
use borant_host::checkpoint;
use common::Fx;

/// A bare remote with one commit on `main`.
fn remote(dir: &Path) -> PathBuf {
    let remote = dir.join("web.git");
    let seed = dir.join("seed");
    let run = |args: &[&str], cwd: &Path| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    run(
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
        dir,
    );
    run(&["init", "-q", "-b", "main", seed.to_str().unwrap()], dir);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    run(&["add", "a.txt"], &seed);
    run(
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
        &seed,
    );
    run(&["push", "-q", remote.to_str().unwrap(), "main"], &seed);
    remote
}

fn order(remote: &Path, setup: &str) -> LaunchOrder {
    LaunchOrder {
        agent: "w1".parse().unwrap(),
        harness: Harness::Fake,
        repo: Some(RepoInfo {
            remote_url: remote.to_string_lossy().into_owned(),
            branch: "main".into(),
            subdir: String::new(),
        }),
        wip: None,
        channels: vec!["api".parse().unwrap()],
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        from_harness: None,
        from_host: None,
        model: None,
        project: Some(Project {
            name: "web".parse().unwrap(),
            remote_url: remote.to_string_lossy().into_owned(),
            default_branch: "main".into(),
            checkpoint_remote: Some("/srv/ckpt.git".into()),
            setup: Some(setup.into()),
            test: None,
        }),
    }
}

async fn fx() -> Fx {
    Fx::new(false, |cfg| cfg.commands.fake = Some(vec!["true".into()])).await
}

#[tokio::test]
async fn setup_runs_once_in_a_new_clone() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let o = order(&remote, "echo ran >> setup.log");
    fx.host.launch(o.clone()).await.unwrap();
    let clone = fx.dir.path().join("work/w1/web");
    assert_eq!(
        std::fs::read_to_string(clone.join("setup.log")).unwrap(),
        "ran\n"
    );
    // A restore of the same clone skips the setup.
    fx.host.launch(o).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(clone.join("setup.log")).unwrap(),
        "ran\n"
    );
    // The clone keeps the checkpoint remote of the project.
    let facts = checkpoint::detect(&clone, None).await.unwrap();
    assert_eq!(facts.checkpoint_remote.as_deref(), Some("/srv/ckpt.git"));
}

#[tokio::test]
async fn failed_setup_fails_the_launch_with_its_output() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let err = fx
        .host
        .launch(order(&remote, "echo boom; echo bad >&2; exit 3"))
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("boom") && msg.contains("bad"), "{msg}");
}
