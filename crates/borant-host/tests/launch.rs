mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use borant_core::model::{Harness, MoveReason, Project, RepoInfo};
use borant_core::proto::LaunchOrder;
use borant_host::checkpoint;
use common::Fx;

/// A bare remote with one commit on `main`, with `a.txt` and `sub/b.txt`.
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
    std::fs::create_dir(seed.join("sub")).unwrap();
    std::fs::write(seed.join("sub/b.txt"), "two\n").unwrap();
    run(&["add", "."], &seed);
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
        reserve_secs: None,
        project: Some(Project {
            name: "web".parse().unwrap(),
            remote_url: remote.to_string_lossy().into_owned(),
            default_branch: "main".into(),
            checkpoint_remote: Some("/srv/ckpt.git".into()),
            setup: Some(setup.into()),
            test: None,
            collection: None,
        }),
        ..Default::default()
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
    // The new clone borrows the objects of the mirror of the project.
    let mirror = fx.dir.path().join("projects/web.git");
    assert!(mirror.join("HEAD").is_file(), "no mirror at {mirror:?}");
    let alternates = std::fs::read_to_string(clone.join(".git/objects/info/alternates"))
        .expect("a project clone has an alternates file");
    assert!(
        alternates.contains(&mirror.join("objects").to_string_lossy().into_owned()),
        "alternates {alternates:?} do not name the mirror {mirror:?}"
    );
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn setup_time_fits_inside_the_reservation() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let mut o = order(&remote, "sleep 30");
    // 62 s of reservation leave 2 s after the 60 s margin.
    o.reserve_secs = Some(62);
    let t = std::time::Instant::now();
    let err = fx.host.launch(o).await.unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(t.elapsed() < std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn background_process_of_setup_does_not_hold_the_launch() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let t = std::time::Instant::now();
    fx.host
        .launch(order(&remote, "sleep 30 & echo started"))
        .await
        .unwrap();
    assert!(t.elapsed() < std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn second_launch_of_one_agent_fails_fast() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let first = order(&remote, "sleep 2");
    let second = first.clone();
    let host = fx.host.clone();
    let running = tokio::spawn(async move { host.launch(first).await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let err = fx.host.launch(second).await.unwrap_err();
    assert!(format!("{err:#}").contains("already runs"), "{err:#}");
    running.await.unwrap().unwrap();
    // The lock ends with the launch.
    fx.host.launch(order(&remote, "true")).await.unwrap();
}

#[tokio::test]
async fn setup_marker_lives_in_the_git_dir_for_a_subdir() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let mut o = order(&remote, "echo ran >> ../setup.log");
    o.repo.as_mut().unwrap().subdir = "sub".into();
    fx.host.launch(o.clone()).await.unwrap();
    fx.host.launch(o).await.unwrap();
    let clone = fx.dir.path().join("work/w1/web");
    assert!(clone.join(".git/borant-setup-done").exists());
    assert_eq!(
        std::fs::read_to_string(clone.join("setup.log")).unwrap(),
        "ran\n"
    );
}

#[tokio::test]
async fn new_channel_branch_starts_from_the_default_branch() {
    let fx = fx().await;
    let remote = remote(fx.dir.path());
    let mut o = order(&remote, "true");
    o.repo.as_mut().unwrap().branch = "feat/health".into();
    fx.host.launch(o).await.unwrap();
    let clone = fx.dir.path().join("work/w1/web");
    assert_eq!(
        git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/health"
    );
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&remote, &["rev-parse", "main"])
    );
}
