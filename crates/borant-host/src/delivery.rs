//! The inbox of each local agent, delivery into sessions, local calls, the
//! session log, and the outbox.

use std::sync::atomic::Ordering;
use std::time::Duration;

use borant_core::events::{SwarmEvent, SwarmEventBody};
use borant_core::model::{
    Envelope, EnvelopeKind, Sender, SessionEvent, SessionEventBody, Target, TaskStatus,
};
use borant_core::ports::{Delivery, Notice, PortErrorKind};
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
        let mut failing = false;
        loop {
            // A paused or fenced session must not take messages: after a
            // takeover, the new session owns the inbox.
            let blocked = self
                .state()
                .sessions
                .get(&id)
                .is_none_or(|s| s.paused || s.fenced.is_some());
            if blocked {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
                }
            }
            let pulled = tokio::select! {
                _ = cancel.cancelled() => return,
                r = self.inner.bus.pull(&agent, 16, Duration::from_secs(5)) => r,
            };
            match pulled {
                Ok(batch) => {
                    if failing {
                        failing = false;
                        tracing::info!(%agent, "inbox pull recovered");
                    }
                    for d in batch {
                        self.receive(&agent, id, d).await;
                    }
                }
                Err(e) => {
                    if failing {
                        tracing::debug!(%agent, "inbox pull still failing: {e}");
                    } else {
                        failing = true;
                        tracing::warn!(%agent, "inbox pull failed: {e}");
                    }
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
        // A pull that started before a pause or a fence must not take the
        // message. Without an ack, the bus delivers it again later.
        let blocked = self
            .state()
            .sessions
            .get(&id)
            .is_none_or(|s| s.paused || s.fenced.is_some());
        if blocked {
            return;
        }
        let env = d.envelope;
        let own_broadcast =
            env.from == Sender::Agent(agent.clone()) && env.kind != EnvelopeKind::Direct;
        let seen = self
            .state()
            .seen
            .get(agent)
            .is_some_and(|s| s.contains(&env.id));
        if seen || own_broadcast {
            if let Err(e) = d.acker.ack().await {
                tracing::warn!(%agent, "ack failed, the message can arrive again: {e}");
                self.delivery_event(agent, "delivery.ack_failed", |count| {
                    SwarmEventBody::DeliveryAckFailed {
                        message: env.id,
                        error: e.to_string(),
                        count,
                    }
                })
                .await;
            }
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
            tracing::warn!(%agent, "cannot log message yet: {e}");
            self.delivery_event(agent, "delivery.append_failed", |count| {
                SwarmEventBody::DeliveryAppendFailed {
                    message: env.id,
                    error: e.to_string(),
                    count,
                }
            })
            .await;
            return;
        }
        if let Err(e) = d.acker.ack().await {
            tracing::warn!(%agent, "ack failed, the message can arrive again: {e}");
            self.delivery_event(agent, "delivery.ack_failed", |count| {
                SwarmEventBody::DeliveryAckFailed {
                    message: env.id,
                    error: e.to_string(),
                    count,
                }
            })
            .await;
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
            self.delivery_event(agent, "delivery.wake_limited", |count| {
                SwarmEventBody::DeliveryWakeLimited {
                    message: env.id,
                    count,
                }
            })
            .await;
            self.wake_limit_notice(agent, sender.as_ref()).await;
        }
        tracing::debug!(%agent, id = %env.id, wake, "message queued");
    }

    /// Tells the sessions that a pause caught, and whose tools work again,
    /// that the hub is back. The message goes to the agent inbox, so the
    /// inbox loop logs and delivers it like any received message, and an idle
    /// session wakes.
    pub(crate) async fn wake_unpaused(&self) {
        const TEXT: &str = "Borant is back. Your tools work again. Continue your task.";
        let targets: Vec<(SessionId, AgentId)> = {
            let mut st = self.state();
            let mut targets = Vec::new();
            for s in st.sessions.values_mut() {
                if !s.wake_pending || s.paused {
                    continue;
                }
                if s.fenced.is_some() {
                    // The harness is told that it lost its lease instead.
                    s.wake_pending = false;
                    continue;
                }
                if let Some(reg) = &s.reg {
                    targets.push((s.id, reg.agent.clone()));
                }
            }
            targets
        };
        for (id, agent) in targets {
            let env = Envelope {
                id: MessageId::new(),
                from: Sender::Human,
                to: Target::Agent(agent.clone()),
                kind: EnvelopeKind::Direct,
                text: TEXT.to_owned(),
                urgent: false,
                event: None,
                sent: self.inner.clock.now(),
            };
            match self.inner.bus.publish(&subjects::agent(&agent), &env).await {
                Ok(()) => {
                    if let Some(s) = self.state().sessions.get_mut(&id) {
                        s.wake_pending = false;
                    }
                }
                Err(e) => {
                    tracing::warn!(%agent, "cannot send the hub-is-back message yet: {e}");
                }
            }
        }
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

    /// Waits until a message that wakes the session is queued. Returns false
    /// at the timeout, or at once when the session is fenced or paused.
    async fn wait_ready(&self, id: SessionId, timeout: Duration) -> Result<bool, ApiError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notify = {
                let st = self.state();
                let s = st.sessions.get(&id).ok_or_else(|| {
                    crate::persist::no_session(st.restored, format!("no local session {id}"))
                })?;
                s.notify.clone()
            };
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let st = self.state();
                let Some(s) = st.sessions.get(&id) else {
                    return Ok(false);
                };
                if s.fenced.is_some() || s.paused {
                    return Ok(false);
                }
                if s.queue.iter().any(|q| q.wake) {
                    return Ok(true);
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep_until(deadline) => return Ok(false),
            }
        }
    }

    /// Waits until a message that wakes the session is queued, then returns
    /// it with every other queued message. Returns empty at the timeout, or
    /// at once when the session is fenced or paused.
    pub async fn wait(&self, id: SessionId, timeout: Duration) -> Result<Vec<Envelope>, ApiError> {
        if self.wait_ready(id, timeout).await? {
            Ok(self.drain(id).await)
        } else {
            Ok(Vec::new())
        }
    }

    /// Takes the oldest queued message and logs its delivery.
    async fn take_one(&self, id: SessionId) -> Option<Envelope> {
        let (agent, env) = {
            let mut st = self.state();
            let s = st.sessions.get_mut(&id)?;
            let env = s.queue.pop_front()?.env;
            (s.reg.as_ref().map(|r| r.agent.clone()), env)
        };
        if let Some(agent) = agent {
            self.append(
                &agent,
                Some(id),
                SessionEventBody::Delivered { message: env.id },
            )
            .await;
        }
        Some(env)
    }

    /// Messages for a session as they arrive, for the Pi and omp extension.
    /// When a message that wakes the session arrives, the stream sends it with
    /// every other queued message. Each message is logged as delivered only
    /// when the stream sends it, so a dropped connection loses nothing.
    pub fn stream(&self, id: SessionId) -> BoxStream<'static, Envelope> {
        let host = self.clone();
        futures::stream::unfold((host, false), move |(host, mut draining)| async move {
            loop {
                if draining {
                    if let Some(e) = host.take_one(id).await {
                        return Some((e, (host, true)));
                    }
                    draining = false;
                }
                match host.wait_ready(id, Duration::from_secs(30)).await {
                    Ok(true) => draining = true,
                    Ok(false) => {
                        if !host.state().sessions.contains_key(&id) {
                            return None;
                        }
                        if host.fence_reason(id).is_some() {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                    Err(_) => return None,
                }
            }
        })
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
                let s = st.sessions.get(&id).ok_or_else(|| {
                    crate::persist::no_session(st.restored, format!("no local session {id}"))
                })?;
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
                // A session that writes an event is not stalled.
                if let Some(id) = session
                    && let Some(s) = self.state().sessions.get_mut(&id)
                {
                    s.last_event_at = Some(ev.at);
                }
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
                tracing::warn!(%agent, "session log unavailable, using the outbox: {e}");
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
                    Ok(env) => match self.inner.bus.publish(&item.target, &env).await {
                        Ok(()) => true,
                        Err(e) => {
                            if self.outbox_failing_now() {
                                tracing::warn!(
                                    subject = %item.target,
                                    "outbox publish failed, will retry: {e}"
                                );
                            } else {
                                tracing::debug!(
                                    subject = %item.target,
                                    "outbox publish failed, will retry: {e}"
                                );
                            }
                            false
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            subject = %item.target,
                            "drop undecodable outbox envelope: {e}"
                        );
                        true
                    }
                },
                OutboxKind::Event => match serde_json::from_slice::<SwarmEvent>(&item.payload) {
                    Ok(event) => match &self.inner.events {
                        Some(log) => match log.publish(&event).await {
                            Ok(()) => true,
                            Err(e) => {
                                if self.outbox_failing_now() {
                                    tracing::warn!(
                                        kind = %item.target,
                                        "outbox event failed, will retry: {e}"
                                    );
                                } else {
                                    tracing::debug!(
                                        kind = %item.target,
                                        "outbox event failed, will retry: {e}"
                                    );
                                }
                                false
                            }
                        },
                        None => {
                            tracing::debug!(
                                kind = %item.target,
                                "drop an outbox event: the daemon has no event log"
                            );
                            true
                        }
                    },
                    Err(e) => {
                        tracing::warn!(kind = %item.target, "drop undecodable outbox event: {e}");
                        true
                    }
                },
                OutboxKind::Append => {
                    let agent = item.target.parse::<AgentId>();
                    let ev = serde_json::from_slice::<SessionEvent>(&item.payload);
                    match (agent, ev) {
                        (Ok(agent), Ok(ev)) => match self.inner.store.append(&agent, &ev).await {
                            Ok(_) => true,
                            Err(e) => {
                                if self.outbox_failing_now() {
                                    tracing::warn!(
                                        %agent,
                                        "outbox append failed, will retry: {e}"
                                    );
                                } else {
                                    tracing::debug!(
                                        %agent,
                                        "outbox append failed, will retry: {e}"
                                    );
                                }
                                false
                            }
                        },
                        (Err(e), _) => {
                            tracing::warn!(
                                target = %item.target,
                                "drop outbox item with a bad agent id: {e}"
                            );
                            true
                        }
                        (_, Err(e)) => {
                            tracing::warn!(
                                target = %item.target,
                                "drop undecodable outbox event: {e}"
                            );
                            true
                        }
                    }
                }
            };
            if !ok {
                return;
            }
            if let Err(e) = self.inner.outbox.remove(item.id) {
                tracing::error!("outbox remove failed: {e}");
                return;
            }
        }
        if self.inner.outbox_failing.swap(false, Ordering::Relaxed) {
            tracing::info!("outbox flush recovered");
        }
    }

    /// True on the first failure of a run, and marks the run as failing.
    fn outbox_failing_now(&self) -> bool {
        !self.inner.outbox_failing.swap(true, Ordering::Relaxed)
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
                    let notice = Notice::new(
                        format!("Borant #{}: task needs attention", ev.channel),
                        env.text.clone(),
                    );
                    let _ = n.notify(&notice).await;
                }
            }
        }
    }
}
