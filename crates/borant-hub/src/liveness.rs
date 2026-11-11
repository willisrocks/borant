//! The liveness sweep (liveness spec, "Hub: the sweep").
//!
//! Every 60 s the hub looks for a spawn that its record still calls running
//! while its lease is gone: a dead session that no host reported, because the
//! hub was down when the host ended it, or because only the PID looked alive.
//! A death ends the session through the one session-end path, tells the owner
//! of the agent, and relaunches a movable worker that still holds a claim.
//!
//! The pass also reports silence, a spawn that stays queued, and forgets the
//! hosts that failed a launch longer than the lease grace ago.

use std::collections::HashMap;
use std::time::Duration;

use borant_core::model::{
    Agent, Lease, LeaseState, Sender, Session, SessionEventBody, SpawnRecord, SpawnState,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::{AgentId, Timestamp, keys};

use crate::leases::pinned_session_dead;
use crate::{Hub, Lists, Memory};

/// How often the liveness sweep runs.
pub(crate) const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// The newest checkpoint of an agent: the ref, its sha, and the branch that
/// was checked out when the host took it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub git_ref: String,
    pub sha: String,
    pub branch: Option<String>,
}

/// What the owner message names about the last session of an agent.
#[derive(Debug, Clone, Default)]
struct SessionFacts {
    last_event: Option<Timestamp>,
    checkpoint: Option<Checkpoint>,
}

/// True when a launch on `host` failed for `agent` within the lease grace, so
/// a placement still calls the host `RecentlyFailed`. An older failure no
/// longer blocks the host.
pub(crate) fn failed_recently(
    mem: &Memory,
    agent: &AgentId,
    host: &borant_core::HostName,
    now: Timestamp,
    cfg: &borant_core::rules::lease::LeaseConfig,
) -> bool {
    mem.failed_hosts
        .get(agent)
        .and_then(|hosts| hosts.get(host))
        .is_some_and(|at| now.since(*at) < cfg.grace)
}

/// True when a launch of any agent failed on `host` within the lease grace.
/// A host that fails launches (starved of CPU or memory, say) takes no new
/// agent for a while, whoever the agent is.
pub(crate) fn host_failing(
    mem: &Memory,
    host: &borant_core::HostName,
    now: Timestamp,
    cfg: &borant_core::rules::lease::LeaseConfig,
) -> bool {
    mem.failed_hosts
        .values()
        .filter_map(|hosts| hosts.get(host))
        .any(|at| now.since(*at) < cfg.grace)
}

/// The agents of a tick, by id.
pub(crate) fn agent_index(lists: &Lists) -> HashMap<&AgentId, &Agent> {
    lists
        .agents
        .iter()
        .map(|doc| (&doc.value.id, &doc.value))
        .collect()
}

/// The leases of a tick, by agent.
fn lease_index(lists: &Lists) -> HashMap<&AgentId, &Lease> {
    lists
        .leases
        .iter()
        .map(|doc| (&doc.value.agent, &doc.value))
        .collect()
}

impl Hub {
    /// Runs the sweep when its time has come, at most once a [`SWEEP_EVERY`].
    /// A failure is a warn, never a failed tick: the lease pass still runs.
    pub(crate) async fn tick_liveness(&self, lists: &Lists) -> Result<(), ApiError> {
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_sweep
                .is_some_and(|last| now.since(last) < SWEEP_EVERY)
            {
                return Ok(());
            }
            mem.last_sweep = Some(now);
        }
        if let Err(e) = self.liveness_pass(now, lists).await {
            tracing::warn!("liveness pass failed: {e}");
        }
        Ok(())
    }

    async fn liveness_pass(&self, now: Timestamp, lists: &Lists) -> Result<(), ApiError> {
        self.prune_failed_hosts(now).await;
        self.sweep_dead(now, lists).await?;
        self.report_silence(now, lists).await?;
        Ok(())
    }

    /// Forgets the launch failures that are older than the lease grace.
    async fn prune_failed_hosts(&self, now: Timestamp) {
        let grace = self.cfg.lease.grace;
        let mut mem = self.mem.lock().await;
        mem.failed_hosts.retain(|_, hosts| {
            hosts.retain(|_, at| now.since(*at) < grace);
            !hosts.is_empty()
        });
    }

    /// Ends every spawn that still says running while its session is dead.
    async fn sweep_dead(&self, now: Timestamp, lists: &Lists) -> Result<(), ApiError> {
        let records = self.store.list::<SpawnRecord>(keys::SPAWN_PREFIX).await?;
        let agents = agent_index(lists);
        let leases = lease_index(lists);
        for record in records {
            if record.value.state != SpawnState::Running {
                continue;
            }
            let step = {
                let mut mem = self.mem.lock().await;
                let step = self
                    .sweep_one(&mut mem, &record.value, now, &agents, &leases)
                    .await;
                drop(mem);
                self.flush().await;
                step
            };
            if let Err(e) = step {
                tracing::warn!(agent = %record.value.agent, "the dead-session check failed: {e}");
            }
        }
        Ok(())
    }

    /// One running spawn: end its session when the lease says no session can
    /// hold it any more, then leave the claim to the relaunch path.
    async fn sweep_one(
        &self,
        mem: &mut Memory,
        record: &SpawnRecord,
        now: Timestamp,
        agents: &HashMap<&AgentId, &Agent>,
        leases: &HashMap<&AgentId, &Lease>,
    ) -> Result<(), ApiError> {
        let agent = &record.agent;
        let Some(agent) = agents.get(agent).map(|a| (*a).clone()) else {
            return Ok(());
        };
        if agent.retired || agent.paused.is_some() {
            return Ok(());
        }
        let Some(lease) = leases.get(&agent.id).copied() else {
            return Ok(());
        };
        let LeaseState::Expired {
            since,
            last_session,
        } = &lease.state
        else {
            return Ok(());
        };
        // The heartbeat test reads the session of the last lease: a beat that
        // is newer than the expiry and leaves it out says the session is
        // gone. Every agent gets it, pinned or not. Without a host there is
        // no heartbeat to read, so the claim timeout is the evidence; the
        // grace alone is not, because a laptop that sleeps sends no beat and
        // its sessions reclaim their leases when it wakes.
        let dead = pinned_session_dead(mem, &agent, lease, now, &self.cfg.lease)
            || (agent.host.is_none() && now.since(*since) >= self.cfg.lease.claim_timeout);
        if !dead {
            return Ok(());
        }
        let Some(session) = (*last_session).or(agent.session) else {
            return Ok(());
        };
        let claims = self.in_progress_claims(&agent.id, &agent.channels).await?;
        tracing::warn!(agent = %agent.id, %session, "the liveness sweep found a dead session");
        // The owner hears the death before the relaunch message, so the two
        // read in the order that they happened.
        self.tell_owner_dead(&agent, &claims, "its session died")
            .await?;
        self.end_session_inner(
            mem,
            session,
            &agent.id,
            None,
            "the liveness sweep found the session dead",
            true,
        )
        .await?;
        Ok(())
    }

    /// True when the spawn of `agent` ended after its recovery limit, so only
    /// a person may launch it again.
    pub(crate) async fn recovery_exhausted(&self, agent: &AgentId) -> Result<bool, ApiError> {
        Ok(self
            .store
            .get::<SpawnRecord>(&keys::spawn(agent))
            .await?
            .is_some_and(|d| {
                d.value.state == SpawnState::Ended
                    && d.value.recoveries >= borant_core::model::MAX_SESSION_RECOVERIES
            }))
    }

    /// Starts the recovery count over, because a person resumed or moved the
    /// agent: the next death may relaunch it again.
    pub(crate) async fn reset_recoveries(&self, agent: &AgentId) -> Result<(), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok(());
        };
        if doc.value.recoveries == 0 {
            return Ok(());
        }
        let mut record = doc.value;
        record.recoveries = 0;
        match self.store.put(&key, &record, Some(doc.revision)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == ApiErrorCode::Conflict => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The last event time and the newest checkpoint of an agent.
    async fn session_facts(&self, agent: &AgentId) -> SessionFacts {
        let events = self.sessions.read(agent).await.unwrap_or_default();
        let last_event = match events.iter().map(|(_, e)| e.at).max() {
            Some(at) => Some(at),
            // A launched session whose first turn never started has no
            // event; its registration time is the last sign of life.
            None => self.session_started(agent).await,
        };
        let checkpoint = events.iter().rev().find_map(|(_, e)| match &e.body {
            SessionEventBody::Checkpoint {
                git_ref,
                sha,
                branch,
            } => Some(Checkpoint {
                git_ref: git_ref.clone(),
                sha: sha.clone(),
                branch: branch.clone(),
            }),
            _ => None,
        });
        SessionFacts {
            last_event,
            checkpoint,
        }
    }

    /// When the last session of the agent started.
    async fn session_started(&self, agent: &AgentId) -> Option<Timestamp> {
        let session = self
            .store
            .get::<Agent>(&keys::agent(agent))
            .await
            .ok()
            .flatten()
            .and_then(|d| d.value.session)?;
        self.store
            .get::<Session>(&keys::session(&session))
            .await
            .ok()
            .flatten()
            .map(|d| d.value.started)
    }

    /// The owner of an agent: its spawner, else its parent, else the owner of
    /// the channel of its bound task, or of its first channel.
    pub(crate) async fn owner_of(&self, agent: &Agent) -> Result<Option<AgentId>, ApiError> {
        if let Some(doc) = self
            .store
            .get::<SpawnRecord>(&keys::spawn(&agent.id))
            .await?
            && let Sender::Agent(spawner) = &doc.value.by
            && spawner != &agent.id
            && let Some(doc) = self.store.get::<Agent>(&keys::agent(spawner)).await?
            && !doc.value.retired
        {
            return Ok(Some(spawner.clone()));
        }
        if let Some(parent) = &agent.parent
            && let Some(doc) = self.store.get::<Agent>(&keys::agent(parent)).await?
            && !doc.value.retired
        {
            return Ok(Some(parent.clone()));
        }
        let channels = match &agent.task {
            Some(task) => std::slice::from_ref(&task.channel),
            None => agent.channels.as_slice(),
        };
        for channel in channels {
            if let Ok(doc) = self.channel(channel).await
                && let Some(owner) = &doc.value.owner
                && owner != &agent.id
            {
                return Ok(Some(owner.clone()));
            }
        }
        Ok(None)
    }

    /// One direct message to the owner of a dead agent. It names the agent,
    /// the bound task, the last event, and the newest checkpoint.
    pub(crate) async fn tell_owner_dead(
        &self,
        agent: &Agent,
        claims: &[(borant_core::ChannelName, borant_core::model::Task)],
        why: &str,
    ) -> Result<(), ApiError> {
        let Some(owner) = self.owner_of(agent).await? else {
            return Ok(());
        };
        let facts = self.session_facts(&agent.id).await;
        let mut text = format!("agent {} died: {why}.", agent.id);
        text.push_str(&self.task_line(agent, claims));
        text.push_str(&self.facts_line(&facts));
        if !claims.is_empty() {
            text.push_str(" The hub is relaunching it from its checkpoint.");
        }
        if let Err(e) = self.tell(&owner, &text).await {
            tracing::warn!(agent = %agent.id, %owner, "the death message failed: {e}");
        }
        Ok(())
    }

    /// One direct message per relaunch, to the owner of the agent.
    pub(crate) async fn tell_owner_relaunch(
        &self,
        agent: &Agent,
        claims: &[(borant_core::ChannelName, borant_core::model::Task)],
        placed: bool,
        failure: Option<&str>,
    ) -> Result<(), ApiError> {
        let Some(owner) = self.owner_of(agent).await? else {
            return Ok(());
        };
        let facts = self.session_facts(&agent.id).await;
        let mut text = if placed {
            format!("agent {} was relaunched from its checkpoint.", agent.id)
        } else {
            format!(
                "agent {} could not be relaunched: {}",
                agent.id,
                failure.unwrap_or("no host fits")
            )
        };
        text.push_str(&self.task_line(agent, claims));
        text.push_str(&self.facts_line(&facts));
        if let Err(e) = self.tell(&owner, &text).await {
            tracing::warn!(agent = %agent.id, %owner, "the relaunch message failed: {e}");
        }
        Ok(())
    }

    fn task_line(
        &self,
        agent: &Agent,
        claims: &[(borant_core::ChannelName, borant_core::model::Task)],
    ) -> String {
        let bound = agent.task.as_ref().map(|t| (t.channel.clone(), t.id));
        let task = claims
            .iter()
            .find(|(channel, task)| {
                bound
                    .as_ref()
                    .is_some_and(|(c, id)| c == channel && id == &task.id)
            })
            .or_else(|| claims.first());
        match task {
            Some((channel, task)) => format!(" Task #{channel}/{} ({}).", task.id, task.title),
            None => match bound {
                Some((channel, id)) => format!(" Task #{channel}/{id}."),
                None => " No bound task.".to_owned(),
            },
        }
    }

    fn facts_line(&self, facts: &SessionFacts) -> String {
        let mut out = String::new();
        match facts.last_event {
            Some(at) => out.push_str(&format!(" Last event at {at}.")),
            None => out.push_str(" No session event was recorded."),
        }
        match &facts.checkpoint {
            Some(cp) => {
                out.push_str(&format!(
                    " Checkpoint {} sha {} branch {}.",
                    cp.git_ref,
                    cp.sha,
                    cp.branch.as_deref().unwrap_or("-")
                ));
            }
            None => out.push_str(" No checkpoint yet."),
        }
        out
    }

    /// The silence report: an agent that holds its lease, holds an
    /// in-progress claim, and wrote no session event for `stall_minutes`.
    /// Once per silence, and never a kill.
    async fn report_silence(&self, now: Timestamp, lists: &Lists) -> Result<(), ApiError> {
        let stall = Duration::from_secs(self.cfg.alerts.stall_minutes.saturating_mul(60));
        let agents = agent_index(lists);
        for lease in &lists.leases {
            let LeaseState::Held { host, session, .. } = &lease.value.state else {
                continue;
            };
            let Some(agent) = agents.get(&lease.value.agent).map(|a| (*a).clone()) else {
                continue;
            };
            if agent.retired || agent.paused.is_some() {
                continue;
            }
            let claims = self.in_progress_claims(&agent.id, &agent.channels).await?;
            if claims.is_empty() {
                continue;
            }
            let facts = self.session_facts(&agent.id).await;
            let last = match facts.last_event {
                Some(at) => at,
                None => {
                    // A session whose first turn never started: the clock
                    // runs from its registration.
                    let Some(started) = self
                        .store
                        .get::<Session>(&keys::session(session))
                        .await?
                        .map(|d| d.value.started)
                    else {
                        continue;
                    };
                    started
                }
            };
            if now.since(last) < stall {
                continue;
            }
            {
                let mut mem = self.mem.lock().await;
                if mem.silence_dmed.get(&agent.id) == Some(&last) {
                    continue;
                }
                mem.silence_dmed.insert(agent.id.clone(), last);
            }
            let Some(owner) = self.owner_of(&agent).await? else {
                continue;
            };
            let mut text = format!(
                "agent {} is silent: no session event for {} minutes on {host}.",
                agent.id, self.cfg.alerts.stall_minutes
            );
            text.push_str(&self.task_line(&agent, &claims));
            text.push_str(&self.facts_line(&facts));
            text.push_str(
                " It was not stopped; a live process may still be thinking. \
                 Look at its pane if its first turn never started.",
            );
            if let Err(e) = self.tell(&owner, &text).await {
                tracing::warn!(agent = %agent.id, %owner, "the silence message failed: {e}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::HostName;
    use borant_core::rules::lease::LeaseConfig;
    use std::collections::HashMap;

    #[test]
    fn a_failed_host_expires_after_the_grace() {
        let cfg = LeaseConfig::default();
        let agent: AgentId = "w1".parse().unwrap();
        let host: HostName = "h1".parse().unwrap();
        let at = Timestamp(1_000_000);
        let mut mem = Memory::default();
        mem.failed_hosts
            .insert(agent.clone(), HashMap::from([(host.clone(), at)]));

        let within = Timestamp(at.plus(cfg.grace).0 - 1);
        assert!(failed_recently(&mem, &agent, &host, within, &cfg));

        let after = at.plus(cfg.grace);
        assert!(!failed_recently(&mem, &agent, &host, after, &cfg));
    }
}
