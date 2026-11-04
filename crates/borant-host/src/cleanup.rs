//! The rebuildable output that ended agents leave in their work trees: the
//! janitor removes it, so a host's disk does not fill with the `target` and
//! `node_modules` folders of agents that are gone (a Mac reached 95% and
//! could start no helper). A work tree is only touched when it has no
//! uncommitted change and no unpushed commit, and only in directories that
//! git ignores, so nothing that a rebuild cannot bring back ever goes.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use borant_core::rules::schedule::DEFAULT_DISK_STOP_PCT;
use borant_core::{AgentId, Timestamp};
use tokio::process::Command;

/// The limit of one git command of a pass.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The build output that an agent's checkout can rebuild, the default of
/// `[cleanup] dirs` in `host.toml`.
pub const DEFAULT_DIRS: [&str; 5] = ["target", "node_modules", ".next", "dist", ".turbo"];

/// The output that one pass removed from one agent's work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cleaned {
    pub agent: AgentId,
    pub dirs: Vec<PathBuf>,
    pub bytes: u64,
}

/// Why a work tree kept its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// The work tree has a change that no commit holds.
    Dirty,
    /// The work tree has a commit that no remote holds.
    Unpushed,
    /// git did not say whether the tree is safe, or the path left the work
    /// root: the pass keeps everything.
    Unknown,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dirty => "uncommitted changes",
            Self::Unpushed => "unpushed commits",
            Self::Unknown => "git did not answer",
        })
    }
}

/// One work tree that kept its output, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub agent: AgentId,
    pub repo: PathBuf,
    pub reason: SkipReason,
}

/// What one pass did.
#[derive(Debug, Default)]
pub struct Report {
    pub freed_bytes: u64,
    pub cleaned: Vec<Cleaned>,
    pub skipped: Vec<Skipped>,
}

/// One pass over a work root.
pub struct Pass<'a> {
    /// The folder that holds one work tree for each agent.
    pub root: &'a Path,
    /// The directory names to remove below a checkout.
    pub dirs: &'a [String],
    /// The grace after the end of the agent.
    pub min_age: Duration,
    /// Now, on the clock of the host.
    pub now: Timestamp,
    /// The agents that run, or wait to run, on this host: they are never
    /// touched.
    pub live: &'a (dyn Fn(&AgentId) -> bool + Sync),
    /// When the host saw the session of an agent end, which may be later than
    /// the last change of its tree.
    pub ended: &'a (dyn Fn(&AgentId) -> Option<Timestamp> + Sync),
}

impl fmt::Debug for Pass<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pass")
            .field("root", &self.root)
            .field("dirs", &self.dirs)
            .field("min_age", &self.min_age)
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

/// Removes the build output of every ended agent under `pass.root` whose work
/// tree is clean, pushed, and older than `pass.min_age`. A live agent, a dirty
/// tree, an unpushed commit, and a directory that git does not ignore keep
/// everything. Returns what it removed, for the log.
pub async fn sweep(pass: &Pass<'_>) -> Report {
    let mut report = Report::default();
    let Ok(entries) = std::fs::read_dir(pass.root) else {
        return report;
    };
    let canon_root = pass
        .root
        .canonicalize()
        .unwrap_or_else(|_| pass.root.to_owned());
    let mut agents: Vec<(AgentId, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(agent) = entry.file_name().to_string_lossy().parse::<AgentId>() else {
            continue;
        };
        if (pass.live)(&agent) {
            continue;
        }
        // A symlink in the work root could point anywhere: never follow it.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_symlink() || !meta.is_dir() {
            continue;
        }
        agents.push((agent, entry.path()));
    }
    // A stable order keeps the log and the tests readable.
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    for (agent, dir) in agents {
        let mut candidates = Vec::new();
        let newest = scan(&dir, pass.dirs, &mut candidates);
        let ended = match ((pass.ended)(&agent), newest) {
            (Some(at), Some(mtime)) => Timestamp(at.0.max(timestamp(mtime).0)),
            (Some(at), None) => at,
            (None, Some(mtime)) => timestamp(mtime),
            (None, None) => continue,
        };
        if pass.now.since(ended) < pass.min_age {
            continue;
        }
        let mut cleaned = Cleaned {
            agent: agent.clone(),
            dirs: Vec::new(),
            bytes: 0,
        };
        let mut states: HashMap<PathBuf, Option<SkipReason>> = HashMap::new();
        for candidate in candidates {
            // The path of a candidate is below the agent's folder by
            // construction, and neither the folder nor a build directory is
            // ever a symlink: no path outside the work root is touched.
            let Ok(real) = candidate.canonicalize() else {
                continue;
            };
            if !real.starts_with(&canon_root) {
                continue;
            }
            let Some(repo) = repo_root(&candidate).await else {
                continue;
            };
            let Ok(repo_real) = repo.canonicalize() else {
                continue;
            };
            if !repo_real.starts_with(&canon_root) {
                continue;
            }
            let state = match states.get(&repo_real) {
                Some(state) => *state,
                None => {
                    let state = worktree_state(&repo).await;
                    states.insert(repo_real.clone(), state);
                    state
                }
            };
            if let Some(reason) = state {
                report.skipped.push(Skipped {
                    agent: agent.clone(),
                    repo: repo_real,
                    reason,
                });
                continue;
            }
            // git speaks of the paths inside the repository, and a path that
            // the repository does not hold is not its output.
            let Ok(rel) = real.strip_prefix(&repo_real) else {
                continue;
            };
            if !is_output(&repo_real, rel).await {
                continue;
            }
            let bytes = bytes(&candidate);
            if tokio::fs::remove_dir_all(&candidate).await.is_ok() {
                cleaned.bytes = cleaned.bytes.saturating_add(bytes);
                cleaned.dirs.push(candidate);
            }
        }
        if !cleaned.dirs.is_empty() {
            report.freed_bytes = report.freed_bytes.saturating_add(cleaned.bytes);
            report.cleaned.push(cleaned);
        }
    }
    report
}

/// The largest build output under the work root, one entry for each, with its
/// size in bytes. The disk guard logs it when the disk is nearly full. Nothing
/// outside the work root is measured, and no file is read.
pub fn consumers(root: &Path, dirs: &[String], limit: usize) -> Vec<(PathBuf, u64)> {
    let mut found: Vec<(PathBuf, u64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_symlink() || !meta.is_dir() {
            continue;
        }
        let mut candidates = Vec::new();
        scan(&entry.path(), dirs, &mut candidates);
        for candidate in candidates {
            found.push((candidate.clone(), bytes(&candidate)));
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found.truncate(limit);
    found
}

/// The guard's decision at `used` percent of the disk: a pass with a zero age
/// at the stop line and at the warn line, nothing below it. An unknown disk
/// passes, as every rule that reads the disk does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    Idle,
    Early,
    Emergency,
}

pub fn guard_action(used: Option<f64>, warn_pct: u8) -> Guard {
    match used {
        Some(used) if used >= f64::from(DEFAULT_DISK_STOP_PCT) => Guard::Emergency,
        Some(used) if used >= f64::from(warn_pct) => Guard::Early,
        _ => Guard::Idle,
    }
}

/// The newest change below `dir` and the build output that it holds. The walk
/// never follows a symlink and never enters a build directory: the contents
/// of a `target` are what goes, so their times do not matter.
fn scan(dir: &Path, dirs: &[String], found: &mut Vec<PathBuf>) -> Option<SystemTime> {
    let mut newest = std::fs::symlink_metadata(dir)
        .ok()
        .and_then(|m| m.modified().ok());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return newest;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        newest = newest.max(meta.modified().ok());
        if meta.is_symlink() || !meta.is_dir() || entry.file_name() == OsStr::new(".git") {
            continue;
        }
        if is_build_dir(&entry.file_name(), dirs) {
            found.push(path);
            continue;
        }
        newest = newest.max(scan(&path, dirs, found));
    }
    newest
}

fn is_build_dir(name: &OsStr, dirs: &[String]) -> bool {
    dirs.iter().any(|d| OsStr::new(d) == name)
}

fn timestamp(at: SystemTime) -> Timestamp {
    let ms = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Timestamp(i64::try_from(ms).unwrap_or(i64::MAX))
}

/// The bytes of a tree, without following a symlink.
fn bytes(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                // A symlink has the length of its target name, never the
                // size of what it points at.
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

async fn git(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.kill_on_drop(true);
    tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()
}

/// The root of the repository that holds `dir`.
async fn repo_root(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"]).await?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// `None` when the tree is clean and every commit of it is on a remote, so
/// only the build output can go; otherwise why it keeps everything.
async fn worktree_state(repo: &Path) -> Option<SkipReason> {
    let Some(status) = git(repo, &["status", "--porcelain"]).await else {
        return Some(SkipReason::Unknown);
    };
    if !status.status.success() {
        return Some(SkipReason::Unknown);
    }
    if !status.stdout.iter().all(u8::is_ascii_whitespace) {
        return Some(SkipReason::Dirty);
    }
    // The clone of an agent has a branch and an upstream from its remote, so
    // a commit that no remote holds is the one that a push would lose. An
    // empty repository has no commit to lose.
    let head = git(repo, &["rev-parse", "--verify", "-q", "HEAD"]).await;
    let head = head.filter(|o| o.status.success())?;
    if head.stdout.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let Some(count) = git(repo, &["rev-list", "--count", "HEAD", "--not", "--remotes"]).await
    else {
        return Some(SkipReason::Unknown);
    };
    if !count.status.success() {
        return Some(SkipReason::Unknown);
    }
    let count: u64 = String::from_utf8_lossy(&count.stdout)
        .trim()
        .parse()
        .unwrap_or(1);
    (count > 0).then_some(SkipReason::Unpushed)
}

/// Whether `rel` is output that no commit holds: git ignores the path and no
/// tracked file is below it. Removing it cannot change the tree or lose a
/// file that a person wrote.
async fn is_output(repo: &Path, rel: &Path) -> bool {
    let Some(tracked) = git_path(repo, &["ls-files", "--"], rel).await else {
        return false;
    };
    if !tracked.is_empty() {
        return false;
    }
    git_ok(repo, &["check-ignore", "-q", "--"], rel).await
}

/// The stdout of a git command of `repo` about `rel`, when it succeeds.
async fn git_path(repo: &Path, args: &[&str], rel: &Path) -> Option<String> {
    let out = run_git(Command::new("git").arg("-C").arg(repo).args(args).arg(rel)).await?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a git command of `repo` about `rel` succeeds, the answer of a `-q`
/// check.
async fn git_ok(repo: &Path, args: &[&str], rel: &Path) -> bool {
    run_git(Command::new("git").arg("-C").arg(repo).args(args).arg(rel))
        .await
        .is_some()
}

async fn run_git(cmd: &mut Command) -> Option<std::process::Output> {
    cmd.env("GIT_TERMINAL_PROMPT", "0").kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status.success().then_some(out)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use borant_core::{AgentId, Clock, SystemClock, Timestamp};

    use super::*;

    fn dirs() -> Vec<String> {
        DEFAULT_DIRS.iter().map(|s| (*s).to_owned()).collect()
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn build_output(dir: &Path) {
        for name in ["target", "node_modules"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("blob"), vec![7u8; 4096]).unwrap();
        }
    }

    /// A work tree of `agent`: a checkout whose output is ignored, pushed to a
    /// bare remote, so the pass may remove the output.
    fn repo(root: &Path, agent: &str) -> PathBuf {
        let remote = root.join("remotes").join(format!("{agent}.git"));
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "--quiet"]);
        let dir = root.join("work").join(agent).join("clone");
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet", "-b", "main"]);
        std::fs::write(
            dir.join(".gitignore"),
            "target/\nnode_modules/\n.next/\ndist/\n.turbo/\n",
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "-c",
                "user.email=test@localhost",
                "-c",
                "user.name=Test",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
        git(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&dir, &["push", "--quiet", "-u", "origin", "main"]);
        build_output(&dir);
        dir
    }

    /// A clock well after the files that a test just wrote, so a tree that a
    /// test does not backdate is already older than the grace.
    fn later() -> Timestamp {
        SystemClock.now().plus(Duration::from_secs(10 * 3600))
    }

    /// The work root of the host, where `repo` puts its checkouts.
    fn work(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join("work")
    }

    async fn run(
        root: &Path,
        min_age: Duration,
        now: Timestamp,
        live: bool,
        ended: &(dyn Fn(&AgentId) -> Option<Timestamp> + Sync),
    ) -> Report {
        let dirs = dirs();
        let live = move |_: &AgentId| live;
        let pass = Pass {
            root,
            dirs: &dirs,
            min_age,
            now,
            live: &live,
            ended,
        };
        super::sweep(&pass).await
    }

    #[tokio::test]
    async fn cleanup_removes_the_output_of_a_clean_ended_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            later(),
            false,
            &|_| None,
        )
        .await;
        assert!(!dir.join("target").exists(), "the target is gone");
        assert!(!dir.join("node_modules").exists(), "node_modules is gone");
        assert!(dir.join("README.md").exists(), "the checkout stays");
        assert_eq!(report.freed_bytes, 8192);
        assert_eq!(report.cleaned.len(), 1);
        assert_eq!(report.cleaned[0].agent, "w1".parse::<AgentId>().unwrap());
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    }

    #[tokio::test]
    async fn cleanup_keeps_the_output_of_a_dirty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        std::fs::write(dir.join("README.md"), "changed\n").unwrap();
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            later(),
            false,
            &|_| None,
        )
        .await;
        assert!(
            dir.join("target").exists(),
            "an uncommitted change keeps it"
        );
        assert!(dir.join("node_modules").exists());
        assert_eq!(report.skipped[0].reason, SkipReason::Dirty);
        assert_eq!(report.freed_bytes, 0);
    }

    #[tokio::test]
    async fn cleanup_keeps_the_output_of_an_unpushed_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        std::fs::write(dir.join("work.txt"), "not on a remote\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "-c",
                "user.email=test@localhost",
                "-c",
                "user.name=Test",
                "commit",
                "--quiet",
                "-m",
                "local",
            ],
        );
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            later(),
            false,
            &|_| None,
        )
        .await;
        assert!(dir.join("target").exists(), "an unpushed commit keeps it");
        assert_eq!(report.skipped[0].reason, SkipReason::Unpushed);
    }

    #[tokio::test]
    async fn cleanup_keeps_the_output_of_a_live_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        let report = run(&work(&tmp), Duration::ZERO, later(), true, &|_| None).await;
        assert!(dir.join("target").exists(), "a live agent keeps it");
        assert!(report.cleaned.is_empty());
        assert_eq!(report.freed_bytes, 0);
    }

    #[tokio::test]
    async fn cleanup_respects_the_min_age_on_a_manual_clock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        let base = later();
        // The session ended an hour before the pass, and the tree is older.
        let ended_at = base.minus(Duration::from_secs(3600));
        let ended = move |_: &AgentId| Some(ended_at);
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            base,
            false,
            &ended,
        )
        .await;
        assert!(dir.join("target").exists(), "inside the grace it stays");
        assert!(report.cleaned.is_empty());
        // Seven hours later the same tree is past the grace.
        let report = run(
            tmp.path(),
            Duration::from_secs(6 * 3600),
            base.plus(Duration::from_secs(7 * 3600)),
            false,
            &ended,
        )
        .await;
        assert!(!dir.join("target").exists(), "past the grace it goes");
        assert_eq!(report.cleaned.len(), 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_a_build_dir_that_git_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("dist/app.js"), "console.log(1)\n").unwrap();
        git(&dir, &["add", "-f", "dist/app.js"]);
        git(
            &dir,
            &[
                "-c",
                "user.email=test@localhost",
                "-c",
                "user.name=Test",
                "commit",
                "--quiet",
                "-m",
                "app",
            ],
        );
        git(&dir, &["push", "--quiet", "origin", "main"]);
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            later(),
            false,
            &|_| None,
        )
        .await;
        assert!(!dir.join("target").exists(), "the ignored output goes");
        assert!(dir.join("dist/app.js").exists(), "a tracked dir stays");
        // `target` and `node_modules`; the tracked `dist` keeps its file.
        assert_eq!(report.freed_bytes, 8192);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_does_not_follow_a_symlink_inside_the_output() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = repo(tmp.path(), "w1");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), vec![0u8; 64 * 1024]).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("target/link")).unwrap();
        let report = run(
            &work(&tmp),
            Duration::from_secs(6 * 3600),
            later(),
            false,
            &|_| None,
        )
        .await;
        assert!(!dir.join("target").exists());
        assert!(
            outside.path().join("keep.txt").exists(),
            "the symlink target stays"
        );
        // The two blobs, and the link as a name: never the 64 KiB it points at.
        assert_eq!(
            report.freed_bytes,
            8192 + outside.path().as_os_str().len() as u64
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_never_touches_a_path_outside_the_work_root() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("target")).unwrap();
        std::fs::write(outside.path().join("target/keep"), b"outside\n").unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(outside.path(), work.join("w1")).unwrap();
        let report = run(&work, Duration::ZERO, later(), false, &|_| None).await;
        assert!(
            outside.path().join("target/keep").exists(),
            "nothing outside goes"
        );
        assert!(work.join("w1").is_symlink(), "the link itself stays");
        assert!(report.cleaned.is_empty());
    }

    #[test]
    fn cleanup_guard_lines_follow_the_disk_checks() {
        assert_eq!(guard_action(None, 85), Guard::Idle);
        assert_eq!(guard_action(Some(10.0), 85), Guard::Idle);
        assert_eq!(guard_action(Some(84.9), 85), Guard::Idle);
        assert_eq!(guard_action(Some(85.0), 85), Guard::Early);
        assert_eq!(guard_action(Some(94.9), 85), Guard::Early);
        assert_eq!(guard_action(Some(95.0), 85), Guard::Emergency);
        assert_eq!(guard_action(Some(96.0), 90), Guard::Emergency);
    }

    #[tokio::test]
    async fn cleanup_consumers_list_the_largest_output() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo(tmp.path(), "w1");
        let b = repo(tmp.path(), "w2");
        std::fs::write(a.join("target/blob"), vec![0u8; 9000]).unwrap();
        std::fs::write(b.join("node_modules/blob"), vec![0u8; 2000]).unwrap();
        let found = consumers(&work(&tmp), &dirs(), 10);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found[0].1 > found[1].1, "the largest first: {found:?}");
        assert_eq!(found[3].1, 2000, "the smallest last: {found:?}");
    }
}
