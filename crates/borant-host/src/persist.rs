//! The local session table on disk, so a restart of the host daemon keeps
//! its live sessions. The file is `$BORANT_HOME/sessions.json`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use borant_core::model::Harness;
use borant_core::ports::LaunchHandle;
use borant_core::proto::{ApiError, Registered};
use borant_core::{AgentId, ChannelName, CollectionName, RoleName, SessionId};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{Host, LocalSess, checkpoint, pid_alive};

#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    id: SessionId,
    harness: Harness,
    harness_session_id: Option<String>,
    pid: Option<u32>,
    cwd: PathBuf,
    agent_hint: Option<AgentId>,
    channel_hint: Option<ChannelName>,
    #[serde(default)]
    role_hint: Option<RoleName>,
    #[serde(default)]
    collection_hint: Option<CollectionName>,
    shared_dir: bool,
    reg: Option<Registered>,
    fenced: Option<String>,
    turn: u64,
    launch: Option<LaunchHandle>,
}

impl Host {
    fn table_path(&self) -> PathBuf {
        self.inner.cfg.home.join("sessions.json")
    }

    /// Writes the session table. Call it after each change to the table.
    pub(crate) fn save(&self) {
        let saved: Vec<Saved> = self
            .state()
            .sessions
            .values()
            .map(|s| Saved {
                id: s.id,
                harness: s.harness,
                harness_session_id: s.harness_session_id.clone(),
                pid: s.pid,
                cwd: s.cwd.clone(),
                agent_hint: s.agent_hint.clone(),
                channel_hint: s.channel_hint.clone(),
                role_hint: s.role_hint.clone(),
                collection_hint: s.collection_hint.clone(),
                shared_dir: s.shared_dir,
                reg: s.reg.clone(),
                fenced: s.fenced.clone(),
                turn: s.turn,
                launch: s.launch.clone(),
            })
            .collect();
        let path = self.table_path();
        let tmp = path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.inner.cfg.home)?;
            std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            tracing::warn!("could not save the session table: {e}");
        }
    }

    /// Loads the saved sessions whose harness process still runs, and
    /// starts their inbox loops. The next heartbeat checks their leases.
    pub async fn restore(&self) {
        self.restore_table().await;
        self.state().restored = true;
    }

    /// True once the saved sessions are loaded.
    pub fn restored(&self) -> bool {
        self.state().restored
    }

    async fn restore_table(&self) {
        let Ok(bytes) = std::fs::read(self.table_path()) else {
            return;
        };
        let saved: Vec<Saved> = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ignoring a bad session table: {e}");
                return;
            }
        };
        let mut restored = 0;
        for s in saved {
            if !s.pid.is_some_and(pid_alive) {
                continue;
            }
            let repo =
                checkpoint::detect(&s.cwd, self.inner.cfg.checkpoint_remote.as_deref()).await;
            let cancel = CancellationToken::new();
            let agent = s.reg.as_ref().map(|r| r.agent.clone());
            self.state().sessions.insert(
                s.id,
                LocalSess {
                    id: s.id,
                    harness: s.harness,
                    harness_session_id: s.harness_session_id,
                    pid: s.pid,
                    cwd: s.cwd,
                    agent_hint: s.agent_hint,
                    channel_hint: s.channel_hint,
                    role_hint: s.role_hint,
                    collection_hint: s.collection_hint,
                    repo,
                    shared_dir: s.shared_dir,
                    reg: s.reg,
                    queue: VecDeque::new(),
                    notify: Arc::new(Notify::new()),
                    turn: s.turn,
                    busy: false,
                    // The last turn of a restored session is unknown, so the
                    // host reports no idle signal until a turn ends.
                    idle_since: None,
                    fenced: s.fenced,
                    paused: false,
                    wake_pending: false,
                    moving: false,
                    last_tree: None,
                    checkpointing: false,
                    dirty: false,
                    launch: s.launch,
                    cancel: cancel.clone(),
                },
            );
            match agent {
                Some(agent) => {
                    self.load_log(&agent, s.id).await;
                    tokio::spawn(self.clone().inbox_loop(agent, s.id, cancel));
                }
                None => {
                    let host = self.clone();
                    let id = s.id;
                    tokio::spawn(async move {
                        while host.state().sessions.contains_key(&id) && !host.try_attach(id).await
                        {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    });
                }
            }
            restored += 1;
        }
        if restored > 0 {
            tracing::info!(restored, "restored local sessions");
        }
        self.save();
    }
}

/// The error for a session that this host does not know. Before the restore
/// ends, the session can still come back, so the caller must retry. Callers
/// pass `restored` from the state guard they already hold.
pub(crate) fn no_session(restored: bool, what: String) -> ApiError {
    if restored {
        ApiError::not_found(what)
    } else {
        ApiError::unavailable(format!("{what}: the host daemon is restoring its sessions"))
    }
}
