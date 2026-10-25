//! Stop on close (phase 3 spec, "Stop on close"). A worker bound to a task
//! that closes is retired: no placement, no wake, no parking, its claims go
//! back to open, and its session stops at its next turn end, or at
//! [`TURN_END_GRACE`] when the hub never learns of one.

use std::time::Duration;

use borant_core::TaskId;
use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::Agent;
use borant_core::proto::{ApiError, HostCommand, SessionLeaseRef};
use borant_core::{AgentId, ChannelName, HostName, SessionId, Timestamp, keys, subjects};

use crate::{Hub, Memory};

/// How long a retired session that the hub cannot place in a turn gets before
/// it is stopped anyway. `idle_since` is `None` both for a session in a turn
/// and for a session that never reported a turn end (an omp session that waits
/// for its first prompt), so waiting for a turn end that never comes would keep
/// the session, its lease, and its clone for ever.
pub(crate) const TURN_END_GRACE: Duration = Duration::from_secs(30);

/// A retired agent that still waits for its stop: the host that runs its
/// session, and when the retire that asked for the stop happened, so the grace
/// has a start.
#[derive(Debug, Clone)]
pub(crate) struct StopWait {
    pub host: HostName,
    pub since: Timestamp,
}

impl Hub {
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
    /// takes it again, and its session stops at the next turn end, or at
    /// [`TURN_END_GRACE`] when the hub never learns of one (see
    /// [`Hub::stop_retired`]). Returns whether a stop order went out now.
    pub(crate) async fn retire(&self, mem: &mut Memory, agent: &Agent) -> Result<bool, ApiError> {
        let now = self.now();
        let released = self.unclaim_everywhere(mem, &agent.id).await?;
        let mut saved = agent.clone();
        saved.retired = true;
        self.store.upsert(&keys::agent(&agent.id), &saved).await?;
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
            if mem.stopped.contains(&session) {
                // A repeat retire: the stop went out already.
                stopped = true;
            } else if report.as_ref().is_some_and(|r| r.since.is_some()) {
                // The turn already ended, so the session stops at once.
                self.stop_session(mem, &agent.id, session, &host).await?;
                stopped = true;
            } else {
                // Unknown turn state: the heartbeat stops the session at the
                // turn end, and at the grace when no turn end ever comes. The
                // first retire keeps the start of the grace.
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

    /// Stops the sessions of retired agents that ended a turn, or that the hub
    /// cannot place in one, and frees the work dir of those whose session the
    /// host no longer reports. The heartbeat carries the end of the last turn
    /// of each session.
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
            // `idle_since` is `None` while the session is in a turn, and for a
            // session that never ended one (an omp session that waits for its
            // first prompt). The grace stops the second case instead of
            // waiting for a turn end that never comes.
            let due = s.idle_since.is_some()
                || mem
                    .stop_pending
                    .get(&s.agent)
                    .is_some_and(|w| w.host == *host && now.since(w.since) >= TURN_END_GRACE);
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
    async fn stop_session(
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
