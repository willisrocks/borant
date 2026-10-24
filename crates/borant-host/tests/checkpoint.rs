use std::path::Path;
use std::process::Command;

use borant_core::{AgentId, Timestamp};
use borant_host::checkpoint;

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

/// A bare remote and a clone with one commit on `main`.
fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let remote = dir.path().join("remote.git");
    let work = dir.path().join("work");
    Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&remote)
        .status()
        .unwrap();
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&work)
        .status()
        .unwrap();
    git(&work, &["config", "user.email", "t@example.com"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-q", "-m", "init"]);
    git(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&work, &["push", "-q", "origin", "main"]);
    (dir, remote, work)
}

fn agent() -> AgentId {
    "w1".parse().unwrap()
}

#[tokio::test]
async fn create_includes_untracked_files() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("new.txt"), "fresh\n").unwrap();
    let (_, sha) = checkpoint::create(&work, remote.to_str().unwrap(), &agent(), None)
        .await
        .unwrap()
        .unwrap();
    let files = git(&remote, &["ls-tree", "--name-only", &sha]);
    assert!(files.contains("new.txt"), "{files}");
    assert_eq!(git(&remote, &["rev-parse", "refs/borant/w1/wip"]), sha);
}

#[tokio::test]
async fn create_skips_unchanged_tree() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "two\n").unwrap();
    let r = remote.to_str().unwrap();
    let (tree, _) = checkpoint::create(&work, r, &agent(), None)
        .await
        .unwrap()
        .unwrap();
    assert!(
        checkpoint::create(&work, r, &agent(), Some(&tree))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn create_does_not_touch_index_or_branch() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "changed\n").unwrap();
    std::fs::write(work.join("u.txt"), "untracked\n").unwrap();
    let head = git(&work, &["rev-parse", "HEAD"]);
    let status = git(&work, &["status", "--porcelain"]);
    checkpoint::create(&work, remote.to_str().unwrap(), &agent(), None)
        .await
        .unwrap();
    assert_eq!(git(&work, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&work, &["status", "--porcelain"]), status);
}

#[tokio::test]
async fn restore_reproduces_files_on_new_root() {
    let (d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "edited\n").unwrap();
    std::fs::write(work.join("b.txt"), "untracked\n").unwrap();
    let r = remote.to_str().unwrap();
    let (_, sha) = checkpoint::create(&work, r, &agent(), None)
        .await
        .unwrap()
        .unwrap();
    let facts = checkpoint::detect(&work, None).await.unwrap();
    let root = d.path().join("other-host");
    let dir = checkpoint::restore(&root, &facts.info, &agent(), r, Some(&sha))
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
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    let status = git(&dir, &["status", "--porcelain"]);
    assert!(status.contains("M a.txt"), "{status}");
    assert!(status.contains("?? b.txt"), "{status}");
}

#[tokio::test]
async fn save_stale_pushes_side_ref() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "lost turn\n").unwrap();
    let sha = checkpoint::save_stale(&work, remote.to_str().unwrap(), &agent(), Timestamp(42))
        .await
        .unwrap();
    assert_eq!(git(&remote, &["rev-parse", "refs/borant/w1/stale/42"]), sha);
}

#[tokio::test]
async fn detect_reports_subdir_and_branch() {
    let (_d, _remote, work) = setup();
    std::fs::create_dir_all(work.join("crates/x")).unwrap();
    let facts = checkpoint::detect(&work.join("crates/x"), None)
        .await
        .unwrap();
    assert_eq!(facts.info.subdir, "crates/x");
    assert_eq!(facts.info.branch, "main");
    assert!(checkpoint::detect(Path::new("/"), None).await.is_none());
}
