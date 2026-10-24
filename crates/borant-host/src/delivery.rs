//! The inbox of each local agent, delivery into sessions, local calls, the
//! session log, and the outbox.

use std::time::Duration;

use borant_core::model::{
    Envelope, EnvelopeKind, Sender, SessionEvent, SessionEventBody, Target, TaskStatus,
};
use borant_core::ports::{Delivery, PortErrorKind};
use borant_core::proto::{Actor, ApiError, CtlRequest, ToolCall};
use borant_core::rules::{redact::redact, session_log, wake};
use borant_core::{AgentId, EventId, MessageId, SessionId, subjects};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::outbox::OutboxKind;
use crate::{Host, Queued};

/// The size limit of a message text.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

impl Host {
    pub(crate) async fn inbox_loop(self, agent: AgentId, id: SessionId, cancel: CancellationToken) {
        loop {
            let pulled = tokio::select! {
                _ = cancel.cancelled() => return,
                r = self.inner.bus.pull(&agent, 16, Duration::from_secs(5)) => r,
            };
            match pulled {
                Ok(batch) => {
                    for d in batch {
                        self.receive(&agent, id, d).await;
                    }
                }
                Err(e) => {
                    tracing::debug!(%agent, "inbox pull failed: {e}");
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    }
                }
            }
        }
    }

    /// Logs a received message, acks it, and queues it for the session.
    async fn receive(&self, agent: &AgentId, id: SessionId, d: Delivery) {
        let env = d.envelope;
        let own_broadcast =
            env.from == Sender::Agent(agent.clone()) && env.kind != EnvelopeKind::Direct;
        let seen = self
            .state()
            .seen
            .get(agent)
            .is_some_and(|s| s.contains(&env.id));
        if seen || own_broadcast {
            let _ = d.acker.ack().await;
            return;
        }
        let mut logged = env.clone();
        logged.text = redact(&logged.text).into_owned();
        let ev = self.event_record(
            agent,
            Some(id),
            SessionEventBody::SwarmMessage { envelope: logged },
        );
        if let Err(e) = self.inner.store.append(agent, &ev).await {
            // Without the log entry, the message must stay in the inbox.
            tracing::debug!(%agent, "cannot log message yet: {e}");
            return;
        }
        if let Err(e) = d.acker.ack().await {
            tracing::debug!(%agent, "ack failed, the message can arrive again: {e}");
        }
        let now = self.inner.clock.now();
        let (wake, over_limit) = {
            let mut st = self.state();
            st.seen.entry(agent.clone()).or_default().insert(env.id);
            let wants = wake::should_wake(&env, agent);
            let allowed = wants && st.limiter.allow(agent, now);
            if let Some(s) = st.sessions.get_mut(&id) {
                s.queue.push_back(Queued {
                    env: env.clone(),
                    wake: allowed,
                });
                s.notify.notify_waiters();
            }
            (allowed, wants && !allowed)
        };
        if over_limit {
            let sender = match &env.from {
                Sender::Agent(a) => Some(a.clone()),
                Sender::Human => None,
            };
            self.wake_limit_notice(agent, sender.as_ref()).await;
        }
        tracing::debug!(%agent, id = %env.id, wake, "message queued");
    }

    /// Takes every queued message of a session and logs the deliveries.
    pub(crate) async fn drain(&self, id: SessionId) -> Vec<Envelope> {
        let (agent, msgs) = {
            let mut st = self.state();
            let Some(s) = st.sessions.get_mut(&id) else {
                return Vec::new();
            };
            let msgs: Vec<Envelope> = s.queue.drain(..).map(|q| q.env).collect();
            (s.reg.as_ref().map(|r| r.agent.clone()), msgs)
        };
        if let Some(agent) = agent {
            for m in &msgs {
                self.append(
                    &agent,
                    Some(id),
                    SessionEventBody::Delivered { message: m.id },
                )
                .await;
            }
        }
        msgs
    }

    /// Waits until a message that wakes the session is queued, then returns
    /// it with every other queued message. Returns empty at the timeout, or
    /// at once when the session is fenced or paused.
    pub async fn wait(&self, id: SessionId, timeout: Duration) -> Result<Vec<Envelope>, ApiError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notify = {
                let st = self.state();
                let s = st
                    .sessions
                    .get(&id)
                    .ok_or_else(|| ApiError::not_found(format!("no local session {id}")))?;
                s.notify.clone()
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let ready = {
                let st = self.state();
                let Some(s) = st.sessions.get(&id) else {
                    return Ok(Vec::new());
                };
                if s.fenced.is_some() || s.paused {
                    return Ok(Vec::new());
                }
                s.queue.iter().any(|q| q.wake)
            };
            if ready {
                return Ok(self.drain(id).await);
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => return Ok(Vec::new()),
            }
        }
    }

    /// Messages for a session as they arrive, for the Pi and omp extension.
    pub fn stream(&self, id: SessionId) -> BoxStream<'static, Envelope> {
        let host = self.clone();
        futures::stream::unfold(
            (host, Vec::<Envelope>::new()),
            move |(host, mut buf)| async move {
                loop {
                    if let Some(e) = buf.pop() {
                        return Some((e, (host, buf)));
                    }
                    match host.wait(id, Duration::from_secs(30)).await {
                        Ok(msgs) => {
                            buf = msgs;
                            buf.reverse();
                            if buf.is_empty() && !host.state().sessions.contains_key(&id) {
                                return None;
                            }
                            if buf.is_empty() {
                                let fenced = host.fence_reason(id).is_some();
                                if fenced {
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                }
                            }
                        }
                        Err(_) => return None,
                    }
                }
            },
        )
        .boxed()
    }

    /// A tool call from a session, or from the CLI when `session` is `None`.
    pub async fn call(
        &self,
        session: Option<SessionId>,
        call: ToolCall,
    ) -> Result<Value, ApiError> {
        let actor = match session {
            None => None,
            Some(id) => {
                let st = self.state();
                let s = st
                    .sessions
                    .get(&id)
                    .ok_or_else(|| ApiError::not_found(format!("no local session {id}")))?;
                if let Some(reason) = &s.fenced {
                    return Err(ApiError::invalid(reason.clone()));
                }
                let r = s.reg.as_ref().ok_or_else(|| {
                    ApiError::unavailable("this session is not registered with the hub yet")
                })?;
                Some(Actor {
                    agent: r.agent.clone(),
                    session: id,
                    token: r.token,
                })
            }
        };
        match call {
            ToolCall::Send { to, text, urgent } => {
                let from = actor.as_ref().map(|a| a.agent.clone());
                self.send(from.as_ref(), to, text, urgent).await
            }
            ToolCall::SessionHistory { page, page_size } => {
                let a =
                    actor.ok_or_else(|| ApiError::invalid("session_history needs a session"))?;
                self.history(&a.agent, page, page_size).await
            }
            call => self.hub(&CtlRequest::Call { actor, call }).await,
        }
    }

    /// Publishes a message. If the bus is unreachable, the message waits in
    /// the outbox.
    pub(crate) async fn send(
        &self,
        from: Option<&AgentId>,
        to: Target,
        text: String,
        urgent: bool,
    ) -> Result<Value, ApiError> {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(ApiError::invalid(format!(
                "the message has {} bytes; the limit is {MAX_MESSAGE_BYTES}",
                text.len()
            )));
        }
        let (subject, kind) = match &to {
            Target::Agent(a) => (subjects::agent(a), EnvelopeKind::Direct),
            Target::Channel(c) => (subjects::broadcast(c), EnvelopeKind::Broadcast),
        };
        let env = Envelope {
            id: MessageId::new(),
            from: from.cloned().map_or(Sender::Human, Sender::Agent),
            to,
            kind,
            text,
            urgent,
            event: None,
            sent: self.inner.clock.now(),
        };
        match self.inner.bus.publish(&subject, &env).await {
            Ok(()) => Ok(json!({ "id": env.id, "queued": false })),
            Err(e) if matches!(e.kind, PortErrorKind::Unavailable | PortErrorKind::Timeout) => {
                let payload =
                    serde_json::to_vec(&env).map_err(|e| ApiError::internal(e.to_string()))?;
                self.inner
                    .outbox
                    .push(OutboxKind::Publish, &subject, &payload)
                    .map_err(|e| ApiError::internal(e.to_string()))?;
                Ok(json!({ "id": env.id, "queued": true }))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// One page of the session log, counted from the newest end. Page 0 holds
    /// the newest events. Events in a page are oldest first.
    pub(crate) async fn history(
        &self,
        agent: &AgentId,
        page: usize,
        page_size: usize,
    ) -> Result<Value, ApiError> {
        let events: Vec<SessionEvent> = self
            .inner
            .store
            .read(agent)
            .await?
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        let size = page_size.clamp(1, 500);
        let total = events.len();
        let end = total.saturating_sub(page * size);
        let start = end.saturating_sub(size);
        Ok(json!({
            "agent": agent,
            "total": total,
            "page": page,
            "pages": total.div_ceil(size),
            "note": "Page 0 holds the newest events. Ask for page 1, 2, ... for older ones.",
            "events": &events[start..end],
        }))
    }

    pub(crate) fn event_record(
        &self,
        agent: &AgentId,
        session: Option<SessionId>,
        body: SessionEventBody,
    ) -> SessionEvent {
        let (turn, harness) = session
            .and_then(|id| self.state().sessions.get(&id).map(|s| (s.turn, s.harness)))
            .unwrap_or((0, borant_core::model::Harness::Fake));
        SessionEvent {
            id: EventId::new(),
            agent: agent.clone(),
            session,
            harness,
            host: self.inner.cfg.name.clone(),
            turn,
            at: self.inner.clock.now(),
            body,
        }
    }

    /// Appends to the session log, or to the outbox if the store is
    /// unreachable. Trims the log to the last turns every ten turns.
    pub(crate) async fn append(
        &self,
        agent: &AgentId,
        session: Option<SessionId>,
        body: SessionEventBody,
    ) {
        let is_turn = matches!(body, SessionEventBody::TurnStart);
        let ev = self.event_record(agent, session, body);
        match self.inner.store.append(agent, &ev).await {
            Ok(_) => {
                if is_turn {
                    let due = {
                        let mut st = self.state();
                        let n = st.turns_since_trim.entry(agent.clone()).or_default();
                        *n += 1;
                        if *n >= 10 {
                            *n = 0;
                            true
                        } else {
                            false
                        }
                    };
                    if due {
                        self.trim(agent).await;
                    }
                }
            }
            Err(e) => {
                tracing::debug!(%agent, "session log unavailable, using the outbox: {e}");
                match serde_json::to_vec(&ev) {
                    Ok(p) => {
                        if let Err(e) =
                            self.inner
                                .outbox
                                .push(OutboxKind::Append, agent.as_str(), &p)
                        {
                            tracing::error!(%agent, "outbox write failed: {e}");
                        }
                    }
                    Err(e) => tracing::error!("encode session event: {e}"),
                }
            }
        }
    }

    async fn trim(&self, agent: &AgentId) {
        let Ok(events) = self.inner.store.read(agent).await else {
            return;
        };
        if let Some(seq) = session_log::tail_start(&events, self.inner.cfg.keep_turns)
            && let Err(e) = self.inner.store.trim_before(agent, seq).await
        {
            tracing::warn!(%agent, "trim failed: {e}");
        }
    }

    /// Sends the oldest outbox items until one fails.
    pub async fn flush_outbox(&self) {
        let Ok(items) = self.inner.outbox.peek(50) else {
            return;
        };
        for item in items {
            let ok = match item.kind {
                OutboxKind::Publish => match serde_json::from_slice::<Envelope>(&item.payload) {
                    Ok(env) => self.inner.bus.publish(&item.target, &env).await.is_ok(),
                    Err(_) => true,
                },
                OutboxKind::Append => match (
                    item.target.parse::<AgentId>(),
                    serde_json::from_slice::<SessionEvent>(&item.payload),
                ) {
                    (Ok(agent), Ok(ev)) => self.inner.store.append(&agent, &ev).await.is_ok(),
                    _ => true,
                },
            };
            if !ok {
                return;
            }
            if let Err(e) = self.inner.outbox.remove(item.id) {
                tracing::error!("outbox remove failed: {e}");
                return;
            }
        }
    }

    pub(crate) async fn notify_loop(self, shutdown: CancellationToken) {
        loop {
            let mut events = match self.inner.bus.subscribe(subjects::ALL_TASKS).await {
                Ok(s) => s,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            loop {
                let next = tokio::select! {
                    _ = shutdown.cancelled() => return,
                    n = events.next() => n,
                };
                let Some(env) = next else { break };
                let Some(ev) = &env.event else { continue };
                if !matches!(ev.status, TaskStatus::NeedsHuman | TaskStatus::Stuck) {
                    continue;
                }
                if let Some(n) = &self.inner.notifier {
                    let title = format!("Borant #{}: task needs attention", ev.channel);
                    let _ = n.notify(&title, &env.text).await;
                }
            }
        }
    }
}
