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
    let (_, sha) = checkpoint::create(&work, remote.to_str().unwrap(), &agent(), None, &[])
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
    let (tree, sha) = checkpoint::create(&work, r, &agent(), None, &[])
        .await
        .unwrap()
        .unwrap();
    // The edit has the size of the old file and lands in the second of the
    // index entry, so a snapshot that trusts stale stat data commits the old
    // blob instead.
    assert_eq!(git(&remote, &["show", &format!("{sha}:a.txt")]), "two");
    let again = checkpoint::create(&work, r, &agent(), Some(&tree), &[])
        .await
        .unwrap();
    assert!(
        again.is_none(),
        "first key {tree}, second {again:?}, status {}",
        git(&work, &["status", "--porcelain", "--ignored"])
    );
}

#[tokio::test]
async fn create_does_not_touch_index_or_branch() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "changed\n").unwrap();
    std::fs::write(work.join("u.txt"), "untracked\n").unwrap();
    let head = git(&work, &["rev-parse", "HEAD"]);
    let status = git(&work, &["status", "--porcelain"]);
    checkpoint::create(&work, remote.to_str().unwrap(), &agent(), None, &[])
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
    let (_, sha) = checkpoint::create(&work, r, &agent(), None, &[])
        .await
        .unwrap()
        .unwrap();
    let facts = checkpoint::detect(&work, None).await.unwrap();
    let root = d.path().join("other-host");
    let dir = checkpoint::restore(
        &root,
        &facts.info,
        &agent(),
        r,
        Some(&sha),
        None,
        None,
        None,
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
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    let status = git(&dir, &["status", "--porcelain"]);
    assert!(status.contains("M a.txt"), "{status}");
    assert!(status.contains("?? b.txt"), "{status}");
}

#[tokio::test]
async fn current_branch_reports_the_checkout() {
    let (_d, _remote, work) = setup();
    assert_eq!(
        checkpoint::current_branch(&work).await.as_deref(),
        Some("main")
    );
    git(&work, &["checkout", "-q", "--detach"]);
    assert_eq!(
        checkpoint::current_branch(&work).await,
        None,
        "a detached HEAD names no branch"
    );
}

/// A restore over a dirty tree on another branch: the work goes to a stale
/// ref first, and the tree ends on the branch of the checkpoint with its
/// files.
#[tokio::test]
async fn restore_saves_a_dirty_tree_and_keeps_the_checkpoint_branch() {
    let (d, remote, work) = setup();
    let r = remote.to_str().unwrap();
    // The repo of the launch is on main, as the spawn left it.
    let facts = checkpoint::detect(&work, None).await.unwrap();
    git(&work, &["checkout", "-q", "-b", "feat/x"]);
    std::fs::write(work.join("a.txt"), "feature\n").unwrap();
    git(&work, &["commit", "-q", "-am", "feature"]);
    let (_, sha) = checkpoint::create(&work, r, &agent(), None, &[])
        .await
        .unwrap()
        .unwrap();

    let root = d.path().join("other-host");
    let dir = checkpoint::restore(&root, &facts.info, &agent(), r, None, None, None, None, &[])
        .await
        .unwrap();
    // Work that no checkpoint took sits in the agent dir on main.
    std::fs::write(dir.join("a.txt"), "scratch that must survive\n").unwrap();

    checkpoint::restore(
        &root,
        &facts.info,
        &agent(),
        r,
        Some(&sha),
        Some("feat/x"),
        None,
        None,
        &[],
    )
    .await
    .unwrap();

    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "feat/x");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "feature\n",
        "the wip tree lands on the checkpoint branch"
    );
    let stale: Vec<String> = git(
        &remote,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/borant/w1/stale",
        ],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        stale.len(),
        1,
        "the dirty tree went to one stale ref: {stale:?}"
    );
    assert_eq!(
        git(&remote, &["show", &format!("{}:a.txt", stale[0])]),
        "scratch that must survive"
    );
}

/// A worktree that a crash left behind still holds its branch. The restore
/// prunes it, so the checkout of the checkpoint branch works.
#[tokio::test]
async fn restore_prunes_a_stale_worktree_that_holds_the_branch() {
    let (d, remote, work) = setup();
    let r = remote.to_str().unwrap();
    let facts = checkpoint::detect(&work, None).await.unwrap();
    git(&work, &["checkout", "-q", "-b", "feat/x"]);
    std::fs::write(work.join("a.txt"), "feature\n").unwrap();
    git(&work, &["commit", "-q", "-am", "feature"]);
    git(&work, &["push", "-q", "origin", "feat/x"]);
    let (_, sha) = checkpoint::create(&work, r, &agent(), None, &[])
        .await
        .unwrap()
        .unwrap();

    let root = d.path().join("other-host");
    let dir = checkpoint::restore(&root, &facts.info, &agent(), r, None, None, None, None, &[])
        .await
        .unwrap();
    git(&dir, &["fetch", "-q", "origin"]);
    git(&dir, &["branch", "feat/x", "origin/feat/x"]);
    let linked = d.path().join("linked");
    git(
        &dir,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "feat/x"],
    );
    // The directory is gone, but git still holds `feat/x` for the worktree.
    std::fs::remove_dir_all(&linked).unwrap();

    checkpoint::restore(
        &root,
        &facts.info,
        &agent(),
        r,
        Some(&sha),
        Some("feat/x"),
        None,
        None,
        &[],
    )
    .await
    .unwrap();

    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "feat/x");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "feature\n"
    );
}

#[tokio::test]
async fn save_stale_pushes_side_ref() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "lost turn\n").unwrap();
    let sha = checkpoint::save_stale(
        &work,
        remote.to_str().unwrap(),
        &agent(),
        Timestamp(42),
        &[],
    )
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

#[tokio::test]
async fn create_checkpoints_a_new_commit_with_the_same_tree() {
    let (_d, remote, work) = setup();
    std::fs::write(work.join("a.txt"), "two\n").unwrap();
    let r = remote.to_str().unwrap();
    let (key, _) = checkpoint::create(&work, r, &agent(), None, &[])
        .await
        .unwrap()
        .unwrap();
    git(&work, &["commit", "-q", "-am", "two"]);
    let (_, sha) = checkpoint::create(&work, r, &agent(), Some(&key), &[])
        .await
        .unwrap()
        .expect("a commit changes HEAD, so a checkpoint is due");
    let parent = git(&remote, &["rev-parse", &format!("{sha}^")]);
    assert_eq!(parent, git(&work, &["rev-parse", "HEAD"]));
}

/// A wip commit with no parent (the checkpoint of a tree with no commits)
/// restores on a branch that the clone already has: `checkout --orphan` would
/// fail with "a branch named 'main' already exists".
#[tokio::test]
async fn restore_of_a_parentless_wip_over_an_existing_branch() {
    let (d, remote, work) = setup();
    let r = remote.to_str().unwrap();
    // A root commit with its own tree, pushed as the wip ref.
    git(&work, &["checkout", "-q", "--orphan", "tmp"]);
    git(&work, &["rm", "-rf", "-q", "."]);
    std::fs::write(work.join("only.txt"), "wip\n").unwrap();
    git(&work, &["add", "only.txt"]);
    git(&work, &["commit", "-q", "-m", "wip"]);
    let sha = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &["push", "-q", "origin", &format!("{sha}:refs/borant/w1/wip")],
    );
    git(&work, &["checkout", "-q", "main"]);
    let facts = checkpoint::detect(&work, None).await.unwrap();
    let dir = checkpoint::restore(
        &d.path().join("other-host"),
        &facts.info,
        &agent(),
        r,
        Some(&sha),
        None,
        None,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("only.txt")).unwrap(),
        "wip\n"
    );
    assert!(!dir.join("a.txt").exists());
    assert_eq!(git(&dir, &["symbolic-ref", "--short", "HEAD"]), "main");
}
