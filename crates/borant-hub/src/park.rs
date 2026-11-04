//! Parking idle agents (spec "Parking").
//!
//! The host reports the end of the last turn of each session in its
//! heartbeat. An agent that stays idle past the timeout of its pool, holds no
//! task, has no unacked message, and can wake (movable, with channels) is
//! parked: the host writes a final checkpoint and stops the session, and the
//! lease becomes a parked release that a message or a claim wakes again.
//!
//! A `helper` or `worker` gives its slot back the same way on any host, pool
//! or not, after the `idle_release_after` of its role, so a finished agent
//! that nobody cancelled does not hold a laptop.

use std::collections::BTreeSet;
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, HostInfo, Lease, LeaseState, Task, TaskStatus};
use borant_core::proto::{ApiError, HostCommand};
use borant_core::rules::roles;
use borant_core::{AgentId, HostName, PoolName, SessionId, Timestamp, keys, subjects};

use crate::Hub;

/// The idle timeout of a pool host, until the hub config names one for each
/// pool.
pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// How many heartbeats an idle signal stays trustworthy. A host that stops
/// reporting cannot prove that its session is still idle.
const IDLE_REPORT_HEARTBEATS: u32 = 3;

/// How long the hub waits before it sends another park for one agent, so a
/// host that keeps a session with a failed checkpoint is not flooded.
const PARK_RETRY: Duration = Duration::from_secs(60);

impl Hub {
    /// Parks the agents that stayed idle past `idle_timeout` and returns them.
    /// Only the hosts of `pool` are considered, so one pool's timeout never
    /// parks another pool's agents; `None` accepts every host that has a
    /// pool, for a hub whose `hub.toml` names no `[[pools]]`. Public for
    /// tests.
    pub async fn park_idle(
        &self,
        pool: Option<&PoolName>,
        idle_timeout: Duration,
    ) -> Result<Vec<AgentId>, ApiError> {
        let mut parked = Vec::new();
        if self.mem.lock().await.maintenance {
            return Ok(parked);
        }
        let pools = self.pool_hosts(pool).await?;
        if pools.is_empty() {
            return Ok(parked);
        }
        let now = self.now();
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            // Parking is for agents that come back; a retired or paused one does not.
            if agent.retired || agent.paused.is_some() {
                continue;
            }
            let Some(host) = agent.host.clone().filter(|h| pools.contains(h)) else {
                continue;
            };
            if !agent.movable || agent.channels.is_empty() {
                continue;
            }
            let Some(session) = self.idle_session(&agent, &host, now, idle_timeout).await? else {
                continue;
            };
            if self.parked_recently(&agent.id, now).await {
                continue;
            }
            // A claim, a message that waits for the agent, or a claim that
            // waits to be taken means the agent is not done.
            if self.has_claims(&agent.id, &agent.channels).await? {
                continue;
            }
            if !self
                .bus
                .unacked(&agent.id)
                .await
                .unwrap_or_default()
                .is_empty()
            {
                continue;
            }
            self.order_park(&agent, &host, session, now).await?;
            parked.push(agent.id);
        }
        Ok(parked)
    }

    /// Gives back the slot of each idle `helper` or `worker` on any host, pool
    /// or not, after the `idle_release_after` of its role (spec "Parking").
    /// The agent takes the pool park path: a final checkpoint, the session
    /// stopped, and a parked release that a message or a claim wakes. An
    /// agent with a bound open task, a claim, an unacked message, or a
    /// running tool keeps its slot, and so does one that cannot wake again.
    /// Returns the agents it released.
    pub async fn park_idle_roles(&self) -> Result<Vec<AgentId>, ApiError> {
        let mut parked = Vec::new();
        if self.mem.lock().await.maintenance {
            return Ok(parked);
        }
        let now = self.now();
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            if agent.retired || agent.paused.is_some() || agent.pinned {
                continue;
            }
            let Some(idle_timeout) = roles::idle_release(agent.role.as_ref(), &self.cfg.roles)
            else {
                continue;
            };
            // A release wakes on a message or a claim, on some host, so an
            // agent that no placement takes gets no release.
            let Some(host) = agent.host.clone() else {
                continue;
            };
            if !agent.movable || agent.channels.is_empty() {
                continue;
            }
            let Some(session) = self.idle_session(&agent, &host, now, idle_timeout).await? else {
                continue;
            };
            if self.parked_recently(&agent.id, now).await {
                continue;
            }
            // Work in progress keeps the slot: a claim, a bound task that is
            // still open, a tool in flight, or a message that waits.
            if self.has_claims(&agent.id, &agent.channels).await? {
                continue;
            }
            if self.bound_task_is_open(&agent).await? {
                continue;
            }
            if self.tool_running(&agent).await {
                continue;
            }
            if !self
                .bus
                .unacked(&agent.id)
                .await
                .unwrap_or_default()
                .is_empty()
            {
                continue;
            }
            self.order_park(&agent, &host, session, now).await?;
            parked.push(agent.id);
        }
        Ok(parked)
    }

    /// Orders the park of `session` on `host`, keeps the time so that a host
    /// whose final checkpoint fails is asked again only after [`PARK_RETRY`],
    /// and publishes the event. The checkpoint ref is the host's report.
    async fn order_park(
        &self,
        agent: &Agent,
        host: &HostName,
        session: SessionId,
        now: Timestamp,
    ) -> Result<(), ApiError> {
        let payload = serde_json::to_vec(&HostCommand::Park { session })
            .map_err(|e| ApiError::internal(e.to_string()))?;
        self.bus
            .send(&subjects::host_cmd(host), payload.into())
            .await?;
        self.mem
            .lock()
            .await
            .park_sent
            .insert(agent.id.clone(), now);
        tracing::info!(agent = %agent.id, %host, %session, "park order sent");
        let event = self
            .event(
                now,
                Severity::Info,
                SwarmEventBody::AgentParked {
                    session,
                    git_ref: None,
                },
            )
            .about_agent(agent.id.clone())
            .about_host(host.clone());
        self.publish(event).await;
        Ok(())
    }

    /// True when the agent is bound to a task that is still open, so its work
    /// is not done. A bound task that another agent took still binds it.
    async fn bound_task_is_open(&self, agent: &Agent) -> Result<bool, ApiError> {
        let Some(bound) = agent.task.as_ref() else {
            return Ok(false);
        };
        let Some(doc) = self
            .store
            .get::<Task>(&keys::task(&bound.channel, &bound.id))
            .await?
        else {
            return Ok(false);
        };
        Ok(doc.value.status != TaskStatus::Closed)
    }

    /// True when the latest activity of the session says that a tool runs.
    async fn tool_running(&self, agent: &Agent) -> bool {
        self.mem
            .lock()
            .await
            .activity
            .get(&agent.id)
            .is_some_and(|r| {
                Some(r.session) == agent.session && r.activity.tool_running_since.is_some()
            })
    }

    /// The names of the hosts that belong to `pool`, or of every host that
    /// belongs to a pool when `pool` is `None`.
    async fn pool_hosts(&self, pool: Option<&PoolName>) -> Result<BTreeSet<HostName>, ApiError> {
        Ok(self
            .store
            .list::<HostInfo>(keys::HOST_PREFIX)
            .await?
            .into_iter()
            .filter(|d| match (pool, &d.value.pool) {
                (Some(want), Some(have)) => want == have,
                (None, have) => have.is_some(),
                (Some(_), None) => false,
            })
            .map(|d| d.value.name)
            .collect())
    }

    /// The session to park, when the agent is an idle one on `host`.
    async fn idle_session(
        &self,
        agent: &Agent,
        host: &HostName,
        now: Timestamp,
        idle_timeout: Duration,
    ) -> Result<Option<SessionId>, ApiError> {
        let report = self.mem.lock().await.idle.get(&agent.id).cloned();
        let Some(report) = report else {
            return Ok(None);
        };
        let fresh = now.since(report.at) <= self.cfg.lease.heartbeat * IDLE_REPORT_HEARTBEATS;
        if &report.host != host || !fresh {
            return Ok(None);
        }
        // `None` means the session is in a turn.
        let Some(since) = report.since else {
            return Ok(None);
        };
        if now.since(since) <= idle_timeout || agent.session != Some(report.session) {
            return Ok(None);
        }
        let Some(doc) = self
            .store
            .get::<Lease>(&keys::lease_agent(&agent.id))
            .await?
        else {
            return Ok(None);
        };
        let held = matches!(
            &doc.value.state,
            LeaseState::Held { session, host: h, .. } if *session == report.session && h == host
        );
        Ok(held.then_some(report.session))
    }

    async fn parked_recently(&self, agent: &AgentId, now: Timestamp) -> bool {
        self.mem
            .lock()
            .await
            .park_sent
            .get(agent)
            .is_some_and(|sent| now.since(*sent) < PARK_RETRY)
    }
}
