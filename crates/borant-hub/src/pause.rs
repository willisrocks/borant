//! Stop and resume (phase 6 spec, "Stop, resume, caps"). A stop waits for the
//! turn to end and fences the session after a grace. A paused agent keeps its
//! claims, is skipped by every placement path, gets no stall alert and no claim
//! timeout, and is placed again when it is resumed. A role cap stops an agent
//! the same way.

use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    Agent, Channel, HostStats, Lease, LeaseState, Member, MoveReason, PauseReason, Sender, Task,
    TaskStatus,
};
use borant_core::proto::{Actor, ApiError, ApiErrorCode, HostCommand, MoveTarget, PauseSelector};
use borant_core::{AgentId, ChannelName, HostName, SessionId, Timestamp, keys, subjects};
use serde_json::{Value, json};

use crate::{Hub, Memory, PendingMove};

/// The grace of a stop that names none: a turn that runs longer is fenced.
pub const DEFAULT_GRACE_SECS: u64 = 120;

/// The UTC date of the turn counter, for example `2026-10-25`.
pub(crate) fn utc_date(now: Timestamp) -> String {
    chrono::DateTime::from_timestamp_millis(now.0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// The key of the turn counter of one agent on one UTC day.
pub(crate) fn turns_key(agent: &AgentId, now: Timestamp) -> String {
    format!("turns.{agent}.{}", utc_date(now))
}

/// The key of the start of the current session of one agent.
pub(crate) fn session_start_key(agent: &AgentId) -> String {
    format!("session.started.{agent}")
}

impl Hub {
    /// Stops an agent, or each member of a channel whose channels are all
    /// paused. A person or the owner of the channel may call it.
    pub(crate) async fn stop(
        &self,
        mem: &mut Memory,
        actor: Option<&Actor>,
        selector: PauseSelector,
        grace_secs: Option<u32>,
    ) -> Result<Value, ApiError> {
        self.authorize_pause(actor, &selector).await?;
        let by = actor.map_or(Sender::Human, |a| Sender::Agent(a.agent.clone()));
        let grace = Duration::from_secs(grace_secs.map_or(DEFAULT_GRACE_SECS, u64::from));
        match selector {
            PauseSelector::Agent(id) => {
                let agent = self.agent(&id).await?.value;
                let why = format!("stopped by {by}");
                self.stop_agent(mem, &agent, by, why, grace).await?;
                Ok(json!({ "agent": id, "paused": true }))
            }
            PauseSelector::Channel(name) => {
                let stopped = self.stop_channel(mem, &name, by, grace).await?;
                Ok(json!({ "channel": name, "paused": true, "agents": stopped }))
            }
        }
    }

    /// Resumes an agent, or each member of a channel that no paused channel
    /// holds any more, and places it again with a planned move.
    pub(crate) async fn resume(
        &self,
        mem: &mut Memory,
        actor: Option<&Actor>,
        selector: PauseSelector,
    ) -> Result<Value, ApiError> {
        self.authorize_pause(actor, &selector).await?;
        match selector {
            PauseSelector::Agent(id) => {
                let placed = self.resume_agent(mem, &id).await?;
                Ok(json!({ "agent": id, "paused": false, "placed": placed }))
            }
            PauseSelector::Channel(name) => {
                self.set_channel_paused(&name, false).await?;
                let mut resumed = Vec::new();
                for member in self
                    .store
                    .list::<Member>(&keys::members_prefix(&name))
                    .await?
                {
                    let Ok(doc) = self.agent(&member.value.agent).await else {
                        continue;
                    };
                    let agent = doc.value;
                    // A cap or a person keeps an agent down; only a channel
                    // stop is lifted with the channel.
                    let by_channel = agent
                        .paused
                        .as_ref()
                        .is_some_and(|p| p.why.starts_with("channel #"));
                    if !by_channel || self.all_channels_paused(&agent).await? {
                        continue;
                    }
                    self.resume_agent(mem, &agent.id).await?;
                    resumed.push(agent.id);
                }
                Ok(json!({ "channel": name, "paused": false, "agents": resumed }))
            }
        }
    }

    /// Stops one agent: `paused` is set, and its session stops at the turn end
    /// or is fenced after `grace`.
    pub(crate) async fn stop_agent(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        by: Sender,
        why: String,
        grace: Duration,
    ) -> Result<(), ApiError> {
        let now = self.now();
        let mut saved = agent.clone();
        saved.paused = Some(PauseReason {
            by: by.clone(),
            why: why.clone(),
            at: now,
        });
        self.store.upsert(&keys::agent(&agent.id), &saved).await?;
        let event = self
            .event(now, Severity::Warn, SwarmEventBody::AgentPaused { by, why })
            .about_agent(agent.id.clone());
        Self::queue(mem, event);
        tracing::info!(agent = %agent.id, "agent paused");
        // A session that already ended its turn stops at once; one that is
        // still in a turn stops at its turn end, and is fenced when that
        // outlives the grace.
        let idle = mem
            .idle
            .get(&agent.id)
            .filter(|report| report.since.is_some())
            .cloned();
        if let Some(report) = idle {
            self.stop_session(mem, &agent.id, report.session, &report.host)
                .await?;
        } else if let Some((session, host)) = self.held_session(&agent.id).await? {
            mem.pause_fences
                .insert(session, (agent.id.clone(), host.clone(), now.plus(grace)));
        }
        Ok(())
    }

    /// Stops the sessions of paused agents that just ended a turn. The
    /// heartbeat carries the end of the last turn of each session.
    pub(crate) async fn stop_paused(
        &self,
        mem: &mut Memory,
        host: &HostName,
        sessions: &[borant_core::proto::SessionLeaseRef],
    ) -> Result<(), ApiError> {
        for s in sessions {
            if s.idle_since.is_none() || mem.stopped.contains(&s.session) {
                continue;
            }
            let Ok(doc) = self.agent(&s.agent).await else {
                continue;
            };
            if doc.value.paused.is_none() {
                continue;
            }
            self.stop_session(mem, &s.agent, s.session, host).await?;
        }
        Ok(())
    }

    /// Fences the sessions whose stop outlived its grace, so a hung turn still
    /// ends. The host ends the session, which releases the lease as a plain
    /// release, not a park.
    pub(crate) async fn fence_overdue(&self) -> Result<(), ApiError> {
        let now = self.now();
        let mut mem = self.mem.lock().await;
        let overdue: Vec<(SessionId, AgentId, HostName)> = mem
            .pause_fences
            .iter()
            .filter(|(_, (_, _, at))| *at <= now)
            .map(|(session, (agent, host, _))| (*session, agent.clone(), host.clone()))
            .collect();
        for (session, agent, host) in overdue {
            mem.pause_fences.remove(&session);
            // The session is fenced on the host that ran it, even when its
            // lease expired while the turn hung. A Fence for a session that
            // already ended is harmless.
            let cmd = HostCommand::Fence {
                session,
                reason: "stopped: the turn did not end within the grace".to_owned(),
            };
            let payload =
                serde_json::to_vec(&cmd).map_err(|e| ApiError::internal(e.to_string()))?;
            self.bus
                .send(&subjects::host_cmd(&host), payload.into())
                .await?;
            mem.stopped.insert(session);
            tracing::warn!(%agent, %session, "the turn of a stopped agent outlived its grace: fenced");
        }
        Ok(())
    }

    /// Counts the turns of each session of a heartbeat and stops an agent at a
    /// cap of its role: `max_turns_per_day` or `max_session_hours`.
    pub(crate) async fn check_caps(
        &self,
        mem: &mut Memory,
        stats: &HostStats,
    ) -> Result<(), ApiError> {
        let now = self.now();
        for session in &stats.sessions {
            let Ok(doc) = self.agent(&session.agent).await else {
                continue;
            };
            let agent = doc.value;
            if agent.paused.is_some() {
                continue;
            }
            let Some(role) = agent.role.as_ref().and_then(|r| self.cfg.roles.get(r)) else {
                continue;
            };
            // The turns of the day, in KV so a move or a restart keeps them.
            let seen = mem
                .turns_seen
                .get(&agent.id)
                .copied()
                .filter(|seen| session.turn >= *seen)
                .unwrap_or(0);
            mem.turns_seen.insert(agent.id.clone(), session.turn);
            if let Some(cap) = role.max_turns_per_day {
                let key = turns_key(&agent.id, now);
                let doc = self.store.get::<u64>(&key).await?;
                let mut turns = doc.as_ref().map(|d| d.value).unwrap_or(0);
                if session.turn > seen {
                    turns += session.turn - seen;
                    self.store
                        .put(&key, &turns, doc.as_ref().map(|d| d.revision))
                        .await?;
                }
                if turns >= u64::from(cap) {
                    let why = format!("cap: {cap} turns today");
                    self.stop_for_cap(mem, &agent, why).await?;
                    continue;
                }
            }
            if let Some(hours) = role.max_session_hours {
                let start = match self
                    .store
                    .get::<Timestamp>(&session_start_key(&agent.id))
                    .await?
                {
                    Some(doc) => doc.value,
                    None => {
                        self.store
                            .upsert(&session_start_key(&agent.id), &now)
                            .await?;
                        now
                    }
                };
                if now.since(start) >= Duration::from_secs(u64::from(hours) * 3_600) {
                    let why = format!("cap: {hours}h session");
                    self.stop_for_cap(mem, &agent, why).await?;
                }
            }
        }
        Ok(())
    }

    /// The cap stop: the agent pauses with the cap as its reason, its claim
    /// goes to a person, and one event tells the swarm.
    async fn stop_for_cap(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        why: String,
    ) -> Result<(), ApiError> {
        self.stop_agent(
            mem,
            agent,
            Sender::Human,
            why.clone(),
            Duration::from_secs(DEFAULT_GRACE_SECS),
        )
        .await?;
        self.mark_claim_needs_human(&agent.id, &why).await?;
        Ok(())
    }

    /// Sets the task that the agent holds to `needs_human`, with the reason.
    async fn mark_claim_needs_human(&self, agent: &AgentId, why: &str) -> Result<(), ApiError> {
        let Ok(tasks) = self.all_tasks().await else {
            return Ok(());
        };
        let Some((channel, task)) = tasks.iter().find(|(_, task)| {
            task.claimed_by.as_ref() == Some(agent) && task.status == TaskStatus::InProgress
        }) else {
            return Ok(());
        };
        let key = keys::task(channel, &task.id);
        let Some(doc) = self.store.get::<Task>(&key).await? else {
            return Ok(());
        };
        let mut next = doc.value;
        next.status = TaskStatus::NeedsHuman;
        next.attention_msg = Some(why.to_owned());
        next.updated = self.now();
        self.store.put(&key, &next, Some(doc.revision)).await?;
        tracing::warn!(%agent, task = %next.id, "the claim of a capped agent needs a person");
        Ok(())
    }

    /// Stops each member of the channel whose channels are all paused now.
    async fn stop_channel(
        &self,
        mem: &mut Memory,
        name: &ChannelName,
        by: Sender,
        grace: Duration,
    ) -> Result<u32, ApiError> {
        self.set_channel_paused(name, true).await?;
        let mut stopped = 0;
        for member in self
            .store
            .list::<Member>(&keys::members_prefix(name))
            .await?
        {
            let Ok(doc) = self.agent(&member.value.agent).await else {
                continue;
            };
            let agent = doc.value;
            if agent.paused.is_some() || !self.all_channels_paused(&agent).await? {
                continue;
            }
            let why = format!("channel #{name} stopped");
            self.stop_agent(mem, &agent, by.clone(), why, grace).await?;
            stopped += 1;
        }
        tracing::info!(channel = %name, stopped, "channel paused");
        Ok(stopped)
    }

    /// Sets the `paused` flag of a channel.
    async fn set_channel_paused(&self, name: &ChannelName, paused: bool) -> Result<(), ApiError> {
        let key = keys::channel(name);
        let Some(doc) = self.store.get::<Channel>(&key).await? else {
            return Err(ApiError::not_found(format!("no channel {name}")));
        };
        let mut channel = doc.value;
        channel.paused = paused;
        self.store.put(&key, &channel, Some(doc.revision)).await?;
        Ok(())
    }

    /// Every channel of the agent is paused, and it has one.
    pub(crate) async fn all_channels_paused(&self, agent: &Agent) -> Result<bool, ApiError> {
        if agent.channels.is_empty() {
            return Ok(false);
        }
        for name in &agent.channels {
            let Some(doc) = self.store.get::<Channel>(&keys::channel(name)).await? else {
                continue;
            };
            if !doc.value.paused {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Clears the pause of one agent and places it again with a planned move.
    async fn resume_agent(&self, mem: &mut Memory, id: &AgentId) -> Result<bool, ApiError> {
        let mut agent = self.agent(id).await?.value;
        agent.paused = None;
        self.store.upsert(&keys::agent(id), &agent).await?;
        mem.pause_fences.retain(|_, (a, _, _)| a != id);
        let event = self
            .event(self.now(), Severity::Info, SwarmEventBody::AgentResumed)
            .about_agent(id.clone());
        Self::queue(mem, event);
        // The tick places it again when no host qualifies now.
        mem.pending_moves.insert(
            id.clone(),
            PendingMove {
                target: MoveTarget::default(),
                reason: MoveReason::Resume,
                prompt: None,
            },
        );
        let placed = self
            .place_agent(mem, &agent, MoveReason::Resume, MoveTarget::default(), None)
            .await?;
        tracing::info!(agent = %id, placed, "agent resumed");
        Ok(placed)
    }

    /// A person, or the owner of a channel of the target, may stop or resume.
    async fn authorize_pause(
        &self,
        actor: Option<&Actor>,
        selector: &PauseSelector,
    ) -> Result<(), ApiError> {
        let Some(actor) = actor else {
            return Ok(());
        };
        let owned = match selector {
            PauseSelector::Channel(name) => self.owns_channel(&actor.agent, name).await,
            PauseSelector::Agent(id) => {
                let agent = self.agent(id).await?.value;
                let mut owned = false;
                for name in &agent.channels {
                    if self.owns_channel(&actor.agent, name).await {
                        owned = true;
                        break;
                    }
                }
                owned
            }
        };
        if owned {
            Ok(())
        } else {
            Err(ApiError::denied(
                "only a person or the owner of the channel may stop or resume an agent",
            ))
        }
    }

    async fn owns_channel(&self, actor: &AgentId, name: &ChannelName) -> bool {
        self.channel(name)
            .await
            .map(|doc| doc.value.owner == *actor)
            .unwrap_or(false)
    }

    /// The held session of an agent, if one holds its lease.
    async fn held_session(
        &self,
        agent: &AgentId,
    ) -> Result<Option<(SessionId, HostName)>, ApiError> {
        Ok(self
            .store
            .get::<Lease>(&keys::lease_agent(agent))
            .await?
            .and_then(|doc| match doc.value.state {
                LeaseState::Held { session, host, .. } => Some((session, host)),
                _ => None,
            }))
    }

    /// The number of members of a channel.
    pub(crate) async fn channel_members(&self, name: &ChannelName) -> Result<u64, ApiError> {
        Ok(self
            .store
            .list::<Member>(&keys::members_prefix(name))
            .await?
            .len() as u64)
    }

    /// The reject reason of a spawn past the `max_agents` of its channel.
    pub(crate) fn channel_full(&self, name: &ChannelName, agents: u64) -> Option<String> {
        let max = self.cfg.channels.get(name)?.max_agents?;
        (agents >= u64::from(max)).then(|| format!("channel full ({agents}/{max})"))
    }
}

/// The error of a spawn into a paused channel.
pub(crate) fn channel_paused(name: &ChannelName) -> ApiError {
    ApiError::new(ApiErrorCode::Conflict, format!("channel {name} is paused"))
}
