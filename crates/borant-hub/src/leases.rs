//! Hosts, sessions, leases, and takeovers (spec sections 7, 11, 13).

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    Agent, Harness, HostInfo, Lease, LeaseState, MAX_SESSION_RECOVERIES, MoveReason, Session,
    SpawnRecord, SpawnState, Task, TaskEventKind, TaskStatus,
};
use borant_core::ports::{Notice, Priority};
use borant_core::proto::{
    ApiError, ApiErrorCode, HeartbeatReply, MoveTarget, RegisterSession, Registered, RenewedLease,
    SessionLeaseRef,
};
use borant_core::rules::roles::OnePer;
use borant_core::rules::{lease, schedule, session_log, wake};
use borant_core::{
    AgentId, ChannelName, CollectionName, FencingToken, HostName, ProjectName, RoleName, SessionId,
    Timestamp, keys,
};
use serde_json::{Value, json};

use crate::store::Doc;
use crate::{ActivityReport, HostSessions, Hub, IdleReport, LeaseGate, Lists, Memory, PendingMove};

/// What the lease pass leaves behind for a lease that it did not write: when
/// to look at it again, or that nothing can happen for it before its revision
/// changes or a message waits for its agent.
#[derive(Debug, Clone, Copy)]
enum Rest {
    At(Timestamp),
    Settled,
}

impl Rest {
    fn at(self) -> Option<Timestamp> {
        match self {
            Rest::At(at) => Some(at),
            Rest::Settled => None,
        }
    }
}

/// How long a lease that needs nothing is left alone before the pass looks
/// again, so a wake that no message announced still comes.
const SETTLED_POLL: std::time::Duration = std::time::Duration::from_secs(60);

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

/// True when the session of a pinned `agent` is dead, so it holds nothing:
/// its pinned host sent a heartbeat after the lease expired that leaves the
/// session out, or no session held the agent for the claim timeout. A held
/// lease, a reservation, and a session that its host still reports keep the
/// agent, and an agent with no host has no place to come back on.
pub(crate) fn pinned_session_dead(
    mem: &Memory,
    agent: &Agent,
    lease: &Lease,
    now: Timestamp,
    cfg: &lease::LeaseConfig,
) -> bool {
    let LeaseState::Expired {
        since,
        last_session: Some(last),
    } = lease.state
    else {
        return false;
    };
    let Some(host) = &agent.host else {
        return false;
    };
    // Nothing held the agent for the claim timeout, so no session can be
    // waiting on the other side of a lost heartbeat.
    if now.since(since) >= cfg.claim_timeout {
        return true;
    }
    mem.host_sessions
        .get(host)
        .is_some_and(|h| h.at > since && !h.sessions.contains(&last))
}

impl Hub {
    /// Records that the hub heard `host` at `now`. A gap of `hung_minutes`
    /// since the last time is a sleep, not a slow beat: the host and its
    /// sessions lost that time, and the ones that say nothing after it are
    /// hung until they move again.
    ///
    /// A host that slept usually sends its registration before its first
    /// beat, so the check runs on a registration too; a plain insert there
    /// would hide the gap from the beat that follows it.
    fn note_host_seen(&self, mem: &mut Memory, host: &HostName, now: Timestamp) {
        let slept_gap =
            std::time::Duration::from_secs(self.cfg.alerts.hung_minutes.saturating_mul(60));
        if mem
            .hosts_seen
            .insert(host.clone(), now)
            .is_some_and(|last| now.since(last) >= slept_gap)
        {
            mem.hosts_slept.insert(host.clone(), now);
            tracing::info!(%host, "the host slept and its sessions lost that time");
        }
    }

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
        let now = self.now();
        self.note_host_seen(mem, &info.name, now);
        // The agents of the host come from the lease list of the last tick,
        // so a registration reads no lease: a host that retries its
        // registration does not read the whole store each time.
        let agents = mem.leases_on.get(&info.name).copied().unwrap_or(0);
        let event = self
            .event(
                now,
                Severity::Info,
                SwarmEventBody::HostRegistered {
                    last_seen: now,
                    always_on: info.always_on,
                    agents,
                },
            )
            .about_host(info.name.clone());
        Self::queue(mem, event);
        Ok(json!({ "host": info.name }))
    }

    /// A pin that a lost hub record dropped stays: the session of a pinned
    /// agent reports it on every heartbeat.
    async fn assert_pinned(&self, s: &SessionLeaseRef) -> Result<(), ApiError> {
        if !s.pinned {
            return Ok(());
        }
        let Ok(mut a) = self.agent(&s.agent).await else {
            return Ok(());
        };
        if !a.value.pinned {
            a.value.pinned = true;
            self.store.put(&a.key, &a.value, Some(a.revision)).await?;
            tracing::info!(agent = %s.agent, "the agent is pinned again");
        }
        Ok(())
    }

    pub(crate) async fn heartbeat(
        &self,
        mem: &mut Memory,
        host: &HostName,
        sessions: &[SessionLeaseRef],
        launching: &[AgentId],
    ) -> Result<HeartbeatReply, ApiError> {
        let now = self.now();
        self.note_host_seen(mem, host, now);
        mem.host_sessions.insert(
            host.clone(),
            HostSessions {
                at: now,
                sessions: sessions.iter().map(|s| s.session).collect(),
            },
        );
        for s in sessions {
            mem.idle.insert(
                s.agent.clone(),
                IdleReport {
                    session: s.session,
                    host: host.clone(),
                    since: s.idle_since,
                    at: now,
                },
            );
            // The activity arrives at most each 30 s, so a beat without one
            // keeps the last report. The session of the report must match the
            // live session of the agent, or the live view shows none.
            if let Some(activity) = &s.activity {
                mem.activity.insert(
                    s.agent.clone(),
                    ActivityReport {
                        session: s.session,
                        activity: activity.clone(),
                    },
                );
            }
        }
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
                self.assert_pinned(s).await?;
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
            self.assert_pinned(s).await?;
            // The beats that a host queued while its connection was down all
            // land when it reconnects, and a lease that this session already
            // took back gives the same token again: that is one reclaim, not
            // one per beat, so the later beats renew it and neither log nor
            // queue a second event.
            let already_ours = matches!(
                &doc.value.state,
                LeaseState::Held { session: held, host: held_host, .. }
                    if *held == s.session && held_host == host
            );
            if !already_ours {
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
                // A sweep may have ended this session while the host was asleep;
                // the reclaim says it still runs, so the spawn is running again,
                // the session is not ended, and a recovery that waited for the
                // session is unnecessary.
                self.mark_spawn_running(&s.agent).await?;
                if let Ok(Some(mut session)) =
                    self.store.get::<Session>(&keys::session(&s.session)).await
                    && session.value.ended.is_some()
                {
                    session.value.ended = None;
                    self.store
                        .put(&session.key, &session.value, Some(session.revision))
                        .await?;
                }
                if mem
                    .pending_moves
                    .get(&s.agent)
                    .is_some_and(|p| p.reason == MoveReason::Recover)
                {
                    mem.pending_moves.remove(&s.agent);
                }
                tracing::info!(agent = %s.agent, session = %s.session, token = l.token.0, "lease reclaimed after expiry");
                let event = self.lease_event(
                    now,
                    Severity::Info,
                    &s.agent,
                    Some(host),
                    SwarmEventBody::LeaseReclaimed {
                        session: s.session,
                        token: l.token,
                    },
                );
                Self::queue(mem, event);
            }
            renewed.push(RenewedLease {
                session: s.session,
                token: l.token,
            });
        }
        // A retired agent stops at the turn end that this heartbeat reports.
        self.stop_retired(mem, host, sessions).await?;
        // A paused agent stops at the turn end that this heartbeat reports.
        self.stop_paused(mem, host, sessions).await?;
        // A bound worker that went idle gets its continue here, after the
        // stops, so a session on its way out is not told to go on.
        self.continue_idle(mem, sessions).await;
        // A launch that never registered stops when the hub closed its agent.
        let stop_launches = self.stop_launches(host, launching).await?;
        Ok(HeartbeatReply {
            lost,
            renewed,
            stop_launches,
        })
    }

    /// The launches of `host` that must stop now: the agent is retired or
    /// paused, its spawn failed or was cancelled, its lease is released, or
    /// another host holds the agent. A launch that its reservation or its
    /// session still holds here keeps running.
    async fn stop_launches(
        &self,
        host: &HostName,
        launching: &[AgentId],
    ) -> Result<Vec<AgentId>, ApiError> {
        let mut stop = Vec::new();
        for agent in launching {
            let closed = match self.store.get::<Agent>(&keys::agent(agent)).await? {
                None => true,
                Some(d) => d.value.retired || d.value.paused.is_some(),
            };
            if closed {
                stop.push(agent.clone());
                continue;
            }
            // A spawn that failed its launches, or that a person cancelled,
            // waits for a person: its launch does not keep the agent alive.
            if let Some(rec) = self.store.get::<SpawnRecord>(&keys::spawn(agent)).await?
                && matches!(rec.value.state, SpawnState::Failed | SpawnState::Cancelled)
            {
                stop.push(agent.clone());
                continue;
            }
            let away = match self
                .store
                .get::<Lease>(&keys::lease_agent(agent))
                .await?
                .map(|d| d.value.state)
            {
                // A released lease holds no session, so the launch is stale.
                Some(LeaseState::Released { .. }) => true,
                Some(LeaseState::Reserved { host: h, .. } | LeaseState::Held { host: h, .. }) => {
                    h != *host
                }
                // A relaunch places the agent again and stops the stale
                // launch first. No lease at all keeps the launch: a
                // reservation that the replica has not caught up with yet
                // must not stop a launch that the hub just ordered.
                Some(LeaseState::Expired { .. }) | None => false,
            };
            if away {
                stop.push(agent.clone());
            }
        }
        Ok(stop)
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

    /// True when another agent of `role` already fills its `one_per` slot in
    /// the same collection or project: a held or reserved lease, a pending
    /// spawn, or a pin.
    pub(crate) async fn role_taken(
        &self,
        role: &RoleName,
        one_per: OnePer,
        collection: Option<&CollectionName>,
        project: Option<&ProjectName>,
        except: Option<&AgentId>,
    ) -> Result<bool, ApiError> {
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            if agent.role.as_ref() != Some(role) || except == Some(&agent.id) {
                continue;
            }
            let same_scope = match one_per {
                OnePer::Collection => agent.collection.as_ref() == collection,
                OnePer::Project => agent.project.as_ref() == project,
            };
            if !same_scope {
                continue;
            }
            if agent.pinned || self.spawn_pending(&agent.id).await? {
                return Ok(true);
            }
            let lease = self
                .store
                .get::<Lease>(&keys::lease_agent(&agent.id))
                .await?;
            if lease.as_ref().is_some_and(|l| {
                matches!(
                    l.value.state,
                    LeaseState::Held { .. } | LeaseState::Reserved { .. }
                )
            }) {
                return Ok(true);
            }
        }
        Ok(false)
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
        // The auto-join of a channel of another collection is refused before
        // the lease is taken or the agent is written.
        if let Some(c) = &r.channel
            && let Ok(ch) = self.channel(c).await
        {
            let mine = existing
                .as_ref()
                .and_then(|d| d.value.collection.clone())
                .or_else(|| r.collection.clone());
            if !crate::calls::allowed(mine.as_ref(), ch.value.collection.as_ref()) {
                return Err(ApiError::denied(format!("#{c} is in another collection")));
            }
        }
        // A pinned agent keeps its session: no other session may take it while
        // the lease is held or the last session may still run. A dead session
        // is not a session, so the host that held it may register a new one.
        if let Some(d) = existing.as_ref().filter(|d| d.value.pinned) {
            let other = match lease_doc.as_ref().map(|d| &d.value.state) {
                Some(LeaseState::Held { session, .. }) => Some(*session),
                Some(LeaseState::Expired { last_session, .. }) => *last_session,
                _ => None,
            };
            let dead = d.value.host.as_ref() == Some(&r.host)
                && lease_doc.as_ref().is_some_and(|l| {
                    pinned_session_dead(mem, &d.value, &l.value, now, &self.cfg.lease)
                });
            if other.is_some_and(|s| s != r.session) && !dead {
                return Err(ApiError::invalid(format!(
                    "agent {agent_id} is pinned; only its current session can register"
                )));
            }
        }
        // Roles and collections: keep the stored ones when the session sends
        // none, refuse a change, and check one_per before the lease is taken
        // or a membership is written.
        let stored_role = existing.as_ref().and_then(|d| d.value.role.clone());
        let stored_collection = existing.as_ref().and_then(|d| d.value.collection.clone());
        if let Some(role) = &r.role
            && stored_role.as_ref().is_some_and(|stored| stored != role)
        {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "agent {agent_id} has role {}",
                    stored_role.as_ref().map(RoleName::as_str).unwrap_or("none")
                ),
            ));
        }
        if let Some(collection) = &r.collection
            && stored_collection
                .as_ref()
                .is_some_and(|stored| stored != collection)
        {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "agent {agent_id} is in collection {}",
                    stored_collection
                        .as_ref()
                        .map(CollectionName::as_str)
                        .unwrap_or("none")
                ),
            ));
        }
        let role = r.role.clone().or(stored_role);
        let collection = r.collection.clone().or(stored_collection);
        let project = match existing.as_ref().and_then(|d| d.value.project.clone()) {
            Some(p) => Some(p),
            None => match &r.channel {
                Some(c) => self.channel(c).await.ok().and_then(|ch| ch.value.project),
                None => None,
            },
        };
        if let Some(role) = &role
            && let Some(one_per) = self.cfg.roles.get(role).and_then(|c| c.one_per)
            && self
                .role_taken(
                    role,
                    one_per,
                    collection.as_ref(),
                    project.as_ref(),
                    Some(&agent_id),
                )
                .await?
        {
            let scope = match one_per {
                OnePer::Collection => "collection",
                OnePer::Project => "project",
            };
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("only one {role} per {scope}"),
            ));
        }
        // A paused agent runs no session: a stop is not lifted by a retry of
        // the harness.
        if existing.as_ref().is_some_and(|d| d.value.paused.is_some()) {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("agent {agent_id} is paused"),
            ));
        }
        // A retired agent is gone for good: a launch that outlived its
        // retirement cannot bring it back with a session.
        if existing.as_ref().is_some_and(|d| d.value.retired) {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("agent {agent_id} is retired"),
            ));
        }
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
                    spawn_key: None,
                    task: None,
                    stop_on_close: false,
                    continues: None,
                    retired: false,
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
                    members: Default::default(),
                    pinned: false,
                    role: None,
                    collection: None,
                    bundles: Vec::new(),
                    parent: None,
                    paused: None,
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
        // A role pins its agents when it says so: a chief that a person
        // starts with BORANT_ROLE=chief is the main pinned agent. An unpinned
        // or unknown role never clears a pin that a person set.
        if let Some(role) = &role
            && self.cfg.roles.get(role).is_some_and(|c| c.pinned)
        {
            agent.pinned = true;
        }
        agent.role = role;
        agent.collection = collection;
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
        mem.silence_dmed.remove(&agent_id);
        mem.resumed_by.remove(&agent_id);
        mem.recovery_stopped.remove(&agent_id);
        mem.expired_at.remove(&agent_id);
        mem.claims_released.remove(&agent_id);
        mem.idle_checked.remove(&agent_id);
        mem.wake_checked.remove(&agent_id);
        mem.pending_moves.remove(&agent_id);
        mem.park_sent.remove(&agent_id);
        mem.idle.remove(&agent_id);
        mem.activity.remove(&agent_id);
        // A new session counts its turns from zero.
        mem.turns_seen.remove(&agent_id);
        tracing::info!(agent = %agent_id, session = %r.session, token = new_lease.token.0, "session registered");
        // The cap of `max_session_hours` counts from here, and a move starts
        // a new session.
        self.store
            .upsert(&crate::pause::session_start_key(&agent_id), &now)
            .await?;
        let host = session.host.clone();
        let event = self.lease_event(
            now,
            Severity::Info,
            &agent_id,
            Some(&host),
            SwarmEventBody::LeaseAcquired {
                session: r.session,
                token: new_lease.token,
            },
        );
        Self::queue(mem, event);
        // The session of a queued spawn started: the queue is done with it.
        if let Some(mut record) = self
            .store
            .get::<SpawnRecord>(&keys::spawn(&agent_id))
            .await?
            && record.value.state != SpawnState::Running
        {
            record.value.state = SpawnState::Running;
            record.value.last_reject.clear();
            self.store
                .put(&record.key, &record.value, Some(record.revision))
                .await?;
        }
        Ok(Registered {
            agent: agent_id,
            session: r.session,
            token: new_lease.token,
            movable: agent.movable,
            channels: agent.channels,
            pinned: agent.pinned,
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
        self.end_session_inner(mem, session, agent, Some(token), reason, false)
            .await
    }

    /// The one session-end path of a host end and of the liveness sweep.
    ///
    /// A host end carries the token of the session; a sweep found the session
    /// dead and passes none. The path sets `Session.ended`, moves the spawn
    /// record to `Ended` so `ps` and `agents` stop showing it as running, and
    /// runs the pending move or the relaunch of a claim. A host end with a
    /// stale token changes only the session record: a host that lost the
    /// lease must not start a move.
    pub(crate) async fn end_session_inner(
        &self,
        mem: &mut Memory,
        session: SessionId,
        agent: &AgentId,
        token: Option<FencingToken>,
        reason: &str,
        swept: bool,
    ) -> Result<(), ApiError> {
        let now = self.now();
        let key = keys::lease_agent(agent);
        // The turn of this session is over, so a fence that waited for it has
        // nothing to fence.
        mem.pause_fences.remove(&session);
        let mut released = false;
        // An end that the host outbox replayed after an outage reaches the
        // hub after the lease expired. The expired lease still names that
        // session, so the end is real and not a stale one for a newer
        // session; the raised token must not make the hub drop it.
        let mut late = false;
        if let Some(doc) = self.store.get::<Lease>(&key).await? {
            if token.is_some_and(|token| doc.value.token == token)
                && matches!(&doc.value.state, LeaseState::Held { session: s, .. } if *s == session)
            {
                released = true;
                // A park marks the release in the same write, so a message
                // that waits in the inbox wakes the agent again.
                let next = if reason == "park" {
                    lease::park(&doc.value, now)
                } else {
                    lease::release(&doc.value, now)
                };
                self.store.put(&key, &next, Some(doc.revision)).await?;
                mem.idle.remove(agent);
                mem.activity.remove(agent);
                let mut a = self.agent(agent).await?;
                let host = a.value.host.clone();
                a.value.session = None;
                self.store.put(&a.key, &a.value, Some(a.revision)).await?;
                // A park is its own kind: the lease release that a message
                // may wake. The checkpoint ref stays the host's report.
                let body = if reason == "park" {
                    SwarmEventBody::AgentParked {
                        session,
                        git_ref: None,
                    }
                } else {
                    SwarmEventBody::LeaseReleased {
                        session,
                        token: token.unwrap_or(FencingToken(0)),
                    }
                };
                let event = self.lease_event(now, Severity::Info, agent, host.as_ref(), body);
                Self::queue(mem, event);
            } else if !swept
                && matches!(
                    &doc.value.state,
                    LeaseState::Expired { last_session: Some(s), .. } if *s == session
                )
            {
                late = true;
            }
        }
        let mut already_ended = false;
        if let Some(mut s) = self.store.get::<Session>(&keys::session(&session)).await? {
            already_ended = s.value.ended.is_some();
            s.value.ended = Some(now);
            self.store.put(&s.key, &s.value, Some(s.revision)).await?;
        }
        let ended = released || swept || (late && !already_ended);
        if ended
            && (swept || late)
            && let Ok(mut a) = self.agent(agent).await
            && a.value.session == Some(session)
        {
            a.value.session = None;
            self.store.put(&a.key, &a.value, Some(a.revision)).await?;
        }
        tracing::info!(%agent, %session, reason, swept, late, "session ended");
        if ended {
            self.mark_spawn_ended(agent).await?;
            // A late end is a death that the hub learns of after the fact:
            // the owner hears it before the relaunch message, as with the
            // sweep. The sweep sends its own message.
            if late && let Ok(doc) = self.agent(agent).await {
                let claims = self.in_progress_claims(agent, &doc.value.channels).await?;
                self.tell_owner_dead(
                    &doc.value,
                    &claims,
                    "the hub learned of its end after the lease expired",
                )
                .await?;
            }
            // A stale end, for example from a host that lost the lease, must
            // not start a move.
            if let Some(p) = mem.pending_moves.get(agent).cloned() {
                let a = self.agent(agent).await?.value;
                self.place_agent(mem, &a, p.reason, p.target, p.prompt)
                    .await?;
            } else if reason != "park" {
                // Not a park, so not a deliberate sleep: a claim that the
                // agent still holds means the end was not its own exit after
                // the task closed.
                self.relaunch_after_session_end(mem, agent, session, reason, swept)
                    .await?;
            }
        }
        Ok(())
    }

    /// Moves a spawn record that the sweep ended back to `Running`, because
    /// its session reclaimed the expired lease and still holds it. A failed
    /// or cancelled spawn keeps the state that waits for a person.
    async fn mark_spawn_running(&self, agent: &AgentId) -> Result<(), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok(());
        };
        if !matches!(
            doc.value.state,
            SpawnState::Ended | SpawnState::Launching | SpawnState::Queued
        ) {
            return Ok(());
        }
        let mut record = doc.value;
        record.state = SpawnState::Running;
        record.last_reject.clear();
        match self.store.put(&key, &record, Some(doc.revision)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == ApiErrorCode::Conflict => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Moves a running or launching spawn record to `Ended`, so `ps` and
    /// `agents` stop showing the spawn as running. A failed or cancelled
    /// spawn keeps the state that waits for a person.
    async fn mark_spawn_ended(&self, agent: &AgentId) -> Result<(), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok(());
        };
        if !matches!(doc.value.state, SpawnState::Running | SpawnState::Launching) {
            return Ok(());
        }
        let mut record = doc.value;
        record.state = SpawnState::Ended;
        record.last_reject.clear();
        match self.store.put(&key, &record, Some(doc.revision)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == ApiErrorCode::Conflict => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// A session ended while its agent still claimed an `in_progress` task, so
    /// the end was not the agent's own exit after it closed its work: the hub
    /// places the agent again, with the prompt context of its session and its
    /// newest checkpoint, or, when no host takes it, puts the claim in front
    /// of the channel owner as `stuck`.
    ///
    /// `recover` marks a death that the liveness sweep found: a movable
    /// worker is relaunched by a pending move with reason `Recover`, its tries
    /// count on the spawn record, and after `MAX_SESSION_RECOVERIES` the claim
    /// waits for a person as `needs_human`. A retired or paused agent, and a
    /// spawn that failed or that a person cancelled, keeps today's behavior:
    /// no placement, and the task keeps its claim.
    async fn relaunch_after_session_end(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        session: SessionId,
        reason: &str,
        recover: bool,
    ) -> Result<(), ApiError> {
        let a = self.agent(agent).await?.value;
        if a.retired || a.paused.is_some() {
            tracing::debug!(%agent, "session end: the agent is retired or paused");
            return Ok(());
        }
        let record = self.store.get::<SpawnRecord>(&keys::spawn(agent)).await?;
        if record.as_ref().is_some_and(|rec| {
            matches!(rec.value.state, SpawnState::Failed | SpawnState::Cancelled)
        }) {
            tracing::debug!(%agent, "session end: the spawn is closed");
            return Ok(());
        }
        let claims = self.in_progress_claims(agent, &a.channels).await?;
        if claims.is_empty() {
            return Ok(());
        }
        // A sweep recovery of a movable worker counts its tries, and the
        // fourth death in a row leaves the claim to a person.
        let recovering = recover && a.movable;
        if recovering && let Some(doc) = &record {
            if doc.value.recoveries >= MAX_SESSION_RECOVERIES {
                let stopped = self
                    .mark_claim_needs_human(agent, &format!(
                        "the session of {agent} died {MAX_SESSION_RECOVERIES} times with this claim"
                    ))
                    .await?;
                if stopped {
                    mem.recovery_stopped.insert(agent.clone());
                    let event = self.lease_event(
                        self.now(),
                        Severity::Warn,
                        agent,
                        a.host.as_ref(),
                        SwarmEventBody::SessionRelaunch {
                            session,
                            reason: reason.to_owned(),
                            failed: Some(format!(
                                "gave up after {MAX_SESSION_RECOVERIES} recoveries"
                            )),
                        },
                    );
                    Self::queue(mem, event);
                    self.tell_owner_relaunch(
                        &a,
                        &claims,
                        false,
                        Some("the hub gave up after 3 recoveries"),
                    )
                    .await?;
                }
                return Ok(());
            }
            let mut next = doc.value.clone();
            next.recoveries = next.recoveries.saturating_add(1);
            self.store.put(&doc.key, &next, Some(doc.revision)).await?;
        }
        let (target, why) = if recovering {
            (MoveReason::Recover, "the session died")
        } else {
            (MoveReason::Takeover, reason)
        };
        if recovering {
            // The demand stays as a pending move, so the tick places the
            // worker as soon as a host fits.
            mem.pending_moves.insert(
                agent.clone(),
                PendingMove {
                    target: Default::default(),
                    reason: target,
                    prompt: None,
                },
            );
        }
        let placed = self
            .place_agent(mem, &a, target, Default::default(), None)
            .await?;
        let failed = if placed {
            None
        } else {
            let refusal = self.relaunch_refusal(mem, &a).await?;
            if !recovering {
                let note =
                    format!("the session of {agent} ended ({why}); relaunch failed: {refusal}");
                self.claims_stuck(mem, &claims, &note).await?;
            }
            Some(refusal)
        };
        let severity = match failed {
            Some(_) => Severity::Warn,
            None => Severity::Info,
        };
        // One event says which of the two happened.
        let event = self.lease_event(
            self.now(),
            severity,
            agent,
            a.host.as_ref(),
            SwarmEventBody::SessionRelaunch {
                session,
                reason: why.to_owned(),
                failed: failed.clone(),
            },
        );
        Self::queue(mem, event);
        // One direct message per relaunch, to the owner of the agent.
        self.tell_owner_relaunch(&a, &claims, placed, failed.as_deref())
            .await?;
        tracing::info!(%agent, %session, reason, relaunched = placed, recover = recovering, "the session ended with a claim");
        Ok(())
    }

    /// The `in_progress` tasks that `agent` claims in its channels.
    pub(crate) async fn in_progress_claims(
        &self,
        agent: &AgentId,
        channels: &[ChannelName],
    ) -> Result<Vec<(ChannelName, Task)>, ApiError> {
        let mut claims = Vec::new();
        for c in channels {
            for t in self.tasks(c).await? {
                if t.claimed_by.as_ref() == Some(agent) && t.status == TaskStatus::InProgress {
                    claims.push((c.clone(), t));
                }
            }
        }
        Ok(claims)
    }

    /// Why the hub could not place `agent` now, for the stuck message: the
    /// refused hosts as `borant ps --queued` reads them, or the wait on a
    /// bundle whose members are all limited.
    async fn relaunch_refusal(&self, mem: &Memory, agent: &Agent) -> Result<String, ApiError> {
        let views = self.host_views(mem, &agent.id).await?;
        let req = self.placement_req(agent, None).await;
        let bundles = self.launch_bundles(agent);
        match schedule::place(agent.preferred_harness, None, &req, &bundles, &views) {
            Ok((host, _)) => Ok(format!(
                "no member of a bundle of the agent is free on {host}"
            )),
            Err(rejected) if rejected.is_empty() => Ok("no host is online".to_owned()),
            Err(rejected) => Ok(rejected
                .iter()
                .map(|(host, why)| format!("{host}: {why}"))
                .collect::<Vec<_>>()
                .join("; ")),
        }
    }

    /// Sets each claimed task that is still `in_progress` to `stuck` with
    /// `why`, in front of the owner of its channel.
    async fn claims_stuck(
        &self,
        mem: &mut Memory,
        claims: &[(ChannelName, Task)],
        why: &str,
    ) -> Result<(), ApiError> {
        let now = self.now();
        for (channel, task) in claims {
            // A request may have changed the task since the session ended.
            let key = keys::task(channel, &task.id);
            let Some(doc) = self.store.get::<Task>(&key).await? else {
                continue;
            };
            let before = doc.value;
            if before.status != TaskStatus::InProgress {
                continue;
            }
            let mut next = before.clone();
            next.status = TaskStatus::Stuck;
            next.attention_msg = Some(why.to_owned());
            next.updated = now;
            self.store.put(&key, &next, Some(doc.revision)).await?;
            let owner = self.channel(channel).await?.value.owner;
            if let Err(e) = self
                .task_event(
                    channel,
                    &owner,
                    &next,
                    TaskEventKind::Updated,
                    "the session of its claimer ended",
                )
                .await
            {
                tracing::warn!(task = %next.id, "task event lost: {e}");
            }
            self.task_status_event(mem, channel, &before, &next);
            tracing::warn!(task = %next.id, %channel, "the claim is stuck after the session ended");
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
    ///
    /// A lease that an earlier pass found nothing to do for is skipped without
    /// a store call until its time comes or its revision changes, so the tick
    /// costs what the leases that need it cost, not what the store holds.
    pub(crate) async fn tick_leases(&self, lists: &Lists) -> Result<(), ApiError> {
        let leases = &lists.leases;
        // The revision of each agent record: a person who pins, pauses, or
        // resumes an agent writes that record and not its lease, so the pass
        // must look at the lease again when it changes.
        let agent_rev: std::collections::HashMap<&AgentId, u64> = lists
            .agents
            .iter()
            .map(|doc| (&doc.value.id, doc.revision))
            .collect();
        let now = self.now();
        // The expiries of this pass alone, so a pass that a failure stopped
        // early leaves nothing for the next one.
        self.mem.lock().await.expired_hosts.clear();
        for doc in leases {
            let mut mem = self.mem.lock().await;
            if mem.maintenance {
                return Ok(());
            }
            let agent_revision = agent_rev.get(&doc.value.agent).copied();
            if Self::gate_skips(&mem, doc, agent_revision, now) {
                continue;
            }
            let agent = doc.value.agent.clone();
            let step = self.tick_one(&mut mem, doc).await;
            // A lease that the step left alone is not read again before its
            // time or a change of its revision. One that the step wrote, or
            // that a request changed under it, is looked at again next tick.
            match step {
                Ok(Some(rest)) => {
                    mem.lease_gate.insert(
                        agent,
                        LeaseGate {
                            revision: doc.revision,
                            agent_revision,
                            at: rest.at(),
                        },
                    );
                }
                Ok(None) | Err(_) => {
                    mem.lease_gate.remove(&agent);
                }
            }
            // The events of this step wait here, so no publish holds the lock.
            let queued = !mem.outbox.is_empty();
            drop(mem);
            if queued {
                self.flush().await;
            }
            match step {
                // A request changed this lease between the list and the step,
                // so the compare-and-set write failed. The next tick retries.
                Err(e) if e.code == ApiErrorCode::Conflict => {
                    tracing::debug!("lease changed during the tick: {e}");
                }
                // The events are already out; only a real failure stops the
                // pass.
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        }
        // The hosts that went silent and the ones heard again, once per host
        // and per silence, after every lease of the pass had its step.
        self.check_host_silence(now, leases).await;
        // The gates of the leases that the store no longer holds go with them,
        // so a hub that sees many agents over time keeps one entry per lease.
        let mut mem = self.mem.lock().await;
        let held: std::collections::HashSet<&AgentId> =
            leases.iter().map(|doc| &doc.value.agent).collect();
        mem.lease_gate.retain(|agent, _| held.contains(agent));
        mem.wake_seen.retain(|agent, _| held.contains(agent));
        // The agents of each host, for a registration that would otherwise
        // read every lease.
        let mut leases_on: std::collections::HashMap<HostName, u64> =
            std::collections::HashMap::new();
        for doc in leases {
            if let LeaseState::Held { host, .. } | LeaseState::Reserved { host, .. } =
                &doc.value.state
            {
                *leases_on.entry(host.clone()).or_default() += 1;
            }
        }
        mem.leases_on = leases_on;
        Ok(())
    }

    /// True when the pass already looked at this lease and found nothing that
    /// its time or a change of its revision would not bring back. A message
    /// that waits for the agent, a pending placement, or a new agent record
    /// takes the gate away.
    fn gate_skips(
        mem: &Memory,
        doc: &Doc<Lease>,
        agent_revision: Option<u64>,
        now: Timestamp,
    ) -> bool {
        if mem.pending_moves.contains_key(&doc.value.agent) {
            return false;
        }
        match mem.lease_gate.get(&doc.value.agent) {
            Some(gate)
                if gate.revision == doc.revision && gate.agent_revision == agent_revision =>
            {
                gate.at.is_none_or(|at| at > now)
            }
            _ => false,
        }
    }

    /// The listed lease, if no request changed it since the list.
    async fn unchanged_lease(&self, doc: &Doc<Lease>) -> Result<Option<Doc<Lease>>, ApiError> {
        Ok(self
            .store
            .get::<Lease>(&doc.key)
            .await?
            .filter(|fresh| fresh.revision == doc.revision))
    }

    /// One lease: the expiry write when its time has come, else the takeover
    /// and claim path for a lease that no session can hold.
    ///
    /// `Ok(None)` means the step wrote the lease or lost it to a request, so
    /// the next tick looks at it again. `Ok(Some(rest))` means the lease was
    /// left alone, with when to look at it again.
    async fn tick_one(&self, mem: &mut Memory, doc: &Doc<Lease>) -> Result<Option<Rest>, ApiError> {
        let now = self.now();
        let cfg = self.cfg.lease;
        let quiet = mem.started.is_some_and(|t| now.since(t) < cfg.lease);
        let agent_id = doc.value.agent.clone();
        if let Some(mut next) = lease::tick(&doc.value, now, &cfg) {
            let was_held = matches!(doc.value.state, LeaseState::Held { .. });
            // The event of the lease that just ended, before any placement
            // that a later tick makes.
            let ended = match &doc.value.state {
                LeaseState::Held { session, host, .. } => Some((
                    Severity::Warn,
                    Some(host.clone()),
                    SwarmEventBody::LeaseExpired {
                        session: *session,
                        token: doc.value.token,
                    },
                )),
                LeaseState::Reserved {
                    host,
                    reason,
                    until,
                } => Some((
                    Severity::Warn,
                    Some(host.clone()),
                    SwarmEventBody::LeaseReservationEnded {
                        reason: *reason,
                        until: *until,
                    },
                )),
                _ => None,
            };
            if let LeaseState::Reserved { host, .. } = &doc.value.state {
                // A launch did not register in time. The host is no good for
                // this agent, and the try counts against the spawn. Place
                // again at once.
                mem.failed_hosts
                    .entry(agent_id.clone())
                    .or_default()
                    .insert(host.clone(), now);
                // The tick knows the host but neither the timeout the host
                // waited nor the error it saw, so the try records neither.
                let _ = self
                    .note_launch_failure(mem, &agent_id, host, None, None)
                    .await?;
                next.state = LeaseState::Expired {
                    since: now.minus(cfg.grace),
                    last_session: None,
                };
                tracing::warn!(
                    agent = %agent_id,
                    %host,
                    "launch reservation ended without a session"
                );
            }
            self.store.put(&doc.key, &next, Some(doc.revision)).await?;
            // A held lease that expires is the measure of a host that stopped
            // beating. The pass counts them per host, so one silent host is
            // one line and not one line per agent.
            if was_held && let LeaseState::Held { host, .. } = &doc.value.state {
                Self::note_expiry(mem, host, &agent_id, now);
            }
            if was_held {
                mem.expired_at.entry(agent_id.clone()).or_insert(now);
                if let Ok(mut a) = self.agent(&agent_id).await {
                    a.value.session = None;
                    self.store.put(&a.key, &a.value, Some(a.revision)).await?;
                    tracing::info!(agent = %a.value.id, "lease expired");
                }
            }
            if let Some((severity, host, body)) = ended {
                let event = self.lease_event(now, severity, &agent_id, host.as_ref(), body);
                Self::queue(mem, event);
            }
            return Ok(None);
        }
        // A lease with time left needs nothing of this pass: the expiry is
        // `lease::tick` above and the renewal is the heartbeat's. No store
        // read, so a live agent costs no call per tick.
        let since = match &doc.value.state {
            LeaseState::Held { expires, .. } => return Ok(Some(Rest::At(*expires))),
            LeaseState::Reserved { until, .. } => return Ok(Some(Rest::At(*until))),
            LeaseState::Expired { since, .. } | LeaseState::Released { since, .. } => *since,
        };
        // A lease that no session can hold yet needs nothing either: the
        // takeover waits for the grace, and the quiet time after a restart
        // holds it longer. Claims can only return after the claim timeout,
        // which is far past the grace.
        let mut not_before = since.plus(cfg.grace);
        if let Some(started) = mem.started
            && quiet
        {
            not_before = not_before.max(started.plus(cfg.lease));
        }
        if now < not_before {
            return Ok(Some(Rest::At(not_before)));
        }
        // The lease was listed before this step took the mutex. A session
        // that registered in between holds the lease now and must keep its
        // claims, so act only on a listing that is still current. The
        // writes above are safe already: their compare-and-set fails.
        let Some(doc) = self.unchanged_lease(doc).await? else {
            return Ok(None);
        };
        let since = match doc.value.state {
            LeaseState::Expired { since, .. } | LeaseState::Released { since, .. } => since,
            _ => return Ok(Some(Rest::Settled)),
        };
        let Ok(agent) = self.agent(&agent_id).await else {
            // The record of the agent may arrive after its lease; look again
            // at the next poll rather than settle for good.
            return Ok(Some(Rest::At(now.plus(SETTLED_POLL))));
        };
        let agent = agent.value;
        // A retired agent is never placed again: no takeover and no wake.
        if agent.retired || agent.paused.is_some() {
            return Ok(Some(Rest::Settled));
        }
        // A chief is started by a person: the hub never recovers it, not on
        // a dead session and not after a clean end.
        if self.person_started(&agent) {
            mem.pending_moves.remove(&agent_id);
            return Ok(Some(Rest::Settled));
        }

        // Claims return to open when no session held the agent for the
        // claim timeout, measured from the first expiry. A pinned agent keeps
        // its claims, because the hub will not release it.
        let base = mem.expired_at.get(&agent_id).copied().unwrap_or(since);
        if !agent.pinned
            && agent.paused.is_none()
            && now.since(base) >= cfg.claim_timeout
            && mem.claims_released.insert(agent_id.clone())
        {
            let n = self.unclaim_everywhere(mem, &agent_id).await?;
            if n > 0 {
                tracing::warn!(agent = %agent_id, n, "claims returned to open after the claim timeout");
                let event = self
                    .event(
                        now,
                        Severity::Warn,
                        SwarmEventBody::ClaimsReleased {
                            tasks: n as u64,
                            after_secs: now.since(base).as_secs(),
                        },
                    )
                    .about_agent(agent_id.clone());
                Self::queue(mem, event);
            }
        }
        // The hub never replaces a pinned agent whose session may still run.
        // A dead session holds nothing, so the agent comes back on the host
        // that held it, never on another one.
        let pinned_recovery =
            agent.pinned && pinned_session_dead(mem, &agent, &doc.value, now, &self.cfg.lease);
        if agent.pinned
            && !pinned_recovery
            && !matches!(doc.value.state, LeaseState::Released { .. })
        {
            // The session may still run; the claim timeout is what makes its
            // death visible.
            return Ok(Some(Rest::At(since.plus(cfg.claim_timeout))));
        }

        let expired = matches!(doc.value.state, LeaseState::Expired { .. });
        // A parked agent is asleep, not gone: a wake message or a claim wakes
        // it. A plain release (an ordinary session end) stays asleep.
        let parked = matches!(doc.value.state, LeaseState::Released { parked: true, .. });
        let pending = mem.pending_moves.get(&agent_id).cloned();
        if pinned_recovery {
            let target = MoveTarget {
                host: agent.host.clone(),
                harness: None,
            };
            let placed = self
                .place_agent(mem, &agent, MoveReason::Takeover, target.clone(), None)
                .await?;
            if !placed {
                // The pinned host is not ready. The demand waits for it, so
                // the agent goes up there and nowhere else.
                mem.pending_moves.insert(
                    agent_id.clone(),
                    PendingMove {
                        target,
                        reason: MoveReason::Takeover,
                        prompt: None,
                    },
                );
            }
            return Ok(None);
        }
        if let Some(p) = pending {
            // A spawn or a planned move that is not placed yet.
            self.place_agent(mem, &agent, p.reason, p.target, p.prompt)
                .await?;
            return Ok(None);
        }
        if !(expired || parked) || !agent.movable || agent.channels.is_empty() {
            // Nothing places this lease again. Only the claim timeout can
            // still change a task, so that is the one time to look.
            return Ok(Some(match mem.claims_released.contains(&agent_id) {
                true => Rest::Settled,
                false => Rest::At(base.plus(cfg.claim_timeout)),
            }));
        }
        let mut needs = !mem.claims_released.contains(&agent_id)
            && self.has_claims(&agent_id, &agent.channels).await?;
        let pending_msgs = if needs {
            0
        } else {
            self.bus.inbox_pending(&agent_id).await.unwrap_or(0)
        };
        if pending_msgs > 0 {
            let due = mem
                .wake_checked
                .get(&agent_id)
                .is_none_or(|t| now.since(*t) >= WAKE_CHECK_EVERY);
            // A backlog that did not change since the pass read it has the
            // same answer, so the messages are read again at the poll and not
            // before: sixty-two thousand undelivered messages are not a scan
            // every ten seconds.
            let same = mem.wake_seen.get(&agent_id) == Some(&pending_msgs);
            let stale = mem
                .wake_checked
                .get(&agent_id)
                .is_none_or(|t| now.since(*t) >= SETTLED_POLL);
            if due && (!same || stale) {
                mem.wake_checked.insert(agent_id.clone(), now);
                mem.wake_seen.insert(agent_id.clone(), pending_msgs);
                needs = self
                    .bus
                    .unacked(&agent_id)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|e| wake::should_wake(e, &agent_id));
            }
        } else {
            mem.wake_seen.remove(&agent_id);
        }
        // The log check recovers a lost session's undelivered replies. A
        // parked session ended cleanly, so only a claim or a wake message
        // wakes it.
        if !needs && expired && mem.idle_checked.insert(agent_id.clone()) {
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
        // An ended spawn that already spent its recoveries waits for a
        // person: a lease tick must not relaunch it after a hub restart
        // forgot the in-memory stop.
        if needs && self.recovery_exhausted(&agent_id).await? {
            tracing::debug!(agent = %agent_id, "no takeover: the recovery limit");
            return Ok(Some(Rest::Settled));
        }
        if needs {
            let placed = self
                .place_agent(mem, &agent, MoveReason::Takeover, Default::default(), None)
                .await?;
            if !placed && !agent.retired && agent.paused.is_none() {
                // No host fits now. The demand stays as a pending move, so
                // the elastic executor can create a pool host for it and the
                // tick places the agent when that host registers. It goes at
                // registration, retirement, or a pause.
                mem.pending_moves.insert(
                    agent_id.clone(),
                    PendingMove {
                        target: Default::default(),
                        reason: MoveReason::Takeover,
                        prompt: None,
                    },
                );
            }
            return Ok(None);
        }
        // New mail is the one thing that no event of the hub tells the pass
        // about: a host publishes it to the inbox, so a released agent that
        // can wake reads the count of its inbox at the wake check.
        Ok(Some(Rest::At(now.plus(WAKE_CHECK_EVERY))))
    }
}

/// The rule of the alert that a silent host fires. It is not a rule of the
/// alert pass: the hub keeps the silence in memory and resolves it on the
/// host's own beat, because that pass resolves every key that its rules do
/// not call true.
pub(crate) const HOST_SILENT: &str = "host_silent";

/// The held leases that one lease pass expired, per host.
#[derive(Debug, Clone)]
pub(crate) struct HostExpiry {
    /// When the first of them expired.
    pub since: Timestamp,
    /// The agents that lost their lease.
    pub agents: std::collections::BTreeSet<AgentId>,
}

/// A host that stopped beating for the lease timeout and lost its held
/// leases, with the agents that it lost, until the hub hears it again.
#[derive(Debug, Clone)]
pub(crate) struct SilentHost {
    /// The last heartbeat or registration before the silence.
    pub last_seen: Timestamp,
    /// When the first lease of the host expired.
    pub since: Timestamp,
    /// The agents whose leases expired, which is who it lost.
    pub agents: std::collections::BTreeSet<AgentId>,
}

/// The agents of a silence, for one line: the first few by name, then a
/// count, so a host of a hundred agents still reads as one line.
fn agent_names(agents: &std::collections::BTreeSet<AgentId>) -> String {
    const SHOWN: usize = 12;
    let names: Vec<&str> = agents.iter().take(SHOWN).map(AgentId::as_str).collect();
    let rest = agents.len().saturating_sub(names.len());
    let list = names.join(", ");
    if rest > 0 {
        format!("{list}, and {rest} more")
    } else {
        list
    }
}

impl Hub {
    /// Counts one held lease that the pass expired, for its host.
    fn note_expiry(mem: &mut Memory, host: &HostName, agent: &AgentId, now: Timestamp) {
        let entry = mem
            .expired_hosts
            .entry(host.clone())
            .or_insert_with(|| HostExpiry {
                since: now,
                agents: std::collections::BTreeSet::new(),
            });
        entry.agents.insert(agent.clone());
    }

    /// The hosts that stopped beating for the lease timeout and lost their
    /// leases: one `host_silent` alert per host and not one per agent, and it
    /// resolves when the hub hears the host again. A lease that expires while
    /// the host still beats says nothing about the host, so it fires nothing.
    ///
    /// The silence lives in memory and not in `alert.*`: the alert pass owns
    /// that store and resolves every key that its rules do not call true, so
    /// a stored silence would come back as a false resolve. The event kinds
    /// and the notifier are the ones of the alert path, and the silence ends
    /// on the host's own beat.
    async fn check_host_silence(&self, now: Timestamp, leases: &[Doc<Lease>]) {
        let cfg = self.cfg.lease;
        let (silenced, heard) = {
            let mut mem = self.mem.lock().await;
            // A host that the hub heard after its last beat ends the silence,
            // however many beats it queued while it was away: one line per
            // silence, and it goes before a silence that starts again here.
            let heard: Vec<(HostName, SilentHost)> = mem
                .silent_hosts
                .iter()
                .filter(|(host, silent)| {
                    mem.hosts_seen
                        .get(*host)
                        .is_some_and(|seen| *seen > silent.last_seen)
                })
                .map(|(host, silent)| (host.clone(), silent.clone()))
                .collect();
            for (host, _) in &heard {
                mem.silent_hosts.remove(host);
            }
            let mut silenced = Vec::new();
            let expired = std::mem::take(&mut mem.expired_hosts);
            for (host, expiry) in expired {
                if let Some(silent) = mem.silent_hosts.get_mut(&host) {
                    // The silence goes on: the leases of a later pass join
                    // the same line.
                    silent.agents.extend(expiry.agents);
                    continue;
                }
                let Some(last_seen) = mem.hosts_seen.get(&host).copied() else {
                    continue;
                };
                // A beat within the lease timeout says the host is alive: its
                // lease expired for another reason than silence.
                if now.since(last_seen) < cfg.lease {
                    continue;
                }
                let silent = SilentHost {
                    last_seen,
                    since: expiry.since,
                    agents: expiry.agents,
                };
                mem.silent_hosts.insert(host.clone(), silent.clone());
                silenced.push((host, silent));
            }
            (silenced, heard)
        };
        for (host, silent) in silenced {
            self.announce_silence(now, &host, &silent).await;
        }
        for (host, silent) in heard {
            self.announce_back(now, &host, &silent, leases).await;
        }
    }

    /// One warn, one notice, and one event for a host that went silent. The
    /// line names the host, when it was last seen, and the agents it lost, so
    /// a person reads the host and not a Borant outage.
    async fn announce_silence(&self, at: Timestamp, host: &HostName, silent: &SilentHost) {
        let n = silent.agents.len();
        tracing::warn!(
            leases = n,
            "host {host} is silent since {}: {n} leases expired",
            silent.since
        );
        let line = format!(
            "host {host} is silent since {}: {n} leases expired (last seen {})",
            silent.since, silent.last_seen
        );
        if let Some(notifier) = &self.notifier {
            let notice = Notice::new(
                "A host is silent",
                format!("{line}. Affected: {}.", agent_names(&silent.agents)),
            )
            .with_priority(Priority::High)
            .with_tags([HOST_SILENT.to_owned()]);
            if let Err(e) = notifier.notify(&notice).await {
                tracing::warn!(%host, "the silent-host notice failed: {e}");
            }
        }
        let mut event = self
            .event(
                at,
                Severity::Warn,
                SwarmEventBody::AlertFired {
                    rule: HOST_SILENT.to_owned(),
                    subject: host.to_string(),
                },
            )
            .about_host(host.clone());
        event.note = Some(line);
        self.publish(event).await;
    }

    /// One info event when the hub hears a silent host again: how long it was
    /// silent, and how many of the leases it lost it holds again.
    async fn announce_back(
        &self,
        at: Timestamp,
        host: &HostName,
        silent: &SilentHost,
        leases: &[Doc<Lease>],
    ) {
        let held_on: std::collections::HashSet<&AgentId> = leases
            .iter()
            .filter_map(|doc| match &doc.value.state {
                LeaseState::Held { host: h, .. } if h == host => Some(&doc.value.agent),
                _ => None,
            })
            .collect();
        let reclaimed = silent.agents.iter().filter(|a| held_on.contains(a)).count();
        let secs = at.since(silent.last_seen).as_secs();
        tracing::info!(%host, silent_secs = secs, reclaimed, "the host is back");
        let mut event = self
            .event(
                at,
                Severity::Info,
                SwarmEventBody::AlertResolved {
                    rule: HOST_SILENT.to_owned(),
                    subject: host.to_string(),
                },
            )
            .about_host(host.clone());
        event.note = Some(format!(
            "host {host} was silent for {secs}s: {reclaimed} of {} leases reclaimed",
            silent.agents.len()
        ));
        self.publish(event).await;
    }
}
