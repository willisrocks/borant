//! Commands from the hub: launch, prepare a move, fence, and stop.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use borant_core::model::Harness;
use borant_core::ports::{LaunchSpec, Notifier, PortError, PortResult};
use borant_core::proto::{CtlRequest, HostCommand, LaunchOrder};
use borant_core::{SessionId, subjects};
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
                        agent,
                        host: self.inner.cfg.name.clone(),
                        error: format!("{e:#}"),
                    };
                    let _ = self.hub(&req).await;
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
        }
    }

    /// Prepares the working directory and starts the harness. Public for tests.
    pub async fn launch(&self, o: LaunchOrder) -> anyhow::Result<()> {
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
                let dir =
                    checkpoint::restore(&work_root, repo, &o.agent, &remote, o.wip.as_deref())
                        .await?;
                if let Some(r) = project_remote {
                    checkpoint::set_checkpoint_remote(&dir, r).await?;
                }
                if let Some(cmd) = project.and_then(|p| p.setup.as_deref()) {
                    run_setup(&dir, cmd).await?;
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
        if o.harness == Harness::Fake {
            env.push(("BORANT_PROMPT".to_owned(), o.prompt.clone()));
        }
        let spec = LaunchSpec {
            agent: o.agent.clone(),
            harness: o.harness,
            cwd: cwd.clone(),
            program: harness::command(o.harness, &o.prompt, o.model.as_deref(), &cfg.commands),
            env,
        };
        let handle = self.inner.launcher.launch(&spec).await?;
        tracing::info!(agent = %o.agent, harness = %o.harness, cwd = %cwd.display(), ?o.reason, "launched");
        self.state().pending_launch.insert(o.agent, handle);
        Ok(())
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

const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
/// The most output of a failed setup command that goes into the error.
const SETUP_OUTPUT_CHARS: usize = 4000;

/// Runs the setup command of a project once in a clone. A marker in the git
/// directory records a success, so a restore of the same clone skips it, and
/// a clone whose setup failed runs it again.
async fn run_setup(dir: &Path, cmd: &str) -> anyhow::Result<()> {
    let marker = dir.join(".git").join("borant-setup-done");
    if marker.exists() {
        return Ok(());
    }
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(SETUP_TIMEOUT, c.output())
        .await
        .map_err(|_| anyhow::anyhow!("setup `{cmd}` timed out after {SETUP_TIMEOUT:?}"))?
        .with_context(|| format!("run setup `{cmd}`"))?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let text = text.trim();
        let skip = text.chars().count().saturating_sub(SETUP_OUTPUT_CHARS);
        let tail: String = text.chars().skip(skip).collect();
        anyhow::bail!("setup `{cmd}` failed with {}: {tail}", out.status);
    }
    tokio::fs::write(&marker, b"").await?;
    Ok(())
}

/// Desktop notifications through `osascript` on macOS, and `notify-send`
/// elsewhere.
#[derive(Debug, Default)]
pub struct DesktopNotifier;

#[async_trait::async_trait]
impl Notifier for DesktopNotifier {
    async fn notify(&self, title: &str, body: &str) -> PortResult<()> {
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
