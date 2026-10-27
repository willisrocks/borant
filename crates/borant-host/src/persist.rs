//! The local session table on disk, so a restart of the host daemon keeps
//! its live sessions. The file is `$BORANT_HOME/sessions.json`.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use borant_core::model::Harness;
use borant_core::ports::LaunchHandle;
use borant_core::ports::secrets::{BundleName, MemberName};
use borant_core::proto::{ApiError, Registered};
use borant_core::{AgentId, ChannelName, CollectionName, RoleName, SessionId};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::secrets::LaunchVars;
use crate::{Host, LocalSess, checkpoint, pid_alive};

/// The limit of one step of the restore. A session log over a slow link, or a
/// secret store that does not answer, must not hold the host offline: past
/// this, the session goes on without the step and a warn names it.
const RESTORE_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A restore step that takes longer than this logs at info, so a slow restart
/// names its slow step.
const RESTORE_SLOW_STEP: std::time::Duration = std::time::Duration::from_millis(500);

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
    /// The bundles that the launch resolved: names only.
    #[serde(default)]
    bundles: Vec<BundleName>,
    /// The member of each member bundle that the launch used.
    #[serde(default)]
    members: BTreeMap<BundleName, MemberName>,
    /// The secret directory to delete when the session ends.
    #[serde(default)]
    secrets_dir: Option<PathBuf>,
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
                bundles: s.bundles.clone(),
                members: s.members.clone(),
                secrets_dir: s.secrets_dir.clone(),
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

    /// Runs one step of the restore under `RESTORE_STEP_TIMEOUT` and logs how
    /// long it took: at info past `RESTORE_SLOW_STEP`, else at debug. A step
    /// that does not answer is dropped with a warn that names it, so a slow
    /// session log or secret store cannot hold the host offline.
    async fn step<F, T>(&self, name: &'static str, agent: Option<AgentId>, f: F) -> Option<T>
    where
        F: Future<Output = T>,
    {
        let started = std::time::Instant::now();
        match tokio::time::timeout(RESTORE_STEP_TIMEOUT, f).await {
            Ok(value) => {
                let took = started.elapsed();
                if took >= RESTORE_SLOW_STEP {
                    tracing::info!(step = name, agent = ?agent, ms = took.as_millis(), "slow restore step");
                } else {
                    tracing::debug!(step = name, agent = ?agent, ms = took.as_millis(), "restore step");
                }
                Some(value)
            }
            Err(_) => {
                tracing::warn!(
                    step = name,
                    agent = ?agent,
                    timeout = ?RESTORE_STEP_TIMEOUT,
                    "restore step did not finish, the session goes on without it"
                );
                None
            }
        }
    }

    async fn restore_table(&self) {
        let started = std::time::Instant::now();
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
        let mut jobs = Vec::new();
        for s in saved {
            if !s.pid.is_some_and(pid_alive) {
                continue;
            }
            let agent = s.reg.as_ref().map(|r| r.agent.clone());
            let repo = self
                .step(
                    "detect",
                    agent.clone(),
                    checkpoint::detect(&s.cwd, self.inner.cfg.checkpoint_remote.as_deref()),
                )
                .await
                .flatten();
            let cancel = CancellationToken::new();
            let bundles = s.bundles.clone();
            let members = s.members.clone();
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
                    last_event_at: None,
                    fenced: s.fenced,
                    paused: false,
                    wake_pending: false,
                    moving: false,
                    last_tree: None,
                    checkpointing: false,
                    dirty: false,
                    launch: s.launch,
                    cancel: cancel.clone(),
                    bundles: s.bundles,
                    members: s.members,
                    secrets_dir: s.secrets_dir,
                    // Filled by the restore below, from the saved bundles.
                    vars: LaunchVars::default(),
                },
            );
            jobs.push((s.id, agent, bundles, members, cancel));
        }
        let restored = jobs.len();
        // The steps that use the network run together: two sessions on one
        // host must not add their waits.
        let mut waits = FuturesUnordered::new();
        for (id, agent, bundles, members, cancel) in jobs {
            let Some(agent) = agent else {
                let host = self.clone();
                tokio::spawn(async move {
                    while host.state().sessions.contains_key(&id) && !host.try_attach(id).await {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                });
                continue;
            };
            let host = self.clone();
            waits.push(async move {
                // The values of a live session are still in its directory, so
                // a restart resolves them again: the session log must keep
                // redacting them, not only the patterns.
                if !bundles.is_empty() {
                    host.step(
                        "secrets",
                        Some(agent.clone()),
                        host.restore_secrets(id, &agent, &bundles, &members),
                    )
                    .await;
                }
                host.step(
                    "session log",
                    Some(agent.clone()),
                    host.load_log(&agent, id),
                )
                .await;
                tokio::spawn(host.inbox_loop(agent, id, cancel));
            });
        }
        while waits.next().await.is_some() {}
        if restored > 0 {
            tracing::info!(
                restored,
                ms = started.elapsed().as_millis(),
                "restored local sessions"
            );
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use borant_core::model::{Harness, SessionEvent, SessionEventBody};
    use borant_core::ports::{PortResult, SessionStore};
    use borant_core::{EventId, FencingToken, SessionId, SystemClock, Timestamp};
    use borant_launch::ProcessLauncher;
    use borant_memory::{MemoryBus, MemorySession};

    use super::*;
    use crate::outbox::Outbox;

    /// A session log whose read does not answer before the step timeout, as a
    /// long log over a slow link does not.
    #[derive(Debug)]
    struct SlowLog(MemorySession);

    #[async_trait]
    impl SessionStore for SlowLog {
        async fn append(&self, agent: &AgentId, ev: &SessionEvent) -> PortResult<u64> {
            self.0.append(agent, ev).await
        }

        async fn read(&self, _agent: &AgentId) -> PortResult<Vec<(u64, SessionEvent)>> {
            tokio::time::sleep(Duration::from_secs(600)).await;
            Ok(Vec::new())
        }

        async fn trim_before(&self, agent: &AgentId, seq: u64) -> PortResult<()> {
            self.0.trim_before(agent, seq).await
        }

        async fn delete(&self, agent: &AgentId) -> PortResult<()> {
            self.0.delete(agent).await
        }

        async fn agents(&self) -> PortResult<Vec<AgentId>> {
            self.0.agents().await
        }
    }

    fn host_at(home: &Path, store: Arc<dyn SessionStore>) -> Host {
        let mut cfg = crate::HostConfig::new("h1".parse().unwrap(), home.to_path_buf());
        cfg.harnesses = vec![Harness::Fake];
        Host::new(
            cfg,
            Arc::new(MemoryBus::new()),
            store,
            Arc::new(SystemClock),
            Box::new(ProcessLauncher::new(home.join("logs"))),
            None,
            None,
            Outbox::in_memory().unwrap(),
        )
    }

    /// The session table of one live session of this process, of `turn`, in
    /// `home`.
    fn save_one(home: &Path, agent: &AgentId, turn: u64) {
        let id = SessionId::new();
        let saved = vec![Saved {
            id,
            harness: Harness::Fake,
            harness_session_id: None,
            pid: Some(std::process::id()),
            cwd: home.to_path_buf(),
            agent_hint: None,
            channel_hint: None,
            role_hint: None,
            collection_hint: None,
            shared_dir: false,
            reg: Some(Registered {
                agent: agent.clone(),
                session: id,
                token: FencingToken(0),
                movable: true,
                channels: Vec::new(),
                pinned: true,
            }),
            fenced: None,
            turn,
            launch: None,
            bundles: Vec::new(),
            members: BTreeMap::new(),
            secrets_dir: None,
        }];
        std::fs::write(
            home.join("sessions.json"),
            serde_json::to_vec(&saved).unwrap(),
        )
        .unwrap();
    }

    /// A session log over a slow link must not hold the restore: the step is
    /// bounded, the session comes back, and the host is online.
    #[tokio::test]
    async fn restore_bounds_a_session_log_that_does_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let agent: AgentId = "w1".parse().unwrap();
        save_one(dir.path(), &agent, 0);
        let host = host_at(dir.path(), Arc::new(SlowLog(MemorySession::new())));
        let started = Instant::now();
        host.restore().await;
        let took = started.elapsed();
        assert!(host.restored());
        assert!(
            took < RESTORE_STEP_TIMEOUT * 2,
            "the restore waited {took:?} for a log that does not answer"
        );
        assert!(
            host.state()
                .sessions
                .values()
                .any(|s| s.reg.as_ref().is_some_and(|r| r.agent == agent)),
            "the session came back"
        );
    }

    /// The restore still reads the log of a live session: the turn of the
    /// session comes from it, and the messages that no session got are
    /// queued.
    #[tokio::test]
    async fn restore_reads_the_log_of_a_live_session() {
        let dir = tempfile::tempdir().unwrap();
        let agent: AgentId = "w1".parse().unwrap();
        let store = Arc::new(MemorySession::new());
        store
            .append(
                &agent,
                &SessionEvent {
                    id: EventId::new(),
                    agent: agent.clone(),
                    session: None,
                    harness: Harness::Fake,
                    host: "h1".parse().unwrap(),
                    turn: 7,
                    at: Timestamp(1),
                    body: SessionEventBody::TurnStart,
                },
            )
            .await
            .unwrap();
        save_one(dir.path(), &agent, 0);
        let host = host_at(dir.path(), store);
        host.restore().await;
        let turn = host.state().sessions.values().map(|s| s.turn).max();
        assert_eq!(turn, Some(7), "the restore read the session log");
    }
}
