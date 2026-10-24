//! Commands from the hub: launch, prepare a move, fence, and stop.

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

    async fn launch(&self, o: LaunchOrder) -> anyhow::Result<()> {
        let cfg = &self.inner.cfg;
        if !cfg.harnesses.contains(&o.harness) {
            anyhow::bail!(
                "harness {} is not available on host {}",
                o.harness,
                cfg.name
            );
        }
        let work_root = cfg.home.join("work");
        let cwd = match &o.repo {
            Some(repo) => {
                let remote = self.remote_for(&repo.remote_url);
                checkpoint::restore(&work_root, repo, &o.agent, &remote, o.wip.as_deref()).await?
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
            program: harness::command(o.harness, &o.prompt, &cfg.commands),
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
