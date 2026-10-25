//! Stop on close (phase 3 spec, "Stop on close"). A worker bound to a task
//! that closes is retired: no placement, no wake, no parking, its claims go
//! back to open, and its session stops at its next turn end.

use borant_core::TaskId;
use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::Agent;
use borant_core::proto::{ApiError, HostCommand, SessionLeaseRef};
use borant_core::{AgentId, ChannelName, HostName, SessionId, keys, subjects};

use crate::{Hub, Memory};

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
    /// takes it again, and its session stops now when it is idle, else at its
    /// next turn end (see [`Hub::stop_retired`]).
    pub(crate) async fn retire(&self, mem: &mut Memory, agent: &Agent) -> Result<(), ApiError> {
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
        // A session that already ended its turn stops at once; the host stops
        // one that is still in a turn when the turn ends.
        let idle = mem
            .idle
            .get(&agent.id)
            .filter(|report| report.since.is_some())
            .cloned();
        if let Some(report) = idle {
            self.stop_session(mem, &agent.id, report.session, &report.host)
                .await?;
        }
        // The clone is the biggest thing a retired agent leaves behind, but
        // the host deletes it only when no session of the agent runs there.
        // The heartbeat sends the order once the session is gone.
        if let Some(host) = &agent.host {
            mem.delete_pending.insert(agent.id.clone(), host.clone());
        }
        Ok(())
    }

    /// Stops the sessions of retired agents that just ended a turn. The
    /// heartbeat carries the end of the last turn of each session.
    pub(crate) async fn stop_retired(
        &self,
        mem: &mut Memory,
        host: &HostName,
        sessions: &[SessionLeaseRef],
    ) -> Result<(), ApiError> {
        for s in sessions {
            // `idle_since` is `None` while the session is in a turn.
            if s.idle_since.is_none() || mem.stopped.contains(&s.session) {
                continue;
            }
            let Ok(doc) = self.agent(&s.agent).await else {
                continue;
            };
            if !doc.value.retired {
                continue;
            }
            self.stop_session(mem, &s.agent, s.session, host).await?;
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
