//! Stop on close (phase 3 spec, "Stop on close"). A worker bound to a task
//! that closes is retired: no placement, no wake, no parking, its claims go
//! back to open, and its session stops at its next turn end, or at
//! [`TURN_END_GRACE`] when the hub never learns of one.

use std::time::Duration;

use borant_core::TaskId;
use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, Channel};
use borant_core::proto::{ApiError, HostCommand, SessionLeaseRef};
use borant_core::{AgentId, ChannelName, HostName, SessionId, Timestamp, keys, subjects};

use crate::{Hub, Memory};

/// How long a retired session that the hub has no turn state for gets before
/// it is stopped anyway. The heartbeat's `SessionStats` carries the `busy` of
/// each session, which `TurnStart`/`TurnEnd` set, so this safety grace covers
/// only a session that the host reports no stats for; waiting for a turn end
/// of such a session could keep its lease and its clone for ever.
pub(crate) const UNKNOWN_TURN_GRACE: Duration = Duration::from_secs(600);

/// A retired agent that still waits for its stop: the host that runs its
/// session, and when the retire that asked for the stop happened, so the
/// safety grace has a start.
#[derive(Debug, Clone)]
pub(crate) struct StopWait {
    pub host: HostName,
    pub since: Timestamp,
}

impl Hub {
    /// Whether the last heartbeat says that the session of an agent runs a
    /// turn, from the `busy` of its `SessionStats`. `None` means that the host
    /// reported no stats for the session, so the hub cannot know its turn
    /// state at all.
    fn session_busy(mem: &Memory, host: &HostName, agent: &AgentId) -> Option<bool> {
        mem.host_stats
            .get(host)?
            .sessions
            .iter()
            .find(|s| &s.agent == agent)
            .map(|s| s.busy)
    }

    /// Retires every agent that the closed task bound and that stops on close.
    /// Returns the agents that were retired.
    pub(crate) async fn retire_for_task(
        &self,
        mem: &mut Memory,
        channel: &ChannelName,
        task: &TaskId,
    ) -> Result<Vec<AgentId>, ApiError> {
        let mut retired = Vec::new();
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            if agent.retired || !agent.stop_on_close {
                continue;
            }
            let Some(bound) = agent.task.as_ref() else {
                continue;
            };
            if bound.channel != *channel || bound.id != *task {
                continue;
            }
            self.retire(mem, &agent).await?;
            retired.push(agent.id);
        }
        Ok(retired)
    }

    /// Retires one agent: its claims go back to open, no placement or wake
    /// takes it again, and its session stops at once when it is not busy, else
    /// at its next turn end (see [`Hub::stop_retired`]). A session that the
    /// host reports no stats for stops at [`UNKNOWN_TURN_GRACE`]. Returns
    /// whether a stop order went out now.
    pub(crate) async fn retire(&self, mem: &mut Memory, agent: &Agent) -> Result<bool, ApiError> {
        let now = self.now();
        let released = self.unclaim_everywhere(mem, &agent.id).await?;
        let mut saved = agent.clone();
        saved.retired = true;
        self.store.upsert(&keys::agent(&agent.id), &saved).await?;
        // A channel that this agent owned passes to the agent that spawned
        // it, when that agent is alive and not retired. With no heir the
        // channel has no owner, and only a person closes its tasks or takes
        // it. One event per channel says who owns it now.
        let heir = self.owner_heir(agent).await?;
        let owned: Vec<(String, Channel, u64)> = self
            .store
            .list::<serde_json::Value>(keys::CHANNEL_PREFIX)
            .await?
            .into_iter()
            .filter(|d| d.key.matches('.').count() == 1)
            .filter_map(|d| {
                let ch = serde_json::from_value::<Channel>(d.value).ok()?;
                (ch.owner.as_ref() == Some(&agent.id)).then_some((d.key, ch, d.revision))
            })
            .collect();
        for (key, mut ch, revision) in owned {
            ch.owner = heir.clone();
            self.store.put(&key, &ch, Some(revision)).await?;
            let event = self
                .event(
                    now,
                    Severity::Info,
                    SwarmEventBody::ChannelOwner {
                        previous: agent.id.clone(),
                        owner: ch.owner.clone(),
                    },
                )
                .about_channel(ch.name.clone());
            Self::queue(mem, event);
        }
        // A retired agent has no demand: no pool host is created for it.
        mem.pending_moves.remove(&agent.id);
        tracing::info!(agent = %agent.id, claims = released, "agent retired with its bound task");
        let event = self
            .event(now, Severity::Info, SwarmEventBody::AgentRetired)
            .about_agent(agent.id.clone());
        Self::queue(mem, event);
        // The session that has to go: the one of the last heartbeat report,
        // else the one that the agent row names, so a session that no
        // heartbeat ever reported still gets its stop.
        let report = mem.idle.get(&agent.id).cloned();
        let session = report.as_ref().map(|r| r.session).or(agent.session);
        let host = report
            .as_ref()
            .map(|r| r.host.clone())
            .or_else(|| agent.host.clone());
        let mut stopped = false;
        if let (Some(session), Some(host)) = (session, host) {
            // `busy` comes from the last heartbeat, and `idle_since` from the
            // last turn end; either one says that no turn runs now.
            let quiet = report.as_ref().is_some_and(|r| r.since.is_some())
                || Self::session_busy(mem, &host, &agent.id) == Some(false);
            if mem.stopped.contains(&session) {
                // A repeat retire: the stop went out already.
                stopped = true;
            } else if quiet {
                self.stop_session(mem, &agent.id, session, &host).await?;
                stopped = true;
            } else {
                // A turn runs, or the hub has no stats for the session: the
                // heartbeat stops it at the turn end, and at the safety grace
                // when no stats ever arrive. The first retire keeps the start.
                mem.stop_pending
                    .entry(agent.id.clone())
                    .or_insert(StopWait { host, since: now });
            }
        }
        // The clone is the biggest thing a retired agent leaves behind, but
        // the host deletes it only when no session of the agent runs there.
        // The heartbeat sends the order once the session is gone.
        if let Some(host) = &agent.host {
            mem.delete_pending.insert(agent.id.clone(), host.clone());
        }
        Ok(stopped)
    }

    /// The agent that takes over the channels of a retired owner: its
    /// spawner, when that agent is still here and not retired itself.
    async fn owner_heir(&self, agent: &Agent) -> Result<Option<AgentId>, ApiError> {
        let Some(parent) = &agent.parent else {
            return Ok(None);
        };
        let Some(doc) = self.store.get::<Agent>(&keys::agent(parent)).await? else {
            return Ok(None);
        };
        Ok((!doc.value.retired).then(|| parent.clone()))
    }

    /// Stops the sessions of retired agents that are not busy, or that the hub
    /// has no turn state for, and frees the work dir of those whose session
    /// the host no longer reports. The heartbeat carries the end of the last
    /// turn of each session, and the `busy` of each session.
    pub(crate) async fn stop_retired(
        &self,
        mem: &mut Memory,
        host: &HostName,
        sessions: &[SessionLeaseRef],
    ) -> Result<(), ApiError> {
        let now = self.now();
        for s in sessions {
            if mem.stopped.contains(&s.session) {
                continue;
            }
            let Ok(doc) = self.agent(&s.agent).await else {
                continue;
            };
            if !doc.value.retired {
                continue;
            }
            // A session that is not in a turn stops now: `idle_since` from its
            // last turn end, or `busy: false` from the host. A busy session
            // stops at its turn end, and one that the host reports no stats
            // for stops at the safety grace, so no retired session waits for a
            // turn end that no host will ever report.
            let busy = Self::session_busy(mem, host, &s.agent);
            let due = s.idle_since.is_some()
                || busy == Some(false)
                || (busy.is_none()
                    && mem.stop_pending.get(&s.agent).is_some_and(|w| {
                        w.host == *host && now.since(w.since) >= UNKNOWN_TURN_GRACE
                    }));
            if !due {
                continue;
            }
            self.stop_session(mem, &s.agent, s.session, host).await?;
            mem.stop_pending.remove(&s.agent);
        }
        // A retired agent whose session the host no longer reports has ended
        // it, so its work dir can go.
        let waiting: Vec<AgentId> = mem
            .delete_pending
            .iter()
            .filter(|(agent, at)| *at == host && !sessions.iter().any(|s| s.agent == **agent))
            .map(|(agent, _)| agent.clone())
            .collect();
        for agent in waiting {
            self.delete_work(host, &agent).await?;
            mem.delete_pending.remove(&agent);
        }
        Ok(())
    }

    /// Tells the host to delete the work dir of a retired agent. The host
    /// deletes it only when no session of the agent runs there, so this is
    /// safe to send before a session has ended.
    async fn delete_work(&self, host: &HostName, agent: &AgentId) -> Result<(), ApiError> {
        let payload = serde_json::to_vec(&HostCommand::DeleteWork {
            agent: agent.clone(),
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
        self.bus
            .send(&subjects::host_cmd(host), payload.into())
            .await?;
        tracing::info!(%agent, %host, "delete-work order sent for a retired agent");
        Ok(())
    }

    /// Sends one stop order to the host of the session, once.
    pub(crate) async fn stop_session(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        session: SessionId,
        host: &HostName,
    ) -> Result<(), ApiError> {
        let payload = serde_json::to_vec(&HostCommand::Stop { session })
            .map_err(|e| ApiError::internal(e.to_string()))?;
        self.bus
            .send(&subjects::host_cmd(host), payload.into())
            .await?;
        mem.stopped.insert(session);
        tracing::info!(%agent, %session, %host, "stop order sent to a retired agent");
        Ok(())
    }
}
