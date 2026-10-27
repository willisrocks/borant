//! The checkpoint pushes of a session run with the variables of its launch,
//! as the restore of the launch does, so a collection bundle carries the
//! credential of the checkpoint remote for the life of the session. The
//! values reach git only as the environment of its child process.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use borant_core::model::{Harness, MoveReason, Project, RepoInfo};
use borant_core::ports::secrets::{Bundle, BundleName, SecretRef};
use borant_core::proto::{HostCommand, LaunchOrder, LocalEvent, LocalRegister};
use borant_core::{AgentId, SessionId, Timestamp};
use borant_host::{HostConfig, HostSecrets, checkpoint};
use borant_secrets::EnvStore;
use common::Fx;

/// The value of the token of the bundle of these tests.
const TOKEN: &str = "checkpoint-token-value-123456";

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

/// A bare remote with `a.txt` on `main`.
fn remote(dir: &Path) -> PathBuf {
    let remote = dir.join("web.git");
    let seed = dir.join("seed");
    git(
        dir,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(dir, &["init", "-q", "-b", "main", seed.to_str().unwrap()]);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    git(&seed, &["add", "a.txt"]);
    git(
        &seed,
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
    );
    git(&seed, &["push", "-q", remote.to_str().unwrap(), "main"]);
    remote
}

/// A hooks directory whose `pre-push` hook records the environment of the
/// git process. Only the bundle points git at it (through `GIT_CONFIG_*`), so
/// a record exists exactly when a push runs with the variables of the launch.
fn hooks(dir: &Path, record: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let hook = dir.join("pre-push");
    std::fs::write(&hook, format!("#!/bin/sh\nenv > '{}'\n", record.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.to_path_buf()
}

/// The variables that make git run the hooks in `dir`, plus the token.
fn vars(dir: &Path) -> Vec<(String, String)> {
    vec![
        ("CHECKPOINT_TOKEN".into(), TOKEN.into()),
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "core.hooksPath".into()),
        (
            "GIT_CONFIG_VALUE_0".into(),
            dir.to_string_lossy().into_owned(),
        ),
    ]
}

/// The host secrets of one bundle `work` of the store `env` whose variables
/// are `vars`.
fn secrets(vars: &[(String, String)]) -> HostSecrets {
    let name: BundleName = "work".parse().unwrap();
    let bundle = Bundle {
        name: name.clone(),
        store: "env".into(),
        vars: vars
            .iter()
            .map(|(k, _)| (k.clone(), SecretRef(format!("ref-{k}"))))
            .collect(),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::new(),
    };
    HostSecrets {
        stores: BTreeMap::from([(
            "env".to_owned(),
            Arc::new(EnvStore::from_vars(
                vars.iter().map(|(k, v)| (format!("ref-{k}"), v.clone())),
            )) as _,
        )]),
        bundles: BTreeMap::from([(name, bundle)]),
        harness: BTreeMap::new(),
    }
}

fn order(remote: &Path) -> LaunchOrder {
    LaunchOrder {
        agent: agent(),
        harness: Harness::Fake,
        repo: Some(RepoInfo {
            remote_url: remote.to_string_lossy().into_owned(),
            branch: "main".into(),
            subdir: String::new(),
        }),
        wip: None,
        channels: Vec::new(),
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
            checkpoint_remote: None,
            setup: None,
            test: None,
            collection: None,
            mirror_issues: None,
        }),
        members: BTreeMap::new(),
        bundles: vec!["work".parse().unwrap()],
        git_author: None,
        sandbox: None,
    }
}

async fn fx(root: PathBuf, secrets: Option<HostSecrets>) -> Fx {
    Fx::new(true, move |cfg: &mut HostConfig| {
        cfg.commands.fake = Some(vec!["true".into()]);
        cfg.secrets_root = Some(root);
        cfg.secrets = secrets;
    })
    .await
}

/// Registers a session of the agent in `cwd`, as the harness of a launch does.
async fn register(fx: &Fx, cwd: &Path) -> SessionId {
    fx.host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent()),
            ..Default::default()
        })
        .await
        .unwrap()
        .session
}

#[tokio::test]
async fn the_checkpoint_of_a_launched_session_pushes_with_the_bundle_vars() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let dir = hooks(&root.path().join("hooks"), &record);
    let fx = fx(root.path().join("agents"), Some(secrets(&vars(&dir)))).await;
    let remote = remote(fx.dir.path());
    fx.host.launch(order(&remote)).await.unwrap();
    let work = fx.dir.path().join("work/w1/web");
    let session = register(&fx, &work).await;
    fx.host.event(session, LocalEvent::TurnEnd).await.unwrap();
    std::fs::write(work.join("b.txt"), "work in progress\n").unwrap();

    // A park writes a final checkpoint before it stops the session.
    fx.host.handle_command(HostCommand::Park { session }).await;

    let files = git(&remote, &["ls-tree", "--name-only", "refs/borant/w1/wip"]);
    assert!(files.contains("b.txt"), "the checkpoint: {files}");
    let seen = std::fs::read_to_string(&record).expect("the push saw no variables");
    assert!(
        seen.contains(&format!("CHECKPOINT_TOKEN={TOKEN}")),
        "{seen}"
    );
}

#[tokio::test]
async fn the_checkpoint_of_a_session_without_bundles_pushes_without_variables() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let _dir = hooks(&root.path().join("hooks"), &record);
    let fx = fx(root.path().join("agents"), None).await;
    let remote = remote(fx.dir.path());
    fx.host.launch(order(&remote)).await.unwrap();
    let work = fx.dir.path().join("work/w1/web");
    let session = register(&fx, &work).await;
    fx.host.event(session, LocalEvent::TurnEnd).await.unwrap();
    std::fs::write(work.join("b.txt"), "work in progress\n").unwrap();

    fx.host.handle_command(HostCommand::Park { session }).await;

    let files = git(&remote, &["ls-tree", "--name-only", "refs/borant/w1/wip"]);
    assert!(files.contains("b.txt"), "the checkpoint: {files}");
    assert!(
        !record.exists(),
        "a session without bundles pushed with variables"
    );
}

/// A work tree with one commit whose `origin` is `remote`.
fn work_tree(root: &Path, remote: &Path) -> PathBuf {
    let work = root.join("work");
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("b.txt"), "work in progress\n").unwrap();
    work
}

#[tokio::test]
async fn create_and_save_stale_push_with_the_given_vars() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let dir = hooks(&root.path().join("hooks"), &record);
    let remote = remote(root.path());
    let work = work_tree(root.path(), &remote);
    let r = remote.to_str().unwrap();

    checkpoint::create(&work, r, &agent(), None, &vars(&dir))
        .await
        .unwrap();
    let seen = std::fs::read_to_string(&record).expect("create pushed without the variables");
    assert!(
        seen.contains(&format!("CHECKPOINT_TOKEN={TOKEN}")),
        "{seen}"
    );

    std::fs::remove_file(&record).unwrap();
    checkpoint::save_stale(&work, r, &agent(), Timestamp(42), &vars(&dir))
        .await
        .unwrap();
    let seen = std::fs::read_to_string(&record).expect("save_stale pushed without the variables");
    assert!(
        seen.contains(&format!("CHECKPOINT_TOKEN={TOKEN}")),
        "{seen}"
    );
}

#[tokio::test]
async fn a_failed_push_names_no_variable_value() {
    let root = tempfile::tempdir().unwrap();
    let remote = remote(root.path());
    let work = work_tree(root.path(), &remote);
    let gone = root.path().join("gone.git");
    let env = vec![("CHECKPOINT_TOKEN".to_owned(), TOKEN.to_owned())];

    let err = checkpoint::create(&work, gone.to_str().unwrap(), &agent(), None, &env)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("push"), "{msg}");
    assert!(!msg.contains(TOKEN), "the error holds a value: {msg}");
}

#[tokio::test]
async fn a_bundle_cannot_turn_off_the_prompt_guard() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let dir = hooks(&root.path().join("hooks"), &record);
    let remote = remote(root.path());
    let work = work_tree(root.path(), &remote);
    let mut env = vars(&dir);
    env.push(("GIT_TERMINAL_PROMPT".into(), "1".into()));

    checkpoint::create(&work, remote.to_str().unwrap(), &agent(), None, &env)
        .await
        .unwrap();

    let seen = std::fs::read_to_string(&record).expect("the push saw no variables");
    assert!(
        seen.lines().any(|line| line == "GIT_TERMINAL_PROMPT=0"),
        "the bundle turned the guard off: {seen}"
    );
}
