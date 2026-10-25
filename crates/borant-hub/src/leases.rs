//! Hosts, sessions, leases, and takeovers (spec sections 7, 11, 13).

use borant_core::model::{Agent, Harness, HostInfo, Lease, LeaseState, MoveReason, Session};
use borant_core::proto::{
    ApiError, ApiErrorCode, HeartbeatReply, RegisterSession, Registered, RenewedLease,
    SessionLeaseRef,
};
use borant_core::rules::{lease, session_log, wake};
use borant_core::{AgentId, FencingToken, HostName, SessionId, keys};
use serde_json::{Value, json};

use crate::store::Doc;
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
        let mut renewed = Vec::new();
        for s in sessions {
            let key = keys::lease_agent(&s.agent);
            let Some(doc) = self.store.get::<Lease>(&key).await? else {
                lost.push(s.session);
                continue;
            };
            if let Ok(l) = lease::renew(&doc.value, s.session, s.token, now, &self.cfg.lease) {
                self.store.put(&key, &l, Some(doc.revision)).await?;
                continue;
            }
            // A sleep or a network drop expired the lease, but nobody took
            // the agent, so the session continues with the raised token.
            let Some(l) =
                lease::reclaim(&doc.value, s.session, s.token, host, now, &self.cfg.lease)
            else {
                lost.push(s.session);
                continue;
            };
            self.store.put(&key, &l, Some(doc.revision)).await?;
            if let Ok(mut a) = self.agent(&s.agent).await
                && a.value.session != Some(s.session)
            {
                a.value.session = Some(s.session);
                self.store.put(&a.key, &a.value, Some(a.revision)).await?;
            }
            mem.expired_at.remove(&s.agent);
            mem.claims_released.remove(&s.agent);
            mem.idle_checked.remove(&s.agent);
            mem.wake_checked.remove(&s.agent);
            tracing::info!(agent = %s.agent, session = %s.session, token = l.token.0, "lease reclaimed after expiry");
            renewed.push(RenewedLease {
                session: s.session,
                token: l.token,
            });
        }
        Ok(HeartbeatReply { lost, renewed })
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
                    model: None,
                    model_harness: None,
                    project: None,
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
        mem.expired_at.remove(&agent_id);
        mem.claims_released.remove(&agent_id);
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
        let mut released = false;
        if let Some(doc) = self.store.get::<Lease>(&key).await?
            && matches!(&doc.value.state, LeaseState::Held { session: s, .. } if *s == session)
            && doc.value.token == token
        {
            released = true;
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
        // A stale end, for example from a host that lost the lease, must not
        // start a move.
        if released && let Some(p) = mem.pending_moves.get(agent).cloned() {
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
        // Hosts must send a heartbeat before the hub places agents again, so
        // a host that died while the hub was down is not chosen.
        self.mem.lock().await.started = Some(now);
        Ok(())
    }

    /// Runs one lease at a time and takes the memory lock for each step, not
    /// for the whole pass. A request is served between steps, so the catch-up
    /// after a restart on a large store does not stall `handle`.
    pub(crate) async fn tick_leases(&self, leases: Vec<Doc<Lease>>) -> Result<(), ApiError> {
        for doc in leases {
            let mut mem = self.mem.lock().await;
            if mem.maintenance {
                return Ok(());
            }
            match self.tick_one(&mut mem, doc).await {
                // A request changed this lease between the list and the step,
                // so the compare-and-set write failed. The next tick retries.
                Err(e) if e.code == ApiErrorCode::Conflict => {
                    tracing::debug!("lease changed during the tick: {e}");
                }
                other => other?,
            }
        }
        Ok(())
    }

    /// The listed lease, if no request changed it since the list.
    async fn unchanged_lease(&self, doc: &Doc<Lease>) -> Result<Option<Doc<Lease>>, ApiError> {
        Ok(self
            .store
            .get::<Lease>(&doc.key)
            .await?
            .filter(|fresh| fresh.revision == doc.revision))
    }

    async fn tick_one(&self, mem: &mut Memory, doc: Doc<Lease>) -> Result<(), ApiError> {
        let now = self.now();
        let cfg = self.cfg.lease;
        let quiet = mem.started.is_some_and(|t| now.since(t) < cfg.lease);
        let agent_id = doc.value.agent.clone();
        if let Some(mut next) = lease::tick(&doc.value, now, &cfg) {
            let was_held = matches!(doc.value.state, LeaseState::Held { .. });
            if matches!(doc.value.state, LeaseState::Reserved { .. }) {
                // A launch did not register in time. Place again at once.
                next.state = LeaseState::Expired {
                    since: now.minus(cfg.grace),
                    last_session: None,
                };
                tracing::warn!(agent = %agent_id, "launch reservation ended without a session");
            }
            self.store.put(&doc.key, &next, Some(doc.revision)).await?;
            if was_held {
                mem.expired_at.entry(agent_id.clone()).or_insert(now);
                if let Ok(mut a) = self.agent(&agent_id).await {
                    a.value.session = None;
                    self.store.put(&a.key, &a.value, Some(a.revision)).await?;
                    tracing::info!(agent = %a.value.id, "lease expired");
                }
            }
            return Ok(());
        }
        // The lease was listed before this step took the mutex. A session
        // that registered in between holds the lease now and must keep its
        // claims, so act only on a listing that is still current. The
        // writes above are safe already: their compare-and-set fails.
        let Some(doc) = self.unchanged_lease(&doc).await? else {
            return Ok(());
        };
        let since = match doc.value.state {
            LeaseState::Expired { since, .. } | LeaseState::Released { since } => since,
            _ => return Ok(()),
        };
        let Ok(agent) = self.agent(&agent_id).await else {
            return Ok(());
        };
        let agent = agent.value;

        // Claims return to open when no session held the agent for the
        // claim timeout, measured from the first expiry.
        let base = mem.expired_at.get(&agent_id).copied().unwrap_or(since);
        if now.since(base) >= cfg.claim_timeout && mem.claims_released.insert(agent_id.clone()) {
            let n = self.unclaim_everywhere(&agent_id).await?;
            if n > 0 {
                tracing::warn!(agent = %agent_id, n, "claims returned to open after the claim timeout");
            }
        }

        let expired = matches!(doc.value.state, LeaseState::Expired { .. });
        let pending = mem.pending_moves.get(&agent_id).cloned();
        if quiet || now.since(since) < cfg.grace {
            return Ok(());
        }
        if let Some(p) = pending {
            // A spawn or a planned move that is not placed yet.
            self.place_agent(mem, &agent, p.reason, p.target, p.prompt)
                .await?;
            return Ok(());
        }
        if !expired || !agent.movable || agent.channels.is_empty() {
            return Ok(());
        }
        let mut needs = !mem.claims_released.contains(&agent_id)
            && self.has_claims(&agent_id, &agent.channels).await?;
        if !needs && self.bus.inbox_pending(&agent_id).await.unwrap_or(0) > 0 {
            let due = mem
                .wake_checked
                .get(&agent_id)
                .is_none_or(|t| now.since(*t) >= WAKE_CHECK_EVERY);
            if due {
                mem.wake_checked.insert(agent_id.clone(), now);
                needs = self
                    .bus
                    .unacked(&agent_id)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|e| wake::should_wake(e, &agent_id));
            }
        }
        if !needs && mem.idle_checked.insert(agent_id.clone()) {
            let events: Vec<_> = self
                .sessions
                .read(&agent_id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(_, e)| e)
                .collect();
            needs = !session_log::undelivered(&events).is_empty();
            if needs {
                mem.idle_checked.remove(&agent_id);
            }
        }
        if needs {
            self.place_agent(mem, &agent, MoveReason::Takeover, Default::default(), None)
                .await?;
        }
        Ok(())
    }
}
