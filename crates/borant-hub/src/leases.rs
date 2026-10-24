//! Hosts, sessions, leases, and takeovers (spec sections 7, 11, 13).

use borant_core::model::{Agent, Harness, HostInfo, Lease, LeaseState, MoveReason, Session};
use borant_core::proto::{ApiError, HeartbeatReply, RegisterSession, Registered, SessionLeaseRef};
use borant_core::rules::{lease, session_log, wake};
use borant_core::{AgentId, FencingToken, HostName, SessionId, keys};
use serde_json::{Value, json};

use crate::{Hub, Memory};

/// How often the hub reads the pending messages of an idle expired agent.
const WAKE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

fn short(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => "claude",
        other => other.as_str(),
    }
}

fn letters() -> String {
    let mut n = ulid::Ulid::generate().random();
    (0..4)
        .map(|_| {
            let c = (b'a' + (n % 26) as u8) as char;
            n /= 26;
            c
        })
        .collect()
}

impl Hub {
    pub(crate) async fn register_host(
        &self,
        mem: &mut Memory,
        mut info: HostInfo,
    ) -> Result<Value, ApiError> {
        let key = keys::host(&info.name);
        if let Some(old) = self.store.get::<HostInfo>(&key).await? {
            info.draining = old.value.draining;
        }
        self.store.upsert(&key, &info).await?;
        mem.hosts_seen.insert(info.name.clone(), self.now());
        Ok(json!({ "host": info.name }))
    }

    pub(crate) async fn heartbeat(
        &self,
        mem: &mut Memory,
        host: &HostName,
        sessions: &[SessionLeaseRef],
    ) -> Result<HeartbeatReply, ApiError> {
        let now = self.now();
        mem.hosts_seen.insert(host.clone(), now);
        let mut lost = Vec::new();
        for s in sessions {
            let key = keys::lease_agent(&s.agent);
            let Some(doc) = self.store.get::<Lease>(&key).await? else {
                lost.push(s.session);
                continue;
            };
            match lease::renew(&doc.value, s.session, s.token, now, &self.cfg.lease) {
                Ok(l) => {
                    self.store.put(&key, &l, Some(doc.revision)).await?;
                }
                Err(_) => lost.push(s.session),
            }
        }
        Ok(HeartbeatReply { lost })
    }

    pub(crate) async fn new_agent_id(&self, harness: Harness) -> Result<AgentId, ApiError> {
        for _ in 0..20 {
            let id: AgentId = format!("{}-{}", short(harness), letters())
                .parse()
                .map_err(|e: borant_core::CoreError| ApiError::internal(e.to_string()))?;
            if self.store.get::<Agent>(&keys::agent(&id)).await?.is_none() {
                return Ok(id);
            }
        }
        Err(ApiError::internal("could not pick a free agent ID"))
    }

    pub(crate) async fn register_session(
        &self,
        mem: &mut Memory,
        r: RegisterSession,
    ) -> Result<Registered, ApiError> {
        let now = self.now();
        let agent_id = match (&r.agent, &r.harness_session_id) {
            (Some(a), _) => a.clone(),
            (None, Some(hsid)) => {
                let found = self
                    .store
                    .list::<Agent>(keys::AGENT_PREFIX)
                    .await?
                    .into_iter()
                    .find(|d| {
                        d.value.harness_session_id.as_ref() == Some(hsid)
                            && d.value.host.as_ref() == Some(&r.host)
                    });
                match found {
                    Some(d) => d.value.id,
                    None => self.new_agent_id(r.harness).await?,
                }
            }
            (None, None) => self.new_agent_id(r.harness).await?,
        };

        let agent_key = keys::agent(&agent_id);
        let existing = self.store.get::<Agent>(&agent_key).await?;
        let lease_key = keys::lease_agent(&agent_id);
        let lease_doc = self.store.get::<Lease>(&lease_key).await?;
        let new_lease = lease::acquire(
            &agent_id,
            lease_doc.as_ref().map(|d| &d.value),
            r.session,
            &r.host,
            now,
            &self.cfg.lease,
        )?;
        self.store
            .put(
                &lease_key,
                &new_lease,
                lease_doc.as_ref().map(|d| d.revision),
            )
            .await?;

        let (mut agent, rev) = match existing {
            Some(d) => (d.value, Some(d.revision)),
            None => (
                Agent {
                    id: agent_id.clone(),
                    preferred_harness: r.harness,
                    repo: None,
                    movable: false,
                    channels: Vec::new(),
                    session: None,
                    host: None,
                    harness_session_id: None,
                    created: now,
                },
                None,
            ),
        };
        if r.repo.is_some() {
            agent.repo = r.repo.clone();
        }
        agent.movable = agent.repo.is_some() && !r.shared_dir;
        agent.session = Some(r.session);
        agent.host = Some(r.host.clone());
        agent.harness_session_id = r.harness_session_id.clone();
        agent.preferred_harness = r.harness;
        self.store.put(&agent_key, &agent, rev).await?;

        let session = Session {
            id: r.session,
            agent: agent_id.clone(),
            harness: r.harness,
            harness_session_id: r.harness_session_id,
            host: r.host,
            cwd: r.cwd,
            pid: r.pid,
            started: now,
            ended: None,
        };
        self.store
            .upsert(&keys::session(&r.session), &session)
            .await?;

        if let Some(c) = &r.channel {
            if self.channel(c).await.is_ok() {
                self.join(&agent_id, c).await?;
            } else {
                tracing::warn!(agent = %agent_id, channel = %c, "BORANT_CHANNEL names a missing channel");
            }
        }
        let agent = self.agent(&agent_id).await?.value;
        self.refresh_inbox(&agent).await?;

        mem.failed_hosts.remove(&agent_id);
        mem.idle_checked.remove(&agent_id);
        mem.wake_checked.remove(&agent_id);
        mem.pending_moves.remove(&agent_id);
        tracing::info!(agent = %agent_id, session = %r.session, token = new_lease.token.0, "session registered");
        Ok(Registered {
            agent: agent_id,
            session: r.session,
            token: new_lease.token,
            movable: agent.movable,
            channels: agent.channels,
        })
    }

    pub(crate) async fn end_session(
        &self,
        mem: &mut Memory,
        session: SessionId,
        agent: &AgentId,
        token: FencingToken,
        reason: &str,
    ) -> Result<(), ApiError> {
        let now = self.now();
        let key = keys::lease_agent(agent);
        if let Some(doc) = self.store.get::<Lease>(&key).await?
            && matches!(&doc.value.state, LeaseState::Held { session: s, .. } if *s == session)
            && doc.value.token == token
        {
            self.store
                .put(&key, &lease::release(&doc.value, now), Some(doc.revision))
                .await?;
            let mut a = self.agent(agent).await?;
            a.value.session = None;
            self.store.put(&a.key, &a.value, Some(a.revision)).await?;
        }
        if let Some(mut s) = self.store.get::<Session>(&keys::session(&session)).await? {
            s.value.ended = Some(now);
            self.store.put(&s.key, &s.value, Some(s.revision)).await?;
        }
        tracing::info!(%agent, %session, reason, "session ended");
        if let Some(p) = mem.pending_moves.get(agent).cloned() {
            let a = self.agent(agent).await?.value;
            self.place_agent(mem, &a, p.reason, p.target, p.prompt)
                .await?;
        }
        Ok(())
    }

    /// Gives every held lease a full lease time, and marks every known host
    /// as seen now, so a hub restart causes no takeover.
    pub async fn restart_grace(&self) -> Result<(), ApiError> {
        let now = self.now();
        for doc in self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await? {
            let l = lease::restart_grace(&doc.value, now, &self.cfg.lease);
            if l != doc.value {
                self.store.put(&doc.key, &l, Some(doc.revision)).await?;
            }
        }
        let mut mem = self.mem.lock().await;
        for h in self.store.list::<HostInfo>(keys::HOST_PREFIX).await? {
            mem.hosts_seen.insert(h.value.name, now);
        }
        Ok(())
    }

    pub(crate) async fn tick_leases(&self, mem: &mut Memory) -> Result<(), ApiError> {
        let now = self.now();
        let cfg = self.cfg.lease;
        for doc in self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await? {
            if let Some(mut next) = lease::tick(&doc.value, now, &cfg) {
                let was_held = matches!(doc.value.state, LeaseState::Held { .. });
                if matches!(doc.value.state, LeaseState::Reserved { .. }) {
                    // A launch did not register in time. Place again at once.
                    next.state = LeaseState::Expired {
                        since: now.minus(cfg.grace),
                    };
                    tracing::warn!(agent = %doc.value.agent, "launch reservation ended without a session");
                }
                self.store.put(&doc.key, &next, Some(doc.revision)).await?;
                if was_held && let Ok(mut a) = self.agent(&doc.value.agent).await {
                    a.value.session = None;
                    self.store.put(&a.key, &a.value, Some(a.revision)).await?;
                    tracing::info!(agent = %a.value.id, "lease expired");
                }
                continue;
            }
            let LeaseState::Expired { since } = doc.value.state else {
                continue;
            };
            let idle = now.since(since);
            if idle < cfg.grace {
                continue;
            }
            let Ok(agent) = self.agent(&doc.value.agent).await else {
                continue;
            };
            let agent = agent.value;
            if !agent.movable || agent.channels.is_empty() {
                continue;
            }
            let mut claims = self.has_claims(&agent.id, &agent.channels).await?;
            if claims && idle >= cfg.claim_timeout {
                let n = self.unclaim_all(&agent.id, &agent.channels).await?;
                tracing::warn!(agent = %agent.id, n, "claims returned to open after the claim timeout");
                claims = false;
            }
            let mut needs = claims || mem.pending_moves.contains_key(&agent.id);
            if !needs && self.bus.inbox_pending(&agent.id).await.unwrap_or(0) > 0 {
                let due = mem
                    .wake_checked
                    .get(&agent.id)
                    .is_none_or(|t| now.since(*t) >= WAKE_CHECK_EVERY);
                if due {
                    mem.wake_checked.insert(agent.id.clone(), now);
                    needs = self
                        .bus
                        .unacked(&agent.id)
                        .await
                        .unwrap_or_default()
                        .iter()
                        .any(|e| wake::should_wake(e, &agent.id));
                }
            }
            if !needs && mem.idle_checked.insert(agent.id.clone()) {
                let events: Vec<_> = self
                    .sessions
                    .read(&agent.id)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, e)| e)
                    .collect();
                needs = !session_log::undelivered(&events).is_empty();
                if needs {
                    mem.idle_checked.remove(&agent.id);
                }
            }
            if needs {
                let (target, prompt) = match mem.pending_moves.get(&agent.id) {
                    Some(p) => (p.target.clone(), p.prompt.clone()),
                    None => (Default::default(), None),
                };
                self.place_agent(mem, &agent, MoveReason::Takeover, target, prompt)
                    .await?;
            }
        }
        Ok(())
    }
}
