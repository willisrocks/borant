//! The bare mirror of a project that the clones of its agents on this host
//! borrow their objects from (spec: one mirror per project on each host).

use std::path::{Path, PathBuf};
use std::process::Command;

use borant_core::ProjectName;
use borant_core::model::RepoInfo;
use borant_host::checkpoint::{self, Mirror};

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

/// A bare remote with `a.txt` on `main`, pushed from a seed clone.
fn remote(dir: &Path, name: &str) -> (String, PathBuf) {
    let remote = dir.join(name);
    let seed = dir.join(format!("seed-{name}"));
    Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&remote)
        .status()
        .unwrap();
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&seed)
        .status()
        .unwrap();
    git(&seed, &["config", "user.email", "t@example.com"]);
    git(&seed, &["config", "user.name", "t"]);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    git(&seed, &["add", "a.txt"]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    git(&seed, &["push", "-q", remote.to_str().unwrap(), "main"]);
    (format!("file://{}", remote.display()), seed)
}

fn project() -> ProjectName {
    "web".parse().unwrap()
}

fn info(url: &str) -> RepoInfo {
    RepoInfo {
        remote_url: url.to_owned(),
        branch: "feature/one".into(),
        subdir: String::new(),
    }
}

/// The objects directory of `dir` that its alternates file names.
fn alternates(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".git/objects/info/alternates")).unwrap_or_default()
}

#[tokio::test]
async fn two_agents_of_a_project_clone_through_one_mirror() {
    let d = tempfile::tempdir().unwrap();
    let (url, _seed) = remote(d.path(), "web.git");
    let projects = d.path().join("projects");
    let work = d.path().join("work");
    let mirror = Mirror {
        root: &projects,
        name: &project(),
    };
    let repo = info(&url);
    let mut dirs = Vec::new();
    for agent in ["w1", "w2"] {
        let agent = agent.parse().unwrap();
        dirs.push(
            checkpoint::restore(
                &work,
                &repo,
                &agent,
                &url,
                None,
                Some("main"),
                Some(mirror),
                &[],
            )
            .await
            .unwrap(),
        );
    }
    let mirror_dir = checkpoint::mirror_dir(&projects, &project());
    assert!(
        mirror_dir.join("HEAD").is_file(),
        "no mirror at {mirror_dir:?}"
    );
    let entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 1, "one mirror, found {entries:?}");
    for dir in &dirs {
        let alternates = alternates(dir);
        assert!(
            alternates.contains(&mirror_dir.join("objects").to_string_lossy().into_owned()),
            "alternates {alternates:?} do not name the mirror {mirror_dir:?}"
        );
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert_eq!(
            git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "feature/one"
        );
        assert_eq!(git(dir, &["remote", "get-url", "origin"]), url);
    }
    assert_ne!(dirs[0], dirs[1]);
    // A restore of an existing agent dir keeps its clone and fetches origin.
    let again = checkpoint::restore(
        &work,
        &repo,
        &"w1".parse().unwrap(),
        &url,
        None,
        Some("main"),
        Some(mirror),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(again, dirs[0]);
    assert_eq!(
        std::fs::read_to_string(again.join("a.txt")).unwrap(),
        "one\n"
    );
    assert!(alternates(&again).contains(&mirror_dir.to_string_lossy().into_owned()));
}

#[tokio::test]
async fn the_mirror_fetches_new_commits_from_the_remote() {
    let d = tempfile::tempdir().unwrap();
    let (url, seed) = remote(d.path(), "web.git");
    let projects = d.path().join("projects");
    let mirror_dir = checkpoint::ensure_mirror(&projects, &project(), &url, &[])
        .await
        .unwrap();
    std::fs::write(seed.join("c.txt"), "three\n").unwrap();
    git(&seed, &["add", "c.txt"]);
    git(&seed, &["commit", "-q", "-m", "three"]);
    git(&seed, &["push", "-q", &url, "main"]);
    let head = git(&seed, &["rev-parse", "HEAD"]);
    checkpoint::ensure_mirror(&projects, &project(), &url, &[])
        .await
        .unwrap();
    assert_eq!(
        git(&mirror_dir, &["rev-parse", "refs/remotes/origin/main"]),
        head
    );
}

#[tokio::test]
async fn concurrent_restores_of_one_project_succeed() {
    let d = tempfile::tempdir().unwrap();
    let (url, _seed) = remote(d.path(), "web.git");
    let projects = d.path().join("projects");
    let work = d.path().join("work");
    let name = project();
    let mirror = Mirror {
        root: &projects,
        name: &name,
    };
    let repo = info(&url);
    let a = "w1".parse().unwrap();
    let b = "w2".parse().unwrap();
    let one = checkpoint::restore(
        &work,
        &repo,
        &a,
        &url,
        None,
        Some("main"),
        Some(mirror),
        &[],
    );
    let two = checkpoint::restore(
        &work,
        &repo,
        &b,
        &url,
        None,
        Some("main"),
        Some(mirror),
        &[],
    );
    let (one, two) = tokio::join!(one, two);
    let one = one.unwrap();
    let two = two.unwrap();
    assert_eq!(std::fs::read_to_string(one.join("a.txt")).unwrap(), "one\n");
    assert_eq!(std::fs::read_to_string(two.join("a.txt")).unwrap(), "one\n");
    let mirror_dir = checkpoint::mirror_dir(&projects, &name);
    assert!(alternates(&one).contains(&mirror_dir.to_string_lossy().into_owned()));
    assert!(alternates(&two).contains(&mirror_dir.to_string_lossy().into_owned()));
}

#[tokio::test]
async fn mirror_never_prunes_or_repacks() {
    let d = tempfile::tempdir().unwrap();
    let (url, _seed) = remote(d.path(), "web.git");
    let projects = d.path().join("projects");
    let mirror_dir = checkpoint::ensure_mirror(&projects, &project(), &url, &[])
        .await
        .unwrap();
    let config = |key: &str| git(&mirror_dir, &["config", key]);
    assert_eq!(
        config("remote.origin.fetch"),
        "+refs/heads/*:refs/remotes/origin/*"
    );
    assert_eq!(config("gc.auto"), "0");
    assert_eq!(config("gc.pruneExpire"), "never");
    assert_eq!(config("maintenance.auto"), "false");
    // A second call fetches the same mirror instead of replacing it.
    let again = checkpoint::ensure_mirror(&projects, &project(), &url, &[])
        .await
        .unwrap();
    assert_eq!(again, mirror_dir);
    assert_eq!(git(&mirror_dir, &["config", "gc.auto"]), "0");
}

#[tokio::test]
async fn a_launch_without_a_project_clones_without_a_mirror() {
    let d = tempfile::tempdir().unwrap();
    let (url, _seed) = remote(d.path(), "web.git");
    let work = d.path().join("work");
    let dir = checkpoint::restore(
        &work,
        &info(&url),
        &"w1".parse().unwrap(),
        &url,
        None,
        Some("main"),
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    assert!(
        !dir.join(".git/objects/info/alternates").exists(),
        "a clone without a project has no alternates"
    );
    assert!(!d.path().join("projects").exists());
}

#[tokio::test]
async fn a_wip_restore_through_the_mirror_returns_the_wip_files() {
    let d = tempfile::tempdir().unwrap();
    let (url, seed) = remote(d.path(), "web.git");
    let (checkpoint_url, _ckpt) = remote(d.path(), "checkpoints.git");
    let work = d.path().join("work");
    let projects = d.path().join("projects");
    let agent = "w1".parse().unwrap();
    std::fs::write(seed.join("a.txt"), "edited\n").unwrap();
    std::fs::write(seed.join("b.txt"), "untracked\n").unwrap();
    let (_, sha) = checkpoint::create(&seed, &checkpoint_url, &agent, None)
        .await
        .unwrap()
        .unwrap();
    let mirror = Mirror {
        root: &projects,
        name: &project(),
    };
    let dir = checkpoint::restore(
        &work,
        &info(&url),
        &agent,
        &checkpoint_url,
        Some(&sha),
        None,
        Some(mirror),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("b.txt")).unwrap(),
        "untracked\n"
    );
    assert_eq!(
        git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/one"
    );
    assert!(
        alternates(&dir).contains(
            &checkpoint::mirror_dir(&projects, &project())
                .to_string_lossy()
                .into_owned()
        )
    );
}
