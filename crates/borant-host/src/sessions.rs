//! Local sessions: registration, events from harness adapters, fencing, and
//! checkpoints.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{SessionEventBody, Target};
use borant_core::proto::{
    ApiError, CtlRequest, EventReply, LocalEvent, LocalRegister, LocalSession, LocalState,
    RegisterSession, Registered, RenewedLease, SessionLeaseRef,
};
use borant_core::rules::{redact::redact, session_log};
use borant_core::{AgentId, SessionId};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::checkpoint;
use crate::{Host, LocalSess, Queued, terminate};

const RETRY_ATTACH: Duration = Duration::from_secs(5);
/// How long a registration call waits for the first attach attempt.
const FIRST_ATTACH_WAIT: Duration = Duration::from_secs(3);

impl Host {
    /// Registers a harness session. If the hub is unreachable, the session is
    /// detached and the daemon retries in the background.
    pub async fn register(&self, mut req: LocalRegister) -> Result<LocalSession, ApiError> {
        // The same native session registering again (a hook retry) gets the
        // same local session. A new native session in the same process, for
        // example after `/clear` in Claude Code, replaces the old one and
        // keeps its agent.
        let previous = req.pid.and_then(|pid| {
            self.state()
                .sessions
                .values()
                .find(|s| s.pid == Some(pid) && s.harness == req.harness)
                .map(|s| {
                    (
                        s.id,
                        s.harness_session_id.clone(),
                        s.reg.as_ref().map(|r| r.agent.clone()),
                    )
                })
        });
        if let Some((old, hsid, agent)) = previous {
            if hsid.is_some() && hsid == req.harness_session_id {
                return self.local_view(old);
            }
            if req.agent.is_none() {
                req.agent = agent;
            }
            self.end_session(old, "replaced by a new session in the same process")
                .await;
        }
        let id = SessionId::new();
        let cwd = PathBuf::from(&req.cwd);
        let repo = checkpoint::detect(&cwd, self.inner.cfg.checkpoint_remote.as_deref()).await;
        {
            let mut st = self.state();
            let shared_dir = st.sessions.values().any(|s| s.cwd == cwd);
            st.sessions.insert(
                id,
                LocalSess {
                    id,
                    harness: req.harness,
                    harness_session_id: req.harness_session_id.clone(),
                    pid: req.pid,
                    cwd,
                    agent_hint: req.agent.clone(),
                    channel_hint: req.channel.clone(),
                    role_hint: req.role.clone(),
                    collection_hint: req.collection.clone(),
                    repo,
                    shared_dir,
                    reg: None,
                    queue: VecDeque::new(),
                    notify: Arc::new(Notify::new()),
                    turn: 0,
                    busy: false,
                    idle_since: None,
                    fenced: None,
                    paused: false,
                    wake_pending: false,
                    moving: false,
                    last_tree: None,
                    checkpointing: false,
                    dirty: false,
                    launch: None,
                    cancel: CancellationToken::new(),
                },
            );
        }
        self.save();
        // The attach runs in its own task, so a caller that gives up does not
        // cancel it, and the retries continue in the background.
        let (first, done) = tokio::sync::oneshot::channel();
        let host = self.clone();
        tokio::spawn(async move {
            let ok = host.try_attach(id).await;
            let _ = first.send(());
            if ok {
                return;
            }
            loop {
                tokio::time::sleep(RETRY_ATTACH).await;
                let gone = !host.state().sessions.contains_key(&id);
                if gone || host.try_attach(id).await {
                    return;
                }
            }
        });
        let _ = tokio::time::timeout(FIRST_ATTACH_WAIT, done).await;
        self.local_view(id)
    }

    /// Registers a local session with the hub. Returns true on success.
    pub(crate) async fn try_attach(&self, id: SessionId) -> bool {
        let req = {
            let st = self.state();
            let Some(s) = st.sessions.get(&id) else {
                return true;
            };
            if s.reg.is_some() {
                return true;
            }
            RegisterSession {
                session: id,
                host: self.inner.cfg.name.clone(),
                harness: s.harness,
                harness_session_id: s.harness_session_id.clone(),
                pid: s.pid,
                cwd: s.cwd.to_string_lossy().into_owned(),
                repo: s.repo.as_ref().map(|r| r.info.clone()),
                agent: s.agent_hint.clone(),
                channel: s.channel_hint.clone(),
                shared_dir: s.shared_dir,
                role: s.role_hint.clone(),
                collection: s.collection_hint.clone(),
            }
        };
        let reg: Registered = match self.hub(&CtlRequest::RegisterSession(req)).await {
            Ok(v) => match serde_json::from_value(v) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("bad registration reply: {e}");
                    return false;
                }
            },
            Err(e) => {
                tracing::warn!(session = %id, "registration failed, will retry: {}", e.message);
                return false;
            }
        };
        let agent = reg.agent.clone();
        let cancel = {
            let mut st = self.state();
            let launch = st.pending_launch.remove(&agent);
            let Some(s) = st.sessions.get_mut(&id) else {
                return true;
            };
            s.reg = Some(reg);
            if launch.is_some() {
                s.launch = launch;
            }
            s.cancel.clone()
        };
        let cwd = self
            .state()
            .sessions
            .get(&id)
            .map(|s| s.cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.save();
        self.append(&agent, Some(id), SessionEventBody::SessionStart { cwd })
            .await;
        self.load_log(&agent, id).await;
        tokio::spawn(self.clone().inbox_loop(agent.clone(), id, cancel));
        tracing::info!(session = %id, %agent, "session attached");
        true
    }

    /// Fills the duplicate filter from the session log, and queues messages
    /// that the log holds but that no session got yet, for example after a
    /// move.
    pub(crate) async fn load_log(&self, agent: &AgentId, id: SessionId) {
        let Ok(events) = self.inner.store.read(agent).await else {
            return;
        };
        let events: Vec<_> = events.into_iter().map(|(_, e)| e).collect();
        let undelivered = session_log::undelivered(&events);
        let mut st = self.state();
        let seen = st.seen.entry(agent.clone()).or_default();
        for e in &events {
            if let SessionEventBody::SwarmMessage { envelope } = &e.body {
                seen.insert(envelope.id);
            }
        }
        if let Some(s) = st.sessions.get_mut(&id) {
            for env in undelivered {
                s.queue.push_back(Queued { env, wake: false });
            }
            s.turn = events.iter().map(|e| e.turn).max().unwrap_or(0);
        }
    }

    pub fn local_view(&self, id: SessionId) -> Result<LocalSession, ApiError> {
        let st = self.state();
        let s = st.sessions.get(&id).ok_or_else(|| {
            crate::persist::no_session(st.restored, format!("no local session {id}"))
        })?;
        Ok(LocalSession {
            session: id,
            agent: s.reg.as_ref().map(|r| r.agent.clone()),
            host: self.inner.cfg.name.clone(),
            state: if s.reg.is_some() {
                LocalState::Attached
            } else {
                LocalState::Detached
            },
            movable: s.movable(),
            pinned: s.reg.as_ref().is_some_and(|r| r.pinned),
            channels: s
                .reg
                .as_ref()
                .map(|r| r.channels.clone())
                .unwrap_or_default(),
            fenced: s.fenced.clone().or_else(|| {
                s.paused.then(|| {
                    "The Borant hub is unreachable, so tools are paused until it returns."
                        .to_owned()
                })
            }),
        })
    }

    /// The session whose harness PID is one of `ancestors`, nearest first.
    pub fn resolve_pid(&self, ancestors: &[u32]) -> Option<SessionId> {
        let st = self.state();
        ancestors.iter().find_map(|p| {
            st.sessions
                .values()
                .find(|s| s.pid == Some(*p))
                .map(|s| s.id)
        })
    }

    /// The session with this native harness session ID, if any.
    pub fn resolve_harness_session(&self, hsid: &str) -> Option<SessionId> {
        self.state()
            .sessions
            .values()
            .find(|s| s.harness_session_id.as_deref() == Some(hsid))
            .map(|s| s.id)
    }

    pub(crate) fn lease_refs(&self) -> Vec<SessionLeaseRef> {
        self.state()
            .sessions
            .values()
            .filter(|s| s.fenced.is_none())
            .filter_map(|s| {
                s.reg.as_ref().map(|r| SessionLeaseRef {
                    session: s.id,
                    agent: r.agent.clone(),
                    token: r.token,
                    idle_since: s.idle_since,
                })
            })
            .collect()
    }

    /// Takes the new tokens of sessions whose lease the hub gave back after
    /// an expiry, and saves the table.
    pub(crate) fn apply_renewed(&self, renewed: &[RenewedLease]) {
        if renewed.is_empty() {
            return;
        }
        {
            let mut st = self.state();
            for r in renewed {
                let Some(s) = st.sessions.get_mut(&r.session) else {
                    continue;
                };
                if s.fenced.is_some() {
                    continue;
                }
                if let Some(reg) = &mut s.reg {
                    tracing::info!(session = %r.session, token = r.token.0, "lease reclaimed after expiry");
                    reg.token = r.token;
                }
            }
        }
        self.save();
    }

    pub(crate) fn set_paused(&self, on: bool) {
        let mut st = self.state();
        // A pause after a full recovery is a new outage: its sessions get one
        // wake message when the hub is back. A pause during the same outage,
        // which a flapping hub causes, does not.
        let arm = on && st.outage_over;
        if arm {
            st.outage_over = false;
        }
        for s in st.sessions.values_mut() {
            if arm && s.fenced.is_none() {
                s.wake_pending = true;
            }
            if s.paused != on {
                s.paused = on;
                s.notify.notify_waiters();
            }
        }
    }

    /// Handles one event from a harness adapter.
    pub async fn event(&self, id: SessionId, ev: LocalEvent) -> Result<EventReply, ApiError> {
        let now = self.inner.clock.now();
        let (agent, start_turn) = {
            let mut st = self.state();
            let restored = st.restored;
            let s = st.sessions.get_mut(&id).ok_or_else(|| {
                crate::persist::no_session(restored, format!("no local session {id}"))
            })?;
            let start = match &ev {
                LocalEvent::TurnStart => true,
                LocalEvent::UserPrompt { .. } => !s.busy,
                _ => false,
            };
            if start {
                s.turn += 1;
                s.busy = true;
                s.idle_since = None;
            }
            if matches!(ev, LocalEvent::TurnEnd) {
                s.busy = false;
                s.idle_since = Some(now);
            }
            (s.reg.as_ref().map(|r| r.agent.clone()), start)
        };
        if let Some(agent) = &agent {
            if start_turn {
                self.append(agent, Some(id), SessionEventBody::TurnStart)
                    .await;
            }
            let cut = |t: &str| session_log::cut(&redact(t), session_log::MAX_TOOL_BYTES);
            let body = match &ev {
                LocalEvent::UserPrompt { text } => {
                    Some(SessionEventBody::UserPrompt { text: cut(text) })
                }
                LocalEvent::ToolCall { name, input } => Some(SessionEventBody::ToolCall {
                    name: name.clone(),
                    input: cut(input),
                }),
                LocalEvent::ToolResult {
                    name,
                    output,
                    is_error,
                } => Some(SessionEventBody::ToolResult {
                    name: name.clone(),
                    output: cut(output),
                    is_error: *is_error,
                }),
                LocalEvent::AssistantMessage { text } => {
                    Some(SessionEventBody::AssistantMessage { text: cut(text) })
                }
                _ => None,
            };
            if let Some(b) = body {
                self.append(agent, Some(id), b).await;
            }
        }
        match &ev {
            LocalEvent::ToolResult { .. } => self.schedule_checkpoint(id),
            LocalEvent::TurnEnd => {
                self.schedule_checkpoint(id);
                let moving = self.state().sessions.get(&id).is_some_and(|s| s.moving);
                if moving {
                    let host = self.clone();
                    tokio::spawn(async move { host.finish_move(id).await });
                }
            }
            LocalEvent::End { reason } => {
                self.end_session(id, reason).await;
                return Ok(EventReply::default());
            }
            _ => {}
        }
        // Only events whose hook can show text to the model take messages.
        let deliver = matches!(
            ev,
            LocalEvent::TurnStart
                | LocalEvent::UserPrompt { .. }
                | LocalEvent::ToolCall { .. }
                | LocalEvent::ToolResult { .. }
        );
        let messages = if deliver {
            self.drain(id).await
        } else {
            Vec::new()
        };
        Ok(EventReply {
            messages,
            fenced: self.fence_reason(id),
        })
    }

    pub(crate) fn fence_reason(&self, id: SessionId) -> Option<String> {
        let st = self.state();
        let s = st.sessions.get(&id)?;
        s.fenced.clone().or_else(|| {
            s.paused.then(|| {
                "The Borant hub is unreachable, so tools are paused until it returns.".to_owned()
            })
        })
    }

    /// Ends a local session and tells the hub.
    pub async fn end_session(&self, id: SessionId, reason: &str) {
        let Some(s) = self.state().sessions.remove(&id) else {
            return;
        };
        s.cancel.cancel();
        s.notify.notify_waiters();
        self.save();
        if let Some(r) = &s.reg {
            self.append(
                &r.agent,
                Some(id),
                SessionEventBody::SessionEnd {
                    reason: reason.into(),
                },
            )
            .await;
            if s.fenced.is_none() {
                let req = CtlRequest::EndSession {
                    session: id,
                    agent: r.agent.clone(),
                    token: r.token,
                    reason: reason.into(),
                };
                if let Err(e) = self.hub(&req).await {
                    tracing::warn!(session = %id, "could not tell the hub about the end: {}", e.message);
                }
            }
        }
        tracing::info!(session = %id, reason, "session ended");
    }

    /// Blocks the tools of a session and stops its inbox.
    pub(crate) fn mark_fenced(&self, id: SessionId, reason: String) {
        {
            let mut st = self.state();
            let Some(s) = st.sessions.get_mut(&id) else {
                return;
            };
            s.fenced = Some(reason);
            s.cancel.cancel();
            s.notify.notify_waiters();
        }
        self.save();
    }

    /// Saves the work of the lost turn of a fenced session to a stale ref and
    /// stops its harness process.
    pub(crate) async fn stop_fenced(&self, id: SessionId) {
        let info = {
            let st = self.state();
            let Some(s) = st.sessions.get(&id) else {
                return;
            };
            (
                s.reg.as_ref().map(|r| r.agent.clone()),
                s.repo.clone(),
                s.pid,
                s.launch.clone(),
            )
        };
        let (agent, repo, pid, launch) = info;
        if let (Some(agent), Some(repo)) = (&agent, &repo) {
            let remote = self.remote_for(repo);
            match checkpoint::save_stale(&repo.root, &remote, agent, self.inner.clock.now()).await {
                Ok(sha) => tracing::info!(%agent, sha, "saved the lost turn to a stale ref"),
                Err(e) => tracing::warn!(%agent, "could not save stale work: {e:#}"),
            }
        }
        // The session stays known, so its hooks keep blocking tools until
        // the harness process exits.
        self.stop_process(pid, launch.as_ref()).await;
    }

    /// Fences a session. With `kill`, it also saves the stale work and stops
    /// the harness process.
    pub(crate) async fn fence(&self, id: SessionId, reason: String, kill: bool) {
        self.mark_fenced(id, reason);
        if kill {
            self.stop_fenced(id).await;
        }
    }

    pub(crate) async fn stop_process(
        &self,
        pid: Option<u32>,
        launch: Option<&borant_core::ports::LaunchHandle>,
    ) {
        if let Some(h) = launch {
            if let Err(e) = self.inner.launcher.stop(h).await {
                tracing::warn!("launcher stop failed: {e}");
            }
        } else if let Some(p) = pid {
            terminate(p);
        }
    }

    /// The checkpoint remote of a repository: the one of its project, else
    /// the one of the host, else its origin.
    pub(crate) fn remote_for(&self, repo: &checkpoint::RepoFacts) -> String {
        repo.checkpoint_remote
            .clone()
            .or_else(|| self.inner.cfg.checkpoint_remote.clone())
            .unwrap_or_else(|| repo.info.remote_url.clone())
    }

    /// Starts a checkpoint in the background, or marks the tree dirty if one
    /// is running. One checkpoint runs at a time for each session.
    pub(crate) fn schedule_checkpoint(&self, id: SessionId) {
        {
            let mut st = self.state();
            let Some(s) = st.sessions.get_mut(&id) else {
                return;
            };
            if !s.movable() || s.fenced.is_some() {
                return;
            }
            if s.checkpointing {
                s.dirty = true;
                return;
            }
            s.checkpointing = true;
        }
        let host = self.clone();
        tokio::spawn(async move {
            loop {
                let _ = host.checkpoint_now(id).await;
                let mut st = host.state();
                let Some(s) = st.sessions.get_mut(&id) else {
                    return;
                };
                if s.dirty {
                    s.dirty = false;
                    continue;
                }
                s.checkpointing = false;
                return;
            }
        });
    }

    /// Runs one checkpoint now and logs it.
    /// Returns false if a checkpoint was due and failed.
    pub(crate) async fn checkpoint_now(&self, id: SessionId) -> bool {
        let (agent, repo, last) = {
            let st = self.state();
            let Some(s) = st.sessions.get(&id) else {
                return true;
            };
            let (Some(r), Some(repo)) = (&s.reg, &s.repo) else {
                return true;
            };
            (r.agent.clone(), repo.clone(), s.last_tree.clone())
        };
        let remote = self.remote_for(&repo);
        match checkpoint::create(&repo.root, &remote, &agent, last.as_deref()).await {
            Ok(Some((tree, sha))) => {
                if let Some(s) = self.state().sessions.get_mut(&id) {
                    s.last_tree = Some(tree);
                }
                self.append(
                    &agent,
                    Some(id),
                    SessionEventBody::Checkpoint {
                        git_ref: checkpoint::wip_ref(&agent),
                        sha,
                    },
                )
                .await;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(%agent, "checkpoint failed: {e:#}");
                return false;
            }
        }
        true
    }

    /// Waits for a running checkpoint, which ends within the git timeout.
    async fn settled(&self, id: SessionId) {
        for _ in 0..700 {
            let busy = self
                .state()
                .sessions
                .get(&id)
                .is_some_and(|s| s.checkpointing);
            if !busy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Finishes a planned move: waits for a running checkpoint, writes a
    /// final one, stops the session, and tells the hub.
    pub(crate) async fn finish_move(&self, id: SessionId) {
        self.settled(id).await;
        if !self.checkpoint_now(id).await {
            // A planned move must lose nothing, so the session keeps running.
            if let Some(s) = self.state().sessions.get_mut(&id) {
                s.moving = false;
            }
            tracing::error!(session = %id, "planned move stopped: the final checkpoint failed");
            return;
        }
        let Some(s) = self.state().sessions.remove(&id) else {
            return;
        };
        s.cancel.cancel();
        self.save();
        let Some(reg) = s.reg.clone() else { return };
        self.append(
            &reg.agent,
            Some(id),
            SessionEventBody::SessionEnd {
                reason: "planned move".into(),
            },
        )
        .await;
        self.stop_process(s.pid, s.launch.as_ref()).await;
        let req = CtlRequest::MoveReady {
            agent: reg.agent.clone(),
            session: id,
            token: reg.token,
        };
        match self.hub(&req).await {
            Ok(_) => tracing::info!(agent = %reg.agent, "planned move: session stopped"),
            Err(e) => tracing::warn!(agent = %reg.agent, "move_ready failed: {}", e.message),
        }
    }

    /// Parks an idle session: writes the final checkpoint, stops the harness,
    /// and ends the session with reason `park`, so the hub learns in one
    /// write that the agent is parked. A session in a turn keeps running, and
    /// so does one whose final checkpoint fails.
    pub(crate) async fn park(&self, id: SessionId) {
        {
            let st = self.state();
            let Some(s) = st.sessions.get(&id) else {
                return;
            };
            if s.busy || s.fenced.is_some() || s.reg.is_none() {
                tracing::warn!(session = %id, "park skipped: the session is not idle");
                return;
            }
        }
        self.settled(id).await;
        if !self.checkpoint_now(id).await {
            tracing::error!(session = %id, "park stopped: the final checkpoint failed");
            return;
        }
        let info = {
            let mut st = self.state();
            let Some(s) = st.sessions.get_mut(&id) else {
                return;
            };
            if s.busy {
                tracing::warn!(session = %id, "park stopped: the session started a turn");
                return;
            }
            s.cancel.cancel();
            s.notify.notify_waiters();
            (s.reg.clone(), s.pid, s.launch.clone())
        };
        if self.state().sessions.remove(&id).is_none() {
            return;
        }
        self.save();
        let (Some(reg), pid, launch) = info else {
            return;
        };
        // The session is gone before the harness stops, so a hook that
        // reports the end cannot label this release as an ordinary end.
        self.append(
            &reg.agent,
            Some(id),
            SessionEventBody::SessionEnd {
                reason: "park".into(),
            },
        )
        .await;
        self.stop_process(pid, launch.as_ref()).await;
        let req = CtlRequest::EndSession {
            session: id,
            agent: reg.agent.clone(),
            token: reg.token,
            reason: "park".into(),
        };
        match self.hub(&req).await {
            Ok(_) => tracing::info!(agent = %reg.agent, session = %id, "session parked"),
            Err(e) => {
                tracing::warn!(agent = %reg.agent, "could not tell the hub about the park: {}", e.message)
            }
        }
    }

    /// Notifies an agent that it hit its wake limit, once.
    pub(crate) async fn wake_limit_notice(&self, agent: &AgentId, sender: Option<&AgentId>) {
        let first = self.state().limit_noticed.insert(agent.clone());
        let Some(to) = sender.filter(|_| first) else {
            return;
        };
        let text = format!("Borant: agent {agent} reached its wake limit.");
        let _ = self
            .send(None, Target::Agent(to.clone()), text, false)
            .await;
    }
}
