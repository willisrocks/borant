//! The restore of a launch runs with the launch's bundle variables, so a
//! collection bundle can carry the git credential of its repositories, and
//! the host has no git-specific configuration of its own. The values reach
//! the git commands and the setup only as the environment of their child
//! processes.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use borant_core::model::{Harness, MoveReason, Project, RepoInfo};
use borant_core::ports::secrets::{Bundle, BundleName, SecretRef};
use borant_core::proto::LaunchOrder;
use borant_host::{HostConfig, HostSecrets};
use borant_secrets::EnvStore;
use common::Fx;

/// The value of the token of the bundle of these tests. At least eight
/// characters, so the redactor of the launch would match it.
const TOKEN: &str = "clone-token-value-123456";

fn bundle_name(name: &str) -> BundleName {
    name.parse().unwrap()
}

fn refs(pairs: &[(&str, &str)]) -> BTreeMap<String, SecretRef> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), SecretRef((*v).to_owned())))
        .collect()
}

/// A bare remote with `a.txt` on `main`.
fn remote(dir: &Path, name: &str) -> PathBuf {
    let remote = dir.join(name);
    let seed = dir.join(format!("seed-{name}"));
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

/// A template directory whose `post-checkout` hook records the environment of
/// the git process. Only a variable of the bundle names this directory, so a
/// record exists exactly when the git commands of a restore run with them.
fn template(dir: &Path, record: &Path) -> PathBuf {
    let hooks = dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\nenv > '{}'\n", record.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.to_path_buf()
}

/// The host secrets of `vars`: one bundle named `work` of the store `env`,
/// with one ref for each `ENV_NAME = ref` pair.
fn secrets(vars: &[(&str, &str)], store_vars: &[(&str, &str)]) -> HostSecrets {
    let bundle = Bundle {
        name: bundle_name("work"),
        store: "env".into(),
        vars: refs(vars),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::new(),
    };
    HostSecrets {
        stores: BTreeMap::from([(
            "env".to_owned(),
            Arc::new(EnvStore::from_vars(
                store_vars
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
            )) as _,
        )]),
        bundles: BTreeMap::from([(bundle_name("work"), bundle)]),
        harness: BTreeMap::new(),
    }
}

fn order(remote: &Path, setup: Option<&str>) -> LaunchOrder {
    LaunchOrder {
        agent: "w1".parse().unwrap(),
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
            setup: setup.map(str::to_owned),
            test: None,
            collection: None,
            mirror_issues: None,
        }),
        members: BTreeMap::new(),
        bundles: vec![bundle_name("work")],
        git_author: None,
        sandbox: None,
    }
}

/// A host whose values live in `root`, out of the home of the fixture.
async fn fx(root: PathBuf, secrets: Option<HostSecrets>) -> Fx {
    Fx::new(false, move |cfg: &mut HostConfig| {
        cfg.commands.fake = Some(vec!["true".into()]);
        cfg.secrets_root = Some(root);
        cfg.secrets = secrets;
    })
    .await
}

#[tokio::test]
async fn the_restore_of_a_launch_runs_with_the_bundle_vars() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let probe = root.path().join("probe");
    let tpl = template(&probe, &record);
    let tpl_value = tpl.to_string_lossy().into_owned();
    let fx = fx(
        root.path().join("agents"),
        Some(secrets(
            &[("CLONE_TOKEN", "clone-token"), ("GIT_TEMPLATE_DIR", "tpl")],
            &[("clone-token", TOKEN), ("tpl", &tpl_value)],
        )),
    )
    .await;
    let remote = remote(fx.dir.path(), "web.git");

    fx.host.launch(order(&remote, None)).await.unwrap();

    // The hook of the git process of the clone wrote the environment.
    let seen = std::fs::read_to_string(&record).expect("the git commands saw no variables");
    assert!(seen.contains(&format!("CLONE_TOKEN={TOKEN}")), "{seen}");
    assert!(
        seen.contains(&format!("GIT_TEMPLATE_DIR={}", tpl.display())),
        "{seen}"
    );
    // A value goes into the child process env only: nothing of the clone
    // holds it.
    let clone = fx.dir.path().join("work/w1/web");
    for entry in walk(&clone) {
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(!text.contains(TOKEN), "{} holds the value", entry.display());
    }
}

/// The paths of the files under `dir`.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test]
async fn a_bundle_var_wins_over_the_same_name_in_the_daemon_env() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let probe = root.path().join("probe");
    let tpl = template(&probe, &record);
    // The daemon of a laptop exports its own `HOME`, and the bundle overrides
    // it, like it does for the harness.
    let home = root.path().join("bundle-home");
    std::fs::create_dir_all(&home).unwrap();
    let daemon_home = std::env::var("HOME").expect("a home in the test env");
    let bundle_home = home.to_string_lossy().into_owned();
    let tpl_value = tpl.to_string_lossy().into_owned();
    assert_ne!(daemon_home, bundle_home);
    let fx = fx(
        root.path().join("agents"),
        Some(secrets(
            &[("HOME", "bundle-home"), ("GIT_TEMPLATE_DIR", "tpl")],
            &[("bundle-home", &bundle_home), ("tpl", &tpl_value)],
        )),
    )
    .await;
    let remote = remote(fx.dir.path(), "web.git");

    fx.host.launch(order(&remote, None)).await.unwrap();

    let seen = std::fs::read_to_string(&record).expect("the git commands saw no variables");
    assert!(
        seen.lines()
            .any(|line| line == format!("HOME={}", home.display())),
        "the inherited HOME won: {seen}"
    );
}

#[tokio::test]
async fn a_launch_without_bundles_clones_without_any_variable() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("env.txt");
    let probe = root.path().join("probe");
    let _tpl = template(&probe, &record);
    let fx = fx(root.path().join("agents"), None).await;
    let remote = remote(fx.dir.path(), "web.git");

    fx.host.launch(order(&remote, None)).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("work/w1/web/a.txt")).unwrap(),
        "one\n"
    );
    assert!(
        !record.exists(),
        "a launch without bundles ran git with variables"
    );
    assert!(
        !root.path().join("agents/w1").exists(),
        "a launch without bundles wrote a secret directory"
    );
}

#[tokio::test]
async fn a_bundle_whose_value_does_not_resolve_fails_before_the_clone() {
    let root = tempfile::tempdir().unwrap();
    let fx = fx(
        root.path().join("agents"),
        Some(secrets(&[("CLONE_TOKEN", "missing")], &[])),
    )
    .await;
    let remote = remote(fx.dir.path(), "web.git");

    let err = fx.host.launch(order(&remote, None)).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("missing"), "the error names the ref: {msg}");
    assert!(!msg.contains(TOKEN), "the error holds a value: {msg}");
    assert!(
        !fx.dir.path().join("work/w1/web").exists(),
        "a launch without its values cloned anyway"
    );
}

#[tokio::test]
async fn a_failed_restore_of_a_launch_removes_its_values() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("agents");
    let fx = fx(
        dir.clone(),
        Some(secrets(
            &[("CLONE_TOKEN", "clone-token")],
            &[("clone-token", TOKEN)],
        )),
    )
    .await;
    // A remote that no clone reaches.
    let missing = root.path().join("no-such-remote.git");

    let err = fx.host.launch(order(&missing, None)).await.unwrap_err();
    assert!(
        !format!("{err:#}").contains(TOKEN),
        "the error holds a value: {err:#}"
    );
    assert!(
        !dir.join("w1").exists(),
        "a launch that failed left its values on disk"
    );
}

#[tokio::test]
async fn the_setup_of_a_project_runs_with_the_bundle_vars() {
    let root = tempfile::tempdir().unwrap();
    let fx = fx(
        root.path().join("agents"),
        Some(secrets(
            &[("CLONE_TOKEN", "clone-token")],
            &[("clone-token", TOKEN)],
        )),
    )
    .await;
    let remote = remote(fx.dir.path(), "web.git");

    // The setup fails unless the value reaches it.
    let setup = "test -n \"$CLONE_TOKEN\" && echo \'with the vars\' > setup.log";
    fx.host.launch(order(&remote, Some(setup))).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("work/w1/web/setup.log")).unwrap(),
        "with the vars\n"
    );
}
