//! Checkpoints of the working tree as git commits on custom refs (spec
//! section 12). The commands never change the branch, the real index, or
//! the files of the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use borant_core::model::RepoInfo;
use borant_core::{AgentId, ProjectName, Timestamp};
use tokio::process::Command;

/// The limit for one git command, including a push over the network.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub fn wip_ref(agent: &AgentId) -> String {
    format!("refs/borant/{agent}/wip")
}

pub fn stale_ref(agent: &AgentId, at: Timestamp) -> String {
    format!("refs/borant/{agent}/stale/{at}")
}

async fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> anyhow::Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("git {} timed out after {GIT_TIMEOUT:?}", args.join(" ")))?
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Facts about the git repository of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFacts {
    pub root: PathBuf,
    pub info: RepoInfo,
    /// The checkpoint remote of the project, from `set_checkpoint_remote`.
    pub checkpoint_remote: Option<String>,
}

/// The git config key that holds the checkpoint remote of a project clone.
const CHECKPOINT_REMOTE_KEY: &str = "borant.checkpointRemote";

/// The absolute git directory of the repository of `dir`.
pub async fn git_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(
        git(dir, &["rev-parse", "--absolute-git-dir"], None).await?,
    ))
}

/// Stores the checkpoint remote of a project in the clone, so `detect`
/// finds it again for each session, also after a daemon restart.
pub async fn set_checkpoint_remote(dir: &Path, remote: &str) -> anyhow::Result<()> {
    git(dir, &["config", CHECKPOINT_REMOTE_KEY, remote], None).await?;
    Ok(())
}

/// Finds the repository of `cwd`. The remote URL is the `origin` remote, or
/// `fallback_remote` if the repository has no `origin`. Returns `None`
/// outside a repository or without any remote.
pub async fn detect(cwd: &Path, fallback_remote: Option<&str>) -> Option<RepoFacts> {
    let root = PathBuf::from(
        git(cwd, &["rev-parse", "--show-toplevel"], None)
            .await
            .ok()?,
    );
    let remote = match git(cwd, &["remote", "get-url", "origin"], None).await {
        Ok(u) => u,
        Err(_) => fallback_remote?.to_owned(),
    };
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"], None)
        .await
        .unwrap_or_else(|_| "main".to_owned());
    let canon_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    let subdir = canon_cwd
        .strip_prefix(&canon_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let checkpoint_remote = git(cwd, &["config", "--get", CHECKPOINT_REMOTE_KEY], None)
        .await
        .ok()
        .filter(|r| !r.is_empty());
    Some(RepoFacts {
        root,
        info: RepoInfo {
            remote_url: remote,
            branch,
            subdir,
        },
        checkpoint_remote,
    })
}

/// Builds a commit of the whole working tree, including untracked files that
/// git does not ignore, with `HEAD` as the parent. Returns the tree and the
/// commit SHA.
async fn snapshot(cwd: &Path, message: &str) -> anyhow::Result<(String, String)> {
    let index_path = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        None,
    )
    .await?;
    let tmp = tempfile::NamedTempFile::new().context("create a temporary index")?;
    if Path::new(&index_path).exists() {
        std::fs::copy(&index_path, tmp.path()).context("copy the index")?;
    }
    git(cwd, &["add", "-A"], Some(tmp.path())).await?;
    let tree = git(cwd, &["write-tree"], Some(tmp.path())).await?;
    let head = git(cwd, &["rev-parse", "--verify", "-q", "HEAD"], None)
        .await
        .ok();
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(h) = &head {
        args.push("-p");
        args.push(h);
    }
    let sha = git(cwd, &args, None).await?;
    Ok((tree, sha))
}

/// Creates a checkpoint and pushes it to the WIP ref of the agent. Returns
/// a key for the tree and `HEAD`, and the commit SHA, or `None` if the key
/// equals `last`.
pub async fn create(
    cwd: &Path,
    remote: &str,
    agent: &AgentId,
    last_tree: Option<&str>,
) -> anyhow::Result<Option<(String, String)>> {
    let (tree, sha) = snapshot(cwd, &format!("borant checkpoint of {agent}")).await?;
    // A new commit with the same tree still needs a checkpoint, or a restore
    // returns to the old parent and loses the commit.
    let head = git(cwd, &["rev-parse", "--verify", "-q", "HEAD"], None)
        .await
        .unwrap_or_default();
    let key = format!("{tree} {head}");
    if last_tree == Some(key.as_str()) {
        return Ok(None);
    }
    let refspec = format!("{sha}:{}", wip_ref(agent));
    git(cwd, &["push", "--force", "--quiet", remote, &refspec], None).await?;
    Ok(Some((key, sha)))
}

/// Pushes the current tree to a stale ref, for the lost turn of a session
/// that lost its lease.
pub async fn save_stale(
    cwd: &Path,
    remote: &str,
    agent: &AgentId,
    at: Timestamp,
) -> anyhow::Result<String> {
    let (_, sha) = snapshot(cwd, &format!("borant stale work of {agent}")).await?;
    let refspec = format!("{sha}:{}", stale_ref(agent, at));
    git(cwd, &["push", "--force", "--quiet", remote, &refspec], None).await?;
    Ok(sha)
}

fn repo_dir_name(url: &str) -> String {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo");
    let name = last.trim_end_matches(".git");
    if name.is_empty() {
        "repo".into()
    } else {
        name.into()
    }
}

/// The folder under the home that holds the bare mirrors of the projects.
pub const MIRRORS_DIR: &str = "projects";

/// The config of a mirror. Nothing may prune, repack, or collect the objects
/// of a mirror, because the clones of the agents borrow them.
const MIRROR_CONFIG: [(&str, &str); 4] = [
    ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
    ("gc.auto", "0"),
    ("gc.pruneExpire", "never"),
    ("maintenance.auto", "false"),
];

/// The bare mirror of a project that a new clone on this host borrows its
/// objects from.
#[derive(Debug, Clone, Copy)]
pub struct Mirror<'a> {
    /// The folder of the mirrors of the host, `{home}/projects`.
    pub root: &'a Path,
    pub name: &'a ProjectName,
}

/// The directory of the bare mirror of `name` under `root`.
pub fn mirror_dir(root: &Path, name: &ProjectName) -> PathBuf {
    root.join(format!("{name}.git"))
}

/// The locks of the mirrors of this process, one per project.
static MIRROR_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The lock of one project, shared by every restore in this process.
fn project_lock(name: &ProjectName) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = MIRROR_LOCKS
        .lock()
        .expect("the mirror locks are not poisoned");
    locks.entry(name.as_str().to_owned()).or_default().clone()
}

/// Creates the mirror of `name` from `remote_url` if it does not exist, and
/// updates it from the remote. One lock per project covers the creation and
/// the fetch, so concurrent restores of one project share both. The host
/// never prunes, repacks, or deletes a mirror.
pub async fn ensure_mirror(
    root: &Path,
    name: &ProjectName,
    remote_url: &str,
) -> anyhow::Result<PathBuf> {
    let dir = mirror_dir(root, name);
    let lock = project_lock(name);
    let _guard = lock.lock().await;
    if dir.join("HEAD").exists() {
        git(&dir, &["fetch", "--quiet", "origin"], None)
            .await
            .with_context(|| format!("update the mirror of {name} in {}", dir.display()))?;
        return Ok(dir);
    }
    create_mirror(&dir, remote_url)
        .await
        .with_context(|| format!("create the mirror of {name} in {}", dir.display()))?;
    Ok(dir)
}

/// Clones `remote_url` bare into a temporary directory next to `dir`, sets
/// the config of the mirror, and moves it into place, so a failure leaves no
/// mirror.
async fn create_mirror(dir: &Path, remote_url: &str) -> anyhow::Result<()> {
    let parent = dir.parent().context("the mirror directory has no parent")?;
    tokio::fs::create_dir_all(parent).await?;
    let tmp = tempfile::Builder::new()
        .prefix(".mirror-")
        .tempdir_in(parent)
        .context("create a temporary mirror directory")?;
    let tmp_dir = tmp.path().join("repo.git");
    git(
        tmp.path(),
        &["clone", "--bare", "--quiet", remote_url, "repo.git"],
        None,
    )
    .await?;
    for (key, value) in MIRROR_CONFIG {
        git(&tmp_dir, &["config", key, value], None).await?;
    }
    tokio::fs::rename(&tmp_dir, dir)
        .await
        .context("move the new mirror into place")?;
    Ok(())
}

/// Prepares the working directory of an agent on this host: clones or
/// fetches the repository under `{work_root}/{agent}/{repo}`, checks out the
/// branch at the parent of the WIP commit, and restores the files of the WIP
/// commit without a new commit. Without a WIP commit, a branch that is not
/// on the remote and not local starts from `origin/{base}`, if `base` is set.
/// A restore of a project updates the bare mirror of the project first, then
/// a new clone borrows its objects; the `origin` of the clone stays the real
/// remote. Returns the working directory.
pub async fn restore(
    work_root: &Path,
    repo: &RepoInfo,
    agent: &AgentId,
    remote: &str,
    wip: Option<&str>,
    base: Option<&str>,
    mirror: Option<Mirror<'_>>,
) -> anyhow::Result<PathBuf> {
    let dir = work_root
        .join(agent.as_str())
        .join(repo_dir_name(&repo.remote_url));
    let reference = match mirror {
        Some(m) => Some(ensure_mirror(m.root, m.name, &repo.remote_url).await?),
        None => None,
    };
    if dir.join(".git").exists() {
        git(&dir, &["fetch", "--quiet", "origin"], None).await?;
    } else {
        tokio::fs::create_dir_all(dir.parent().unwrap_or(work_root)).await?;
        let mut cmd = Command::new("git");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.args(["clone", "--quiet"]);
        if let Some(r) = &reference {
            cmd.arg("--reference").arg(r);
        }
        let out = cmd.arg(&repo.remote_url).arg(&dir).output().await?;
        if !out.status.success() {
            bail!(
                "git clone {} failed: {}",
                repo.remote_url,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    let branch = repo.branch.as_str();
    match wip {
        Some(sha) => {
            git(&dir, &["fetch", "--quiet", remote, &wip_ref(agent)], None).await?;
            let parent = git(
                &dir,
                &["rev-parse", "--verify", "-q", &format!("{sha}^")],
                None,
            )
            .await
            .ok();
            match &parent {
                Some(p) => {
                    git(&dir, &["checkout", "--quiet", "-f", "-B", branch, p], None).await?;
                }
                None => {
                    git(
                        &dir,
                        &["checkout", "--quiet", "-f", "--orphan", branch],
                        None,
                    )
                    .await?;
                }
            }
            git(&dir, &["read-tree", "-u", "--reset", sha], None).await?;
            match &parent {
                Some(p) => git(&dir, &["reset", "--quiet", "--mixed", p], None).await?,
                None => git(&dir, &["rm", "-r", "--cached", "--quiet", "."], None).await?,
            };
        }
        None => {
            let remote_branch = format!("origin/{branch}");
            if git(&dir, &["rev-parse", "--verify", "-q", &remote_branch], None)
                .await
                .is_ok()
            {
                git(
                    &dir,
                    &["checkout", "--quiet", "-f", "-B", branch, &remote_branch],
                    None,
                )
                .await?;
            } else if let Some(base) = base {
                let local = format!("refs/heads/{branch}");
                if git(&dir, &["rev-parse", "--verify", "-q", &local], None)
                    .await
                    .is_ok()
                {
                    git(&dir, &["checkout", "--quiet", branch], None).await?;
                } else {
                    let from = format!("origin/{base}");
                    git(
                        &dir,
                        &["checkout", "--quiet", "-f", "-B", branch, &from],
                        None,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(if repo.subdir.is_empty() {
        dir
    } else {
        dir.join(&repo.subdir)
    })
}
