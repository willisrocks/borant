//! Commands from the hub: launch, prepare a move, fence, and stop.

use std::path::Path;
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

use crate::{Host, checkpoint};

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
                    .map(|s| (s.pid, s.launch.clone()));
                if let Some((pid, launch)) = info {
                    self.stop_process(pid, launch.as_ref()).await;
                }
                self.end_session(session, "stopped by the hub").await;
            }
            HostCommand::Park { session } => self.park(session).await,
        }
    }

    /// Prepares the working directory and starts the harness. Public for tests.
    pub async fn launch(&self, o: LaunchOrder) -> anyhow::Result<()> {
        let started = tokio::time::Instant::now();
        let _lock = self.launch_lock(&o.agent)?;
        let cfg = &self.inner.cfg;
        if !cfg.harnesses.contains(&o.harness) {
            anyhow::bail!(
                "harness {} is not available on host {}",
                o.harness,
                cfg.name
            );
        }
        let work_root = cfg.home.join("work");
        let project = o.project.as_ref();
        let cwd = match &o.repo {
            Some(repo) => {
                let project_remote = project.and_then(|p| p.checkpoint_remote.as_deref());
                let remote = match project_remote {
                    Some(r) => r.to_owned(),
                    None => self
                        .inner
                        .cfg
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
                let dir = checkpoint::restore(
                    &work_root,
                    repo,
                    &o.agent,
                    &remote,
                    o.wip.as_deref(),
                    base,
                    mirror,
                )
                .await?;
                if let Some(r) = project_remote {
                    checkpoint::set_checkpoint_remote(&dir, r).await?;
                }
                if let Some(cmd) = project.and_then(|p| p.setup.as_deref()) {
                    // The session must register before the hub reservation
                    // ends, or the hub launches the agent again.
                    let reserve = o.reserve_secs.map_or(DEFAULT_RESERVE, Duration::from_secs);
                    let budget = reserve
                        .saturating_sub(SETUP_MARGIN)
                        .saturating_sub(started.elapsed());
                    run_setup(&dir, cmd, budget).await?;
                }
                dir
            }
            None => {
                let d = work_root.join(o.agent.as_str());
                tokio::fs::create_dir_all(&d).await?;
                d
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
        let mut program = harness::command(o.harness, &o.prompt, o.model.as_deref(), &cfg.commands);
        // Resolver mode: the values go into the agent's secret directory, and
        // only its path reaches the launch. The shim hands them to the harness.
        let issued = self.issue_secrets(&o).await?;
        if let Some(issued) = &issued {
            program = self.shim_program(&issued.dir, program)?;
            env.push((
                "BORANT_SECRETS_DIR".to_owned(),
                issued.dir.to_string_lossy().into_owned(),
            ));
            let mut st = self.state();
            st.redactors
                .insert(o.agent.clone(), issued.redactor.clone());
            st.launched.insert(o.agent.clone(), issued.clone());
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
                self.forget_secrets(&o.agent, None).await;
                return Err(e.into());
            }
        };
        tracing::info!(agent = %o.agent, harness = %o.harness, cwd = %cwd.display(), ?o.reason, "launched");
        self.state().pending_launch.insert(o.agent, handle);
        Ok(())
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
/// The most output of a failed setup command that goes into the error.
const SETUP_OUTPUT_CHARS: usize = 4000;

/// Runs the setup command of a project once in a clone, within `budget`.
/// A marker in the git directory records a success, so a restore of the
/// same clone skips it, and a clone whose setup failed runs it again. The
/// output goes to a file in the git directory, so a process that the
/// command leaves in the background cannot hold the launch.
async fn run_setup(dir: &Path, cmd: &str, budget: Duration) -> anyhow::Result<()> {
    let git_dir = checkpoint::git_dir(dir).await?;
    let marker = git_dir.join("borant-setup-done");
    if marker.exists() {
        return Ok(());
    }
    let hint = "raise BORANT_RESERVE_SECS on the hub for a longer setup";
    if budget.is_zero() {
        anyhow::bail!("no time is left for setup `{cmd}` in the launch reservation; {hint}");
    }
    let log_path = git_dir.join("borant-setup.log");
    let log = std::fs::File::create(&log_path).context("create the setup log")?;
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run setup `{cmd}`"))?;
    let status = match tokio::time::timeout(budget, child.wait()).await {
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

fn apple_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
