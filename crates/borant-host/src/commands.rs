//! Commands from the hub: launch, prepare a move, fence, and stop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use borant_core::model::Harness;
use borant_core::ports::{LaunchSpec, Notice, Notifier, PortError, PortResult};
use borant_core::proto::{CtlRequest, HostCommand, LaunchOrder};
use borant_core::{AgentId, BundleName, SessionId, subjects};
use borant_launch::harness;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::secrets::remove_dir;
use crate::{Host, PendingLaunch, checkpoint};

impl Host {
    pub(crate) async fn command_loop(self, shutdown: CancellationToken) {
        let subject = subjects::host_cmd(&self.inner.cfg.name);
        loop {
            let mut cmds = match self.inner.bus.serve(&subject).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("cannot serve host commands yet: {e}");
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => continue,
                    }
                }
            };
            loop {
                let next = tokio::select! {
                    _ = shutdown.cancelled() => return,
                    n = cmds.next() => n,
                };
                let Some(incoming) = next else { break };
                match serde_json::from_slice::<HostCommand>(&incoming.payload) {
                    Ok(cmd) => {
                        let host = self.clone();
                        tokio::spawn(async move { host.handle_command(cmd).await });
                    }
                    Err(e) => tracing::warn!("bad host command: {e}"),
                }
            }
        }
    }

    /// Runs one host command. Public for tests.
    pub async fn handle_command(&self, cmd: HostCommand) {
        match cmd {
            HostCommand::Launch(order) => {
                let agent = order.agent.clone();
                if let Err(e) = self.launch(order).await {
                    tracing::warn!(%agent, "launch failed: {e:#}");
                    let req = CtlRequest::LaunchFailed {
                        agent: agent.clone(),
                        host: self.inner.cfg.name.clone(),
                        error: format!("{e:#}"),
                        // A launch that errors never waited a timeout.
                        timeout_secs: None,
                    };
                    if let Err(e) = self.hub(&req).await {
                        tracing::warn!(
                            %agent,
                            "cannot tell the hub about the failed launch: {}",
                            e.message
                        );
                    }
                }
            }
            HostCommand::PrepareMove { session, .. } => self.prepare_move(session).await,
            HostCommand::Fence { session, reason } => self.fence(session, reason, true).await,
            HostCommand::Stop { session } => {
                let info = self
                    .state()
                    .sessions
                    .get(&session)
                    .map(|s| (s.agent(), s.pid, s.launch.clone()));
                if let Some((agent, pid, launch)) = info {
                    self.stop_process(agent.as_ref(), pid, launch.as_ref())
                        .await;
                }
                self.end_session(session, "stopped by the hub").await;
            }
            HostCommand::Park { session } => self.park(session).await,
            HostCommand::DeleteWork { agent } => self.delete_work(&agent).await,
        }
    }

    /// Deletes the work dir of an agent that the hub retired, when no local
    /// session of the agent runs here. The clone is the biggest thing that an
    /// agent leaves behind, so this is what keeps a host's disk from filling.
    /// Public for tests.
    pub async fn delete_work(&self, agent: &AgentId) {
        let live = {
            let st = self.state();
            st.sessions.values().any(|s| {
                s.agent_hint.as_ref() == Some(agent)
                    || s.reg.as_ref().is_some_and(|r| &r.agent == agent)
            })
        };
        if live {
            tracing::info!(%agent, "keeping the work dir: a session of the agent runs here");
            return;
        }
        let dir = self.inner.cfg.home.join("work").join(agent.as_str());
        if !dir.is_dir() {
            return;
        }
        let freed = dir_size(&dir).await;
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => tracing::info!(
                %agent,
                freed_mb = freed / (1024 * 1024),
                dir = %dir.display(),
                "deleted the work dir of a retired agent"
            ),
            Err(e) => {
                tracing::warn!(%agent, dir = %dir.display(), "cannot delete the work dir: {e}")
            }
        }
    }

    /// Prepares the working directory and starts the harness. Public for tests.
    pub async fn launch(&self, o: LaunchOrder) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let _lock = self.launch_lock(&o.agent)?;
        let cfg = &self.inner.cfg;
        if !cfg.harnesses.contains(&o.harness) {
            anyhow::bail!(
                "harness {} is not available on host {}",
                o.harness,
                cfg.name
            );
        }
        // A program that a person removed after the registration must fail
        // here, before anything is prepared: the hub then hears why at once
        // instead of stopping a launch that could never register.
        let bin = harness::program(o.harness, &cfg.commands);
        if harness::find_program(&harness::search_dirs(), &bin).is_none() {
            anyhow::bail!(
                "the program of harness {} is not installed on host {}: {} is not on PATH or in ~/.local/bin",
                o.harness,
                cfg.name,
                bin
            );
        }
        // The values of the launch come before its clone: the git commands of
        // the restore run with them, so a collection bundle can carry the
        // credential of its repositories. A launch that needs bundles never
        // clones with the daemon's own credentials, and a launch whose values
        // do not resolve fails here, before anything is prepared.
        let issued = self.issue_secrets(&o).await?;
        let vars = issued
            .as_ref()
            .map_or(&[][..], |issued| issued.vars.pairs());
        let cwd = match self.prepare_dir(&o, vars, started).await {
            Ok(cwd) => cwd,
            Err(e) => {
                // No session will read the values of a launch that failed.
                if let Some(issued) = &issued {
                    remove_dir(&issued.dir).await;
                }
                return Err(e);
            }
        };
        let mut env = vec![
            ("BORANT_AGENT".to_owned(), o.agent.to_string()),
            (
                "BORANT_HOME".to_owned(),
                cfg.home.to_string_lossy().into_owned(),
            ),
            (
                "BORANT_SOCKET".to_owned(),
                cfg.socket().to_string_lossy().into_owned(),
            ),
        ];
        if let Some(c) = o.channels.first() {
            env.push(("BORANT_CHANNEL".to_owned(), c.to_string()));
        }
        if let Some(author) = &o.git_author {
            env.push(("GIT_AUTHOR_NAME".to_owned(), author.name.clone()));
            env.push(("GIT_AUTHOR_EMAIL".to_owned(), author.email.clone()));
            env.push(("GIT_COMMITTER_NAME".to_owned(), author.name.clone()));
            env.push(("GIT_COMMITTER_EMAIL".to_owned(), author.email.clone()));
        }
        // The names only, never a value: the defaults of the harness, then the
        // bundles of the collection and the role.
        if let Some(secrets) = &cfg.secrets {
            let names = |list: &[BundleName]| -> Vec<String> {
                list.iter().map(|b| b.to_string()).collect()
            };
            let defaults = secrets.bundles_for(o.harness);
            let bundles = secrets.launch_bundles(o.harness, &o.bundles);
            if !bundles.is_empty() {
                tracing::debug!(agent = %o.agent, harness = %o.harness, defaults = ?names(&defaults), bundles = ?names(&bundles), "the bundles of the launch");
            }
        }
        if o.harness == Harness::Fake {
            env.push(("BORANT_PROMPT".to_owned(), o.prompt.clone()));
        }
        let mut program = harness::command(
            o.harness,
            &o.prompt,
            o.model.as_deref(),
            &cfg.commands,
            !o.members.is_empty(),
        );
        // The config directory that the session reads, before the shim and the
        // sandbox wrap the program: the trust mark below goes into its file.
        let claude_config = borant_launch::trust::launch_config_dir(vars, &program);
        // Resolver mode: the values go into the agent's secret directory, and
        // only its path reaches the launch. The shim hands them to the harness.
        if let Some(issued) = &issued {
            program = match self.shim_program(&issued.dir, program) {
                Ok(program) => program,
                Err(e) => {
                    remove_dir(&issued.dir).await;
                    return Err(e.into());
                }
            };
            env.push((
                "BORANT_SECRETS_DIR".to_owned(),
                issued.dir.to_string_lossy().into_owned(),
            ));
            let mut st = self.state();
            st.redactors
                .insert(o.agent.clone(), issued.redactor.clone());
            st.launched.insert(o.agent.clone(), issued.clone());
        }
        // The sandbox wraps the shim, not the other way round: the shim and
        // the harness then run inside the same namespace.
        if let Some(plan) = self.inner.cfg.sandbox_plan(
            &o,
            &cwd,
            issued.as_ref().map(|issued| issued.dir.as_path()),
        ) {
            tracing::info!(agent = %o.agent, program = %plan.program, "the launch runs in a sandbox");
            program = borant_launch::sandbox::wrap(&plan, program);
        }
        // A session of this agent that is still around is stale: the hub
        // relaunches an agent only after its session ended or did not
        // register. tmux refuses a second session of the same name, so the
        // old one goes first: the handle of this daemon's last launch, and the
        // named session that a restart, a lost order, or an older attempt left
        // behind. The stop also sweeps the tools that either left: the same
        // sweep that a stop runs, so a fresh launch starts in a clean dir.
        let stale = self.state().last_launch.remove(&o.agent);
        self.stop_process(Some(&o.agent), None, stale.as_ref())
            .await;
        // Claude Code asks whether it trusts a directory that it does not
        // know, and a git repository root is never covered by the trust of its
        // parent. A launch that sits at that prompt never registers, so the
        // directory is marked trusted first, in the config directory that the
        // session reads. A failure still launches: the prompt is then the
        // visible failure.
        if o.harness == Harness::ClaudeCode
            && let Err(e) =
                borant_launch::trust::trust_launch_folder(&cwd, claude_config.as_deref())
        {
            tracing::warn!(
                agent = %o.agent,
                cwd = %cwd.display(),
                "cannot mark the launch directory as trusted for Claude Code: {e}"
            );
        }
        let spec = LaunchSpec {
            agent: o.agent.clone(),
            harness: o.harness,
            cwd: cwd.clone(),
            program,
            env,
        };
        let handle = match self.inner.launcher.launch(&spec).await {
            Ok(handle) => handle,
            Err(e) => {
                // The directory holds values that no session will read.
                self.forget_secrets(&o.agent).await;
                return Err(e.into());
            }
        };
        tracing::info!(agent = %o.agent, harness = %o.harness, cwd = %cwd.display(), ?o.reason, "launched");
        {
            let mut st = self.state();
            st.pending_launch.insert(
                o.agent.clone(),
                PendingLaunch {
                    handle: handle.clone(),
                    // The clock of the watchdog starts with the harness, not
                    // with the clone that prepared its directory: a large
                    // repository must not eat the time of the launch.
                    started: std::time::Instant::now(),
                },
            );
            st.last_launch.insert(o.agent.clone(), handle);
        }
        if let Some(issued) = &issued {
            // A launch that never registers must not keep its values until the
            // daemon stops: they go when the reservation of the launch ends.
            let host = self.clone();
            let agent = o.agent;
            let issued = issued.clone();
            let reserve = o.reserve_secs.map_or(DEFAULT_RESERVE, Duration::from_secs);
            tokio::spawn(async move {
                tokio::time::sleep(reserve).await;
                host.expire_launch_secrets(&agent, &issued).await;
            });
        }
        Ok(())
    }

    /// Prepares the working directory of a launch: the clone or the fetch of
    /// its project, its checkpoint remote, and the setup of the project. Each
    /// git command and the setup run with `vars`, the resolved bundles of the
    /// launch, so the host needs no git-specific configuration of its own.
    async fn prepare_dir(
        &self,
        o: &LaunchOrder,
        vars: &checkpoint::GitEnv,
        started: std::time::Instant,
    ) -> anyhow::Result<PathBuf> {
        let cfg = &self.inner.cfg;
        let work_root = cfg.home.join("work");
        let project = o.project.as_ref();
        let Some(repo) = &o.repo else {
            let d = work_root.join(o.agent.as_str());
            tokio::fs::create_dir_all(&d).await?;
            return Ok(d);
        };
        let project_remote = project.and_then(|p| p.checkpoint_remote.as_deref());
        let remote = match project_remote {
            Some(r) => r.to_owned(),
            None => cfg
                .checkpoint_remote
                .clone()
                .unwrap_or_else(|| repo.remote_url.clone()),
        };
        let base = project.map(|p| p.default_branch.as_str());
        let mirrors = cfg.home.join(checkpoint::MIRRORS_DIR);
        let mirror = project.map(|p| checkpoint::Mirror {
            root: mirrors.as_path(),
            name: &p.name,
        });
        // One deadline covers every git command and mirror-lock wait of the
        // preparation, so the commands together stay inside the reservation.
        let reserve = o.reserve_secs.map_or(DEFAULT_RESERVE, Duration::from_secs);
        let (git_deadline, setup_deadline) =
            prep_deadlines(tokio::time::Instant::now(), reserve, started.elapsed());
        // Setup preparation (its git directory, the marker) runs inside the
        // same scope, and the setup command gets what is left of it.
        let dir = checkpoint::with_deadline(git_deadline, async {
            let dir = checkpoint::restore(
                &work_root,
                repo,
                &o.agent,
                &remote,
                o.wip.as_deref(),
                o.wip_branch.as_deref(),
                base,
                mirror,
                vars,
            )
            .await?;
            if let Some(r) = project_remote {
                checkpoint::set_checkpoint_remote(&dir, r).await?;
            }
            if let Some(cmd) = project.and_then(|p| p.setup.as_deref()) {
                // The session must register before the hub reservation ends,
                // or the hub launches the agent again.
                run_setup(&dir, cmd, setup_deadline, vars).await?;
            }
            checkpoint::check_deadline()?;
            anyhow::Ok(dir)
        })
        .await?;
        Ok(dir)
    }

    /// Marks a launch of `agent` as running. Fails if one already runs, so
    /// two launches never share a working directory.
    fn launch_lock(&self, agent: &AgentId) -> anyhow::Result<LaunchLock> {
        if !self.state().launching.insert(agent.clone()) {
            anyhow::bail!(
                "a launch of agent {agent} already runs on host {}",
                self.inner.cfg.name
            );
        }
        Ok(LaunchLock {
            host: self.clone(),
            agent: agent.clone(),
        })
    }

    async fn prepare_move(&self, session: SessionId) {
        let idle = {
            let mut st = self.state();
            let Some(s) = st.sessions.get_mut(&session) else {
                tracing::warn!(%session, "prepare_move for an unknown session");
                return;
            };
            s.moving = true;
            !s.busy
        };
        if idle {
            self.finish_move(session).await;
        }
    }
}

/// Removes the launch mark of an agent when its launch ends.
struct LaunchLock {
    host: Host,
    agent: AgentId,
}

impl Drop for LaunchLock {
    fn drop(&mut self) {
        self.host.state().launching.remove(&self.agent);
    }
}

/// The hub reservation for a launch, if the order does not say.
const DEFAULT_RESERVE: Duration = Duration::from_secs(300);
/// The time that the harness keeps, after the setup, to start and register.
const SETUP_MARGIN: Duration = Duration::from_secs(60);
/// The deadlines of a preparation, from the reservation of the launch and
/// the time already spent: the git commands may use all of what is left of
/// the reservation, and the setup command stops `SETUP_MARGIN` earlier. The
/// margin must not shorten the git deadline: a short reservation (the e2e
/// hosts use 30s) would otherwise expire before the first command.
fn prep_deadlines(
    now: tokio::time::Instant,
    reserve: Duration,
    elapsed: Duration,
) -> (tokio::time::Instant, tokio::time::Instant) {
    let left = reserve.saturating_sub(elapsed);
    (now + left, now + left.saturating_sub(SETUP_MARGIN))
}

/// The most output of a failed setup command that goes into the error.
const SETUP_OUTPUT_CHARS: usize = 4000;

/// Runs the setup command of a project once in a clone, by `deadline`.
/// Runs inside the deadline scope of the preparation.
/// A marker in the git directory records a success, so a restore of the
/// same clone skips it, and a clone whose setup failed runs it again. The
/// output goes to a file in the git directory, so a process that the
/// command leaves in the background cannot hold the launch. `vars` are the
/// resolved bundles of the launch: the setup may fetch private dependencies.
async fn run_setup(
    dir: &Path,
    cmd: &str,
    deadline: tokio::time::Instant,
    vars: &checkpoint::GitEnv,
) -> anyhow::Result<()> {
    let git_dir = checkpoint::git_dir(dir).await?;
    let marker = git_dir.join("borant-setup-done");
    let hint = "raise BORANT_RESERVE_SECS on the hub for a longer setup";
    // The git deadline is fatal, cached or not. A valid marker needs no
    // setup command, so only an uncached setup needs budget before `deadline`.
    checkpoint::check_deadline()?;
    if marker.exists() {
        return Ok(());
    }
    let log_path = git_dir.join("borant-setup.log");
    let log = std::fs::File::create(&log_path).context("create the setup log")?;
    // The preparation above took time: check the setup deadline again now,
    // right before the spawn.
    let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
    if budget.is_zero() {
        anyhow::bail!("no time is left for setup `{cmd}` in the launch reservation; {hint}");
    }
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .envs(vars.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run setup `{cmd}`"))?;
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(s) => s?,
        Err(_) => {
            if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            let _ = child.wait().await;
            anyhow::bail!(
                "setup `{cmd}` timed out after {budget:?}; {hint}: {}",
                log_tail(&log_path)
            );
        }
    };
    if !status.success() {
        anyhow::bail!(
            "setup `{cmd}` failed with {status}: {}",
            log_tail(&log_path)
        );
    }
    tokio::fs::write(&marker, b"").await?;
    Ok(())
}

fn log_tail(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(SETUP_OUTPUT_CHARS);
    text.chars().skip(skip).collect()
}

/// Desktop notifications through `osascript` on macOS, and `notify-send`
/// elsewhere.
#[derive(Debug, Default)]
pub struct DesktopNotifier;

#[async_trait::async_trait]
impl Notifier for DesktopNotifier {
    async fn notify(&self, notice: &Notice) -> PortResult<()> {
        let title = &notice.title;
        let body = &notice.body;
        let status = if cfg!(target_os = "macos") {
            let script = format!(
                "display notification {} with title {}",
                apple_string(body),
                apple_string(title)
            );
            tokio::process::Command::new("osascript")
                .args(["-e", &script])
                .status()
                .await
        } else {
            tokio::process::Command::new("notify-send")
                .args([title, body])
                .status()
                .await
        };
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(PortError::other(format!("notifier exited with {s}"))),
            Err(e) => Err(PortError::other(e.to_string())),
        }
    }
}

/// The bytes of a directory tree, for the log of a deleted work dir. A tree
/// that shrinks or vanishes while we walk it counts what it counted.
async fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&next).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

fn apple_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod setup_deadline_tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(d.path())
            .args(["init", "--quiet"])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        d
    }

    #[test]
    fn a_short_reservation_keeps_its_git_deadline() {
        // A takeover or move with a 30s reservation (shorter than the setup
        // margin) still gives git the whole 30s; only setup gets nothing.
        let now = tokio::time::Instant::now();
        let (git, setup) = prep_deadlines(now, Duration::from_secs(30), Duration::from_secs(2));
        assert_eq!(git, now + Duration::from_secs(28));
        assert_eq!(setup, now);
        let (git, setup) = prep_deadlines(now, Duration::from_secs(300), Duration::ZERO);
        assert_eq!(git, now + Duration::from_secs(300));
        assert_eq!(setup, now + Duration::from_secs(240));
        // Spent past the reservation: both are expired.
        let (git, _) = prep_deadlines(now, Duration::from_secs(30), Duration::from_secs(40));
        assert_eq!(git, now);
    }

    #[tokio::test]
    async fn a_cached_marker_does_not_escape_the_deadline() {
        let d = init_repo();
        std::fs::write(d.path().join(".git/borant-setup-done"), b"").unwrap();
        let now = tokio::time::Instant::now();
        // Time left: the cached marker skips the setup.
        let ok = checkpoint::with_deadline(
            now + Duration::from_secs(30),
            run_setup(d.path(), "true", now + Duration::from_secs(30), &[]),
        )
        .await;
        assert!(ok.is_ok(), "{ok:?}");
        // No time left: the marker is no excuse.
        let r = checkpoint::with_deadline(now, run_setup(d.path(), "true", now, &[])).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn only_an_uncached_setup_needs_the_setup_budget() {
        let d = init_repo();
        let now = tokio::time::Instant::now();
        let g = now + Duration::from_secs(30);
        // Setup deadline expired, git deadline in the future.
        // Uncached: fails clearly and never runs.
        let r = checkpoint::with_deadline(g, run_setup(d.path(), "touch ran", now, &[])).await;
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("no time is left for setup"), "{e}");
        assert!(!d.path().join("ran").exists());
        // Cached: passes without running the command.
        std::fs::write(d.path().join(".git/borant-setup-done"), b"").unwrap();
        let r = checkpoint::with_deadline(g, run_setup(d.path(), "touch ran", now, &[])).await;
        assert!(r.is_ok(), "{r:?}");
        assert!(!d.path().join("ran").exists());
    }

    #[tokio::test]
    async fn setup_stops_at_the_absolute_deadline_and_reaps_the_group() {
        let d = init_repo();
        let s = tokio::time::Instant::now() + Duration::from_millis(600);
        let g = s + Duration::from_secs(30);
        let started = std::time::Instant::now();
        // A background child in the process group must die with the setup.
        let cmd = "sleep 30 & echo $! > bg.pid; wait";
        let r = checkpoint::with_deadline(g, run_setup(d.path(), cmd, s, &[])).await;
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("timed out"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!d.path().join(".git/borant-setup-done").exists());
        let pid: i32 = std::fs::read_to_string(d.path().join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
        assert!(!alive, "background child outlived the timeout");
    }

    #[tokio::test]
    async fn setup_git_dir_probe_shares_the_deadline() {
        let d = init_repo();
        let now = tokio::time::Instant::now();
        let started = std::time::Instant::now();
        // The deadline has passed: the git directory probe does not start
        // with a fresh limit, and the setup command never runs.
        let r = checkpoint::with_deadline(now, run_setup(d.path(), "touch ran", now, &[])).await;
        assert!(r.is_err());
        assert!(!d.path().join("ran").exists());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_setup_budget_is_what_is_left_of_the_deadline() {
        let d = init_repo();
        let dl = tokio::time::Instant::now() + Duration::from_millis(800);
        let started = std::time::Instant::now();
        let r = checkpoint::with_deadline(dl, run_setup(d.path(), "sleep 30", dl, &[])).await;
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
