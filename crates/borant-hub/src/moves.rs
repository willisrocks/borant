//! Placement, planned moves, drains, and spawns (spec sections 13 and 14).

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use borant_core::events::{RejectedHost, Severity, SwarmEventBody};
use borant_core::model::{
    Agent, Harness, HostInfo, LaunchFailure, Lease, LeaseState, MemberState, MoveReason, Project,
    Sender, Session, SessionEvent, SessionEventBody, SpawnRecord, SpawnState, launch_error_text,
};
use borant_core::proto::{
    ApiError, ApiErrorCode, HostCommand, LaunchOrder, MoveSelector, MoveTarget,
};
use borant_core::rules::lease;
use borant_core::rules::redact;
use borant_core::rules::roles;
use borant_core::rules::schedule::{self, HostView, PlacementReq, Reject};
use borant_core::{
    AgentId, BundleName, CollectionName, EventId, FencingToken, HostName, MemberName, SessionId,
    Timestamp, keys, subjects,
};
use serde_json::{Value, json};

use crate::members::Assign;
use crate::{Hub, Memory, PendingMove};

/// The launches of one spawn that the hub counts before it gives up.
const MAX_LAUNCH_TRIES: u32 = 3;

/// How long an unchanged refusal waits before the hub publishes it again, so
/// a spawn that no host can take does not fill the event log.
const REFUSAL_REPEAT: Duration = Duration::from_secs(60);

/// The first backoff of a queued spawn whose hosts are all full or offline.
/// Each refusal doubles it, up to [`REFUSAL_BACKOFF_MAX`].
const REFUSAL_BACKOFF: Duration = Duration::from_secs(1);

/// The longest backoff of a queued spawn that no host has room for.
const REFUSAL_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The last refused placement of an agent: what refused it, when the hub
/// published it, and when the spawn queue tries it again. A refusal that is
/// unchanged publishes once a [`REFUSAL_REPEAT`], and a spawn whose hosts are
/// all full or offline backs off for [`PlacementRefusal::tries`] doublings.
#[derive(Debug, Clone)]
pub(crate) struct PlacementRefusal {
    /// The refused hosts, with the first rule that refused each.
    pub rejected: Vec<(HostName, Reject)>,
    /// When the hub last published the refusal, on the hub clock.
    pub told_at: Timestamp,
    /// When the queue tries the agent again, on the hub clock.
    pub retry_at: Timestamp,
    /// The refusals in a row that set `retry_at`.
    pub tries: u32,
}

/// True when two refusals name the same hosts with the same rules. The order
/// of the hosts follows the host list, which may change with a restart.
pub(crate) fn same_refusal(a: &[(HostName, Reject)], b: &[(HostName, Reject)]) -> bool {
    a.len() == b.len() && a.iter().all(|entry| b.contains(entry))
}

/// True when every host refused for a reason that a free slot or a host that
/// comes back fixes, so the spawn waits instead of trying each tick.
fn capacity_refusal(rejected: &[(HostName, Reject)]) -> bool {
    rejected
        .iter()
        .all(|(_, why)| matches!(why, Reject::Full { .. } | Reject::Offline))
}

/// True when no host takes an explicit move now, and a new pool host or a free
/// slot would fix every refusal: the move waits as demand, so the pool orders
/// a host, instead of failing the move or the drain.
fn move_waits_for_a_host(rejected: &[(HostName, Reject)]) -> bool {
    rejected.iter().all(|(_, why)| {
        matches!(why, Reject::Full { .. }) || borant_core::rules::pool::fixable_reason(why)
    })
}

/// The hosts of one placement pass, with the agents on each: the host and
/// lease lists read once, so a pass over many spawns does not list them once
/// per spawn.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlacementPass {
    views: Vec<HostView>,
}

impl PlacementPass {
    /// The views for `agent`, with the hosts where its launch failed marked,
    /// so a refusal says `RecentlyFailed` instead of blaming the clone. It
    /// borrows the pass unless the agent has a failed host.
    pub(crate) fn for_agent<'a>(
        &'a self,
        hub: &Hub,
        mem: &Memory,
        agent: &AgentId,
    ) -> Cow<'a, [HostView]> {
        // The pass lists the hosts once, with the failures of every agent.
        // The flags of this agent come from the memory of now, because an
        // earlier try of the same spawn may have cleared its own failures.
        if mem.failed_hosts.is_empty() && self.views.iter().all(|v| !v.launches_failing) {
            return Cow::Borrowed(&self.views);
        }
        let now = hub.now();
        let mut views = self.views.clone();
        for v in &mut views {
            v.recently_failed =
                crate::liveness::failed_recently(mem, agent, &v.info.name, now, &hub.cfg.lease);
            v.launches_failing =
                crate::liveness::host_failing(mem, Some(agent), &v.info.name, now, &hub.cfg.lease);
        }
        Cow::Owned(views)
    }

    /// Counts the agent that the placement just reserved on `host`, so the
    /// next spawn of the pass does not see the slot as free.
    pub(crate) fn reserved(&mut self, host: &HostName) {
        if let Some(v) = self.views.iter_mut().find(|v| &v.info.name == host) {
            v.active_agents += 1;
        }
    }
}

/// The first prompt of a session after a takeover or a planned move.
pub fn continue_prompt(
    agent: &AgentId,
    host: &HostName,
    from: Option<Harness>,
    to: Harness,
) -> String {
    let mut p = format!(
        "You continue Borant agent {agent} on host {host}. Read your history with the session_history tool, then list your tasks with task_list, then continue your work."
    );
    if let Some(f) = from
        && f != to
    {
        p.push_str(&format!(
            " Your previous session ran in {f}. Tool names can differ."
        ));
    }
    p
}

/// The refused hosts of a placement, as the event and `borant ps --queued`
/// read them: one line per host, with the first failed rule.
fn rejected_hosts(rejected: &[(HostName, Reject)]) -> Vec<RejectedHost> {
    rejected
        .iter()
        .map(|(host, why)| RejectedHost {
            host: host.clone(),
            why: why.to_string(),
        })
        .collect()
}

impl Hub {
    /// True for an agent whose role a person starts, such as a chief: a role
    /// that config pins. The hub never launches, relaunches, or moves it. Its
    /// lease goes to the session that the person starts, and a second session
    /// that the hub launched would take the lease from that one.
    pub(crate) fn person_started(&self, agent: &Agent) -> bool {
        agent
            .role
            .as_ref()
            .is_some_and(|role| self.cfg.roles.get(role).is_some_and(|c| c.pinned))
    }

    /// Stores the refused hosts of a placement in the spawn record of the
    /// agent, for `borant ps --queued`. The hub writes it when the refusal
    /// changes, never on every try. An agent without a spawn record is not
    /// stored. A record that another request changed is read again; when the
    /// write still conflicts, it returns false and the next try writes it.
    async fn note_reject(
        &self,
        agent: &AgentId,
        rejected: &[(HostName, Reject)],
    ) -> Result<bool, ApiError> {
        let key = keys::spawn(agent);
        let last_reject: Vec<(HostName, String)> = rejected
            .iter()
            .map(|(host, why)| (host.clone(), why.to_string()))
            .collect();
        // A request that changed the record between the read and the write
        // makes the write conflict: read again, so the stored reason does not
        // stay the first one for good.
        for _ in 0..3 {
            let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
                return Ok(true);
            };
            if doc.value.last_reject == last_reject {
                return Ok(true);
            }
            let mut record = doc.value;
            record.last_reject = last_reject.clone();
            match self.store.put(&key, &record, Some(doc.revision)).await {
                Ok(_) => return Ok(true),
                Err(e) if e.code == ApiErrorCode::Conflict => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    /// Records a refused placement: publishes `placement.refused` when the
    /// refusal changed or a minute passed since the last one, writes the
    /// spawn record only when the refusal changed, and sets the backoff of a
    /// spawn whose hosts are all full or offline. The lease and host lists of
    /// the tick are not read again here.
    async fn note_placement_refusal(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        reason: MoveReason,
        rejected: &[(HostName, Reject)],
        now: Timestamp,
    ) -> Result<(), ApiError> {
        let previous = mem.placement_refusals.get(agent);
        let changed = previous.is_none_or(|p| !same_refusal(&p.rejected, rejected));
        let told = !changed && previous.is_some_and(|p| now.since(p.told_at) < REFUSAL_REPEAT);
        if !told {
            let event = self
                .event(
                    now,
                    Severity::Warn,
                    SwarmEventBody::PlacementRefused {
                        reason,
                        rejected: rejected_hosts(rejected),
                    },
                )
                .about_agent(agent.clone());
            Self::queue(mem, event);
        }
        // A reason that did not reach the record stays "changed" for the next
        // try, so `ps` never keeps a refusal that the hub no longer holds.
        let stored = if changed {
            self.note_reject(agent, rejected).await?
        } else {
            true
        };
        let entry = mem
            .placement_refusals
            .entry(agent.clone())
            .or_insert_with(|| PlacementRefusal {
                rejected: if stored {
                    rejected.to_vec()
                } else {
                    Vec::new()
                },
                told_at: now,
                retry_at: now,
                tries: 0,
            });
        if stored {
            entry.rejected = rejected.to_vec();
        }
        if !told {
            entry.told_at = now;
        }
        if capacity_refusal(rejected) {
            entry.tries = entry.tries.saturating_add(1);
            let backoff = REFUSAL_BACKOFF.saturating_mul(1u32 << (entry.tries - 1).min(6));
            entry.retry_at = now.plus(backoff.min(REFUSAL_BACKOFF_MAX));
        } else {
            entry.tries = 0;
            entry.retry_at = now;
        }
        Ok(())
    }

    /// The hosts and their agents for one placement pass, read once, so a
    /// pass over many spawns lists the leases and the hosts once.
    pub(crate) async fn placement_pass(&self, mem: &Memory) -> Result<PlacementPass, ApiError> {
        Ok(PlacementPass {
            views: self.views(mem, None).await?,
        })
    }

    /// What a placement asks of a host: the tags of the role of the agent,
    /// the host of its parent when the role runs on the parent's host, and the
    /// bundles of which the agent already holds a member. With no role the
    /// request asks for those members only, but for the host that the hub
    /// names.
    pub(crate) async fn placement_req(
        &self,
        agent: &Agent,
        host: Option<&HostName>,
    ) -> PlacementReq {
        let mut req = PlacementReq {
            host: host.cloned(),
            // A member is the account of a pool: a host that cannot give the
            // launch that bundle runs the agent on its own credentials, which
            // is how a failed pool launch fell back to a box login.
            members: agent.members.keys().cloned().collect(),
            ..Default::default()
        };
        let Some(role) = agent.role.as_ref().and_then(|r| self.cfg.roles.get(r)) else {
            return req;
        };
        req.require = role.require.clone();
        req.prefer = role.prefer.clone();
        req.avoid = role.avoid.clone();
        // A role that asks for a sandbox is placed only on a host that offers
        // that kind.
        req.sandbox = role.sandbox;
        if role.on_parent_host
            && let Some(parent) = &agent.parent
            && let Ok(p) = self.agent(parent).await
        {
            req.host = p.value.host.clone();
        }
        req
    }

    /// The bundles of the next launch of the agent: the collection, the role,
    /// the channel, and the spawn, in that order, without duplicates. The host
    /// adds the harness defaults. A move or a takeover recomputes it from
    /// `hub.toml`, so a changed channel bundle applies at the next launch.
    pub(crate) fn launch_bundles(&self, agent: &Agent) -> Vec<BundleName> {
        let collection = agent
            .collection
            .as_ref()
            .and_then(|c| self.cfg.collections.get(c));
        let role = agent.role.as_ref().and_then(|r| self.cfg.roles.get(r));
        let channel: Vec<BundleName> = agent
            .channels
            .iter()
            .filter_map(|c| self.cfg.channels.get(c))
            .flat_map(|c| c.bundles.iter().cloned())
            .collect();
        roles::launch_bundles(collection, role, &channel, &agent.bundles)
    }

    pub(crate) async fn host_views(
        &self,
        mem: &Memory,
        agent: &AgentId,
    ) -> Result<Vec<HostView>, ApiError> {
        self.views(mem, Some(agent)).await
    }

    /// The view of every registered host, with the agents on each. `agent`
    /// marks the hosts where a launch of that agent already failed, so a
    /// placement says `RecentlyFailed` instead of blaming the clone; the
    /// elastic rules pass `None`.
    pub(crate) async fn views(
        &self,
        mem: &Memory,
        agent: Option<&AgentId>,
    ) -> Result<Vec<HostView>, ApiError> {
        let now = self.now();
        let mut active: HashMap<HostName, u32> = HashMap::new();
        for l in self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await? {
            match l.value.state {
                LeaseState::Held { host, .. } | LeaseState::Reserved { host, .. } => {
                    *active.entry(host).or_default() += 1;
                }
                _ => {}
            }
        }
        Ok(self
            .store
            .list::<HostInfo>(keys::HOST_PREFIX)
            .await?
            .into_iter()
            .map(|d| {
                let name = d.value.name.clone();
                HostView {
                    online: mem
                        .hosts_seen
                        .get(&name)
                        .is_some_and(|t| now.since(*t) <= self.cfg.lease.lease),
                    active_agents: active.get(&name).copied().unwrap_or(0),
                    // A failed launch is a recent failure of the host, not a
                    // repository that the hub cannot reach: `RecentlyFailed`
                    // says the first, and `NoRepo` would blame the clone. An
                    // old failure no longer blocks the host.
                    has_repo_access: true,
                    recently_failed: agent.is_some_and(|a| {
                        crate::liveness::failed_recently(mem, a, &name, now, &self.cfg.lease)
                    }),
                    mem_avail_mb: mem.host_stats.get(&name).and_then(|s| s.mem_avail_mb),
                    disk_used_pct: mem.host_stats.get(&name).and_then(|s| s.disk_used_pct),
                    load_per_cpu: mem
                        .host_stats
                        .get(&name)
                        .filter(|s| s.cpus > 0)
                        .map(|s| s.load1 / f64::from(s.cpus)),
                    launches_failing: crate::liveness::host_failing(
                        mem,
                        agent,
                        &name,
                        now,
                        &self.cfg.lease,
                    ),
                    pool: d.value.pool.clone(),
                    info: d.value,
                }
            })
            .collect())
    }

    /// The host of the reservation of an agent, after a placement.
    async fn reserved_host(&self, agent: &AgentId) -> Result<Option<HostName>, ApiError> {
        Ok(self
            .store
            .get::<Lease>(&keys::lease_agent(agent))
            .await?
            .and_then(|d| match d.value.state {
                LeaseState::Reserved { host, .. } => Some(host),
                _ => None,
            }))
    }

    /// Picks a host, reserves the lease for it, and sends a launch order.
    /// Returns false if no host qualifies now.
    pub(crate) async fn place_agent(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        reason: MoveReason,
        target: MoveTarget,
        prompt: Option<String>,
    ) -> Result<bool, ApiError> {
        Ok(self
            .place_agent_in(mem, agent, reason, target, prompt, None)
            .await?
            .is_some())
    }

    /// [`Hub::place_agent`], with the hosts of `pass` when a pass lists them
    /// once for many spawns. Returns the host that the placement reserved.
    pub(crate) async fn place_agent_in(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        reason: MoveReason,
        target: MoveTarget,
        prompt: Option<String>,
        pass: Option<&PlacementPass>,
    ) -> Result<Option<HostName>, ApiError> {
        let now = self.now();
        // A retired agent is gone for good: no spawn, move, takeover, or wake
        // places it again.
        if agent.retired || agent.paused.is_some() {
            tracing::debug!(agent = %agent.id, "placement skipped: the agent is retired");
            return Ok(None);
        }
        if self.person_started(agent) {
            tracing::debug!(agent = %agent.id, "placement skipped: a person starts this role");
            return Ok(None);
        }
        // The sweep gave up on this claim after its recovery limit: it waits
        // for a person, and no lease tick places the agent again.
        if mem.recovery_stopped.contains(&agent.id) {
            tracing::debug!(agent = %agent.id, "placement skipped: the recovery limit");
            return Ok(None);
        }
        // A spawn that failed its launches, or that a person cancelled, waits
        // for a person: the hub places it only after a new spawn record.
        if let Some(rec) = self
            .store
            .get::<SpawnRecord>(&keys::spawn(&agent.id))
            .await?
            && matches!(rec.value.state, SpawnState::Failed | SpawnState::Cancelled)
        {
            tracing::debug!(
                agent = %agent.id,
                state = ?rec.value.state,
                "placement skipped: the spawn is closed"
            );
            return Ok(None);
        }
        // A pinned agent is never replaced while its session may still run.
        // Only a clean end (a released lease) or a dead session on its own
        // host lets the hub place it again.
        if agent.pinned {
            let key = keys::lease_agent(&agent.id);
            let lease = self.store.get::<Lease>(&key).await?;
            let released = lease
                .as_ref()
                .is_some_and(|d| matches!(d.value.state, LeaseState::Released { .. }));
            let recovery = reason == MoveReason::Takeover
                && target.host == agent.host
                && lease.as_ref().is_some_and(|d| {
                    crate::leases::pinned_session_dead(mem, agent, &d.value, now, &self.cfg.lease)
                });
            if !released && !recovery {
                tracing::info!(agent = %agent.id, "placement skipped: the agent is pinned");
                return Ok(None);
            }
        }
        // A pass lists the hosts and leases once for many spawns; otherwise
        // this placement lists them for itself.
        let views: Cow<'_, [HostView]> = match pass {
            Some(pass) => pass.for_agent(self, mem, &agent.id),
            None => Cow::Owned(self.host_views(mem, &agent.id).await?),
        };
        let (host, harness) = match schedule::place(
            agent.preferred_harness,
            target.harness,
            &self.placement_req(agent, target.host.as_ref()).await,
            &self.launch_bundles(agent),
            &views,
        ) {
            Ok(chosen) => chosen,
            Err(rejected) => {
                tracing::debug!(agent = %agent.id, "no host qualifies for the agent now");
                self.note_placement_refusal(mem, &agent.id, reason, &rejected, now)
                    .await?;
                return Ok(None);
            }
        };

        let key = keys::lease_agent(&agent.id);
        let doc = self.store.get::<Lease>(&key).await?;
        if let Some(d) = &doc
            && matches!(d.value.state, LeaseState::Held { .. })
        {
            // A live session holds the agent. Never take its lease.
            tracing::warn!(agent = %agent.id, "placement skipped: a live session holds the lease");
            return Ok(None);
        }
        let Some(info) = views
            .iter()
            .find(|v| v.info.name == host)
            .map(|v| v.info.clone())
        else {
            return Ok(None);
        };
        // The member of each bundle with several members, for this host and
        // harness. A bundle whose members are all limited keeps the agent
        // pending: the tick places it again until a member resets.
        let members = match self.assign_members(mem, agent, &info, harness).await? {
            Assign::Members(members) => members,
            Assign::Wait => return Ok(None),
        };
        if agent.members != members {
            let mut updated = agent.clone();
            updated.members = members.clone();
            self.store.upsert(&keys::agent(&agent.id), &updated).await?;
        }
        let base = doc.as_ref().map(|d| d.value.clone()).unwrap_or(Lease {
            agent: agent.id.clone(),
            token: FencingToken(0),
            state: LeaseState::Released {
                since: now,
                parked: false,
            },
        });
        let reserved = lease::reserve(&base, &host, reason, now, &self.cfg.lease);
        // The decision, then the reservation, then the order.
        let chosen = self
            .event(
                now,
                Severity::Info,
                SwarmEventBody::PlacementChosen { harness, reason },
            )
            .about_agent(agent.id.clone())
            .about_host(host.clone());
        Self::queue(mem, chosen);
        self.store
            .put(&key, &reserved, doc.as_ref().map(|d| d.revision))
            .await?;
        // A host takes the agent now: the queue may report the next refusal
        // at once, not a minute after this one.
        mem.placement_refusals.remove(&agent.id);
        let until = match reserved.state {
            LeaseState::Reserved { until, .. } => until,
            _ => now.plus(self.cfg.lease.reserve),
        };
        let reservation = self.lease_event(
            now,
            Severity::Info,
            &agent.id,
            Some(&host),
            SwarmEventBody::LeaseReserved { reason, until },
        );
        Self::queue(mem, reservation);

        let events = self.sessions.read(&agent.id).await.unwrap_or_default();
        // The newest checkpoint gives both the wip commit and the branch
        // that was checked out when it was taken, so a restore returns to
        // that branch instead of the branch of the original spawn.
        let checkpoint = events.iter().rev().find_map(|(_, e)| match &e.body {
            SessionEventBody::Checkpoint { sha, branch, .. } => Some((sha.clone(), branch.clone())),
            _ => None,
        });
        let (wip, wip_branch) = match checkpoint {
            Some((sha, branch)) => (Some(sha), branch),
            None => (None, None),
        };
        // The harness of the newest session comes from its own record: a
        // host logs an event of a session that it no longer knows, such as a
        // checkpoint after the end, with a placeholder harness.
        let last_session = events
            .iter()
            .rev()
            .find_map(|(_, e)| e.session.map(|id| (id, e.harness)));
        let from_harness = match last_session {
            Some((id, logged)) => {
                Some(match self.store.get::<Session>(&keys::session(&id)).await {
                    Ok(Some(doc)) => doc.value.harness,
                    _ => logged,
                })
            }
            None => None,
        };
        let prompt = match (reason, prompt) {
            (MoveReason::Spawn, Some(p)) => p,
            (_, Some(p)) => p,
            (_, None) => continue_prompt(&agent.id, &host, from_harness, harness),
        };
        let project = match &agent.project {
            Some(p) => self
                .store
                .get::<Project>(&keys::project(p))
                .await?
                .map(|d| d.value),
            None => None,
        };
        // A model name belongs to one harness. Another harness gets none.
        let model = match (&agent.model, agent.model_harness) {
            (Some(m), Some(h)) if h == harness => Some(m.clone()),
            (Some(m), _) => {
                tracing::warn!(agent = %agent.id, model = %m, %harness, "the model is for another harness; launching without it");
                None
            }
            (None, _) => None,
        };
        // The collection decides the git author, and `launch_bundles` merges
        // the bundle levels (collection, role, channel, spawn) for the launch.
        let collection = agent
            .collection
            .as_ref()
            .and_then(|c| self.cfg.collections.get(c));
        let role = agent.role.as_ref().and_then(|r| self.cfg.roles.get(r));
        let order = LaunchOrder {
            agent: agent.id.clone(),
            harness,
            repo: agent.repo.clone(),
            wip,
            wip_branch,
            channels: agent.channels.clone(),
            prompt,
            reason,
            from_harness,
            from_host: agent.host.clone(),
            model,
            project,
            reserve_secs: Some(self.cfg.lease.reserve.as_secs()),
            members,
            bundles: self.launch_bundles(agent),
            git_author: roles::git_author(collection),
            // The role says whether the host wraps this launch in the
            // namespace sandbox.
            sandbox: role.and_then(|r| r.sandbox),
        };
        let move_event = SessionEvent {
            id: EventId::new(),
            agent: agent.id.clone(),
            session: None,
            harness,
            host: host.clone(),
            turn: 0,
            at: now,
            body: SessionEventBody::Move {
                from_host: agent.host.clone(),
                to_host: host.clone(),
                to_harness: harness,
                reason,
            },
        };
        if let Err(e) = self.sessions.append(&agent.id, &move_event).await {
            tracing::warn!(agent = %agent.id, "could not log the move: {e}");
        }
        let payload = serde_json::to_vec(&HostCommand::Launch(order))
            .map_err(|e| ApiError::internal(e.to_string()))?;
        self.bus
            .send(&subjects::host_cmd(&host), payload.into())
            .await?;
        let launched = self
            .event(
                now,
                Severity::Info,
                SwarmEventBody::MoveLaunched {
                    from_host: agent.host.clone(),
                    to_host: host.clone(),
                    reason,
                },
            )
            .about_agent(agent.id.clone());
        Self::queue(mem, launched);
        // A pending move or spawn stays until the agent registers, so a failed
        // launch is placed again with the same target and prompt.
        tracing::info!(agent = %agent.id, %host, %harness, ?reason, "launch order sent");
        Ok(Some(host))
    }

    async fn selected(&self, selector: &MoveSelector) -> Result<Vec<AgentId>, ApiError> {
        Ok(match selector {
            MoveSelector::Agent(a) => {
                self.agent(a).await?;
                vec![a.clone()]
            }
            MoveSelector::FromHost(h) => self
                .store
                .list::<Lease>(keys::LEASE_AGENT_PREFIX)
                .await?
                .into_iter()
                .filter(|l| matches!(&l.value.state, LeaseState::Held { host, .. } if host == h))
                .map(|l| l.value.agent)
                .collect(),
            MoveSelector::Channel(c) => {
                self.channel(c).await?;
                self.store
                    .list::<borant_core::model::Member>(&keys::members_prefix(c))
                    .await?
                    .into_iter()
                    .map(|m| m.value.agent)
                    .collect()
            }
        })
    }

    /// Validates a manual member switch (`borant move <agent> --member m`) and
    /// records the choice on the agent, so the relaunch of the planned move
    /// carries it in `LaunchOrder.members`.
    ///
    /// The member must be one of a bundle that the agent already uses, on the
    /// target host (the current host when the move names none), for the
    /// harness of the launch. A member that is limited now is a Conflict: the
    /// person can switch again when it resets.
    async fn switch_member(
        &self,
        agent: &AgentId,
        member: &MemberName,
        target: &mut MoveTarget,
    ) -> Result<(), ApiError> {
        let record = self.agent(agent).await?.value;
        if record.members.is_empty() {
            return Err(ApiError::invalid(format!(
                "agent {agent} has no member bundle to switch"
            )));
        }
        let host = target
            .host
            .clone()
            .or_else(|| record.host.clone())
            .ok_or_else(|| ApiError::invalid(format!("agent {agent} is not on a host")))?;
        let harness = target.harness.unwrap_or(record.preferred_harness);
        let info = self
            .store
            .get::<HostInfo>(&keys::host(&host))
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no host {host}")))?
            .value;
        let bundle = record
            .members
            .keys()
            .find(|bundle| {
                info.members.get(*bundle).is_some_and(|known| {
                    known.members.contains(member) && known.harnesses.contains(&harness)
                })
            })
            .cloned()
            .ok_or_else(|| {
                let known = record
                    .members
                    .keys()
                    .next()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "a bundle".into());
                ApiError::invalid(format!("no member {member} in {known} on {host}"))
            })?;
        let state = self
            .store
            .get::<MemberState>(&crate::members::key(&bundle, member))
            .await?;
        if let Some(until) = state
            .as_ref()
            .and_then(|doc| doc.value.limited_until)
            .filter(|until| *until > self.now())
        {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("member {member} is limited until {until}"),
            ));
        }
        if record.members.get(&bundle) != Some(member) {
            let mut updated = record;
            updated.members.insert(bundle, member.clone());
            self.store.upsert(&keys::agent(agent), &updated).await?;
        }
        // The switch relaunches on this host unless the move named another.
        target.host = Some(host);
        Ok(())
    }

    /// True when an elastic pool is configured, so a refused placement can
    /// wait for a host that the pool orders.
    fn has_pools(&self) -> bool {
        self.cfg
            .elastic
            .as_ref()
            .is_some_and(|e| !e.pools.is_empty())
    }

    pub(crate) async fn move_agents(
        &self,
        mem: &mut Memory,
        selector: &MoveSelector,
        target: MoveTarget,
        member: Option<MemberName>,
    ) -> Result<Vec<AgentId>, ApiError> {
        if member.is_some() && !matches!(selector, MoveSelector::Agent(_)) {
            return Err(ApiError::invalid("a member switch names one agent"));
        }
        let mut moved = Vec::new();
        // Slots that earlier agents of this move take on their target.
        let mut planned: HashMap<HostName, u32> = HashMap::new();
        for id in self.selected(selector).await? {
            // A member switch first validates the member and records it on the
            // agent, so the relaunch carries it in `LaunchOrder.members`.
            let mut target = target.clone();
            if let Some(member) = &member {
                self.switch_member(&id, member, &mut target).await?;
            }
            // A retired or paused agent is not placed, so it does not move.
            let stopped = {
                let agent = self.agent(&id).await?.value;
                agent.retired || agent.paused.is_some()
            };
            if stopped {
                tracing::info!(agent = %id, "move skipped: the agent is retired or paused");
                continue;
            }
            // An explicit move is a fresh placement: the hosts that failed a
            // launch for this agent get another try.
            mem.failed_hosts.remove(&id);
            mem.recovery_stopped.remove(&id);
            self.reset_recoveries(&id).await?;
            let lease = self.store.get::<Lease>(&keys::lease_agent(&id)).await?;
            // A move would start a second session. A pinned agent moves only
            // after a clean end, like a placement.
            if self.agent(&id).await?.value.pinned
                && !lease
                    .as_ref()
                    .is_some_and(|l| matches!(l.value.state, LeaseState::Released { .. }))
            {
                tracing::info!(agent = %id, "move skipped: the agent is pinned");
                continue;
            }
            match lease.map(|l| l.value.state) {
                Some(LeaseState::Held { session, host, .. }) => {
                    if member.is_none()
                        && target.host.as_ref() == Some(&host)
                        && target.harness.is_none()
                    {
                        continue;
                    }
                    // Check the target before the session stops. A session
                    // stopped for a full target stays down until a slot opens.
                    let agent = self.agent(&id).await?.value;
                    let mut views = self.host_views(mem, &id).await?;
                    for v in &mut views {
                        if v.info.name == host {
                            v.active_agents = v.active_agents.saturating_sub(1);
                        }
                        v.active_agents += planned.get(&v.info.name).copied().unwrap_or(0);
                    }
                    let req = self.placement_req(&agent, target.host.as_ref()).await;
                    let bundles = self.launch_bundles(&agent);
                    // A target that no host can take now is demand when the
                    // pool can order a host: the session stops, the move
                    // stays pending, and the host that comes up takes it.
                    let to = match schedule::place(
                        agent.preferred_harness,
                        target.harness,
                        &req,
                        &bundles,
                        &views,
                    ) {
                        Ok((to, _)) => Some(to),
                        Err(rejected)
                            if self.has_pools()
                                && target.host.is_none()
                                && move_waits_for_a_host(&rejected) =>
                        {
                            None
                        }
                        Err(_) => {
                            return Err(ApiError::unavailable(format!(
                                "no host can take agent {id} now"
                            )));
                        }
                    };
                    if let Some(to) = &to {
                        *planned.entry(to.clone()).or_default() += 1;
                    }
                    let cmd = HostCommand::PrepareMove {
                        agent: id.clone(),
                        session,
                    };
                    let payload =
                        serde_json::to_vec(&cmd).map_err(|e| ApiError::internal(e.to_string()))?;
                    self.bus
                        .send(&subjects::host_cmd(&host), payload.into())
                        .await?;
                    mem.pending_moves.insert(
                        id.clone(),
                        PendingMove {
                            target: target.clone(),
                            reason: MoveReason::Planned,
                            prompt: None,
                        },
                    );
                    if let Some(to) = to {
                        let now = self.now();
                        let requested = self
                            .event(
                                now,
                                Severity::Info,
                                SwarmEventBody::MoveRequested {
                                    from_host: Some(host.clone()),
                                    to_host: to,
                                    reason: MoveReason::Planned,
                                },
                            )
                            .about_agent(id.clone());
                        Self::queue(mem, requested);
                    }
                }
                Some(LeaseState::Reserved { .. }) => continue,
                _ => {
                    let agent = self.agent(&id).await?.value;
                    if !self
                        .place_agent(mem, &agent, MoveReason::Planned, target.clone(), None)
                        .await?
                    {
                        if !self.has_pools() || target.host.is_some() {
                            return Err(ApiError::unavailable(format!(
                                "no host can take agent {id} now"
                            )));
                        }
                        // The pool counts the pending move as demand and the
                        // tick places the agent when the host is up.
                        mem.pending_moves.insert(
                            id.clone(),
                            PendingMove {
                                target: target.clone(),
                                reason: MoveReason::Planned,
                                prompt: None,
                            },
                        );
                    }
                }
            }
            moved.push(id);
        }
        Ok(moved)
    }

    pub(crate) async fn drain(
        &self,
        mem: &mut Memory,
        host: &HostName,
    ) -> Result<Vec<AgentId>, ApiError> {
        let key = keys::host(host);
        let mut doc = self
            .store
            .get::<HostInfo>(&key)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no host {host}")))?;
        doc.value.draining = true;
        self.store.put(&key, &doc.value, Some(doc.revision)).await?;
        let moved = self
            .move_agents(
                mem,
                &MoveSelector::FromHost(host.clone()),
                MoveTarget::default(),
                None,
            )
            .await?;
        let event = self
            .event(
                self.now(),
                Severity::Warn,
                SwarmEventBody::HostDraining {
                    agents: moved.len() as u64,
                },
            )
            .about_host(host.clone());
        Self::queue(mem, event);
        Ok(moved)
    }

    /// Clears `draining`, so placement offers the host again. The caller
    /// places the queue afterward: this does not take `mem`. Returns whether
    /// the flag changed, because an undrain of a live host is a no-op.
    pub(crate) async fn undrain(&self, host: &HostName) -> Result<bool, ApiError> {
        let key = keys::host(host);
        let mut doc = self
            .store
            .get::<HostInfo>(&key)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no host {host}")))?;
        if !doc.value.draining {
            return Ok(false);
        }
        doc.value.draining = false;
        self.store.put(&key, &doc.value, Some(doc.revision)).await?;
        Ok(true)
    }

    /// Deletes the record of a host that is offline and runs no agent, so
    /// that `borant hosts`, the view, `/metrics`, and placement stop naming a
    /// box that is gone. Only a human may ask for it. `mem` loses the seen
    /// time and the stats of the host.
    pub(crate) async fn remove_host(
        &self,
        mem: &mut Memory,
        host: &HostName,
    ) -> Result<HostName, ApiError> {
        let key = keys::host(host);
        let doc = self
            .store
            .get::<HostInfo>(&key)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no host {host}")))?;
        let now = self.now();
        let seen = mem.hosts_seen.get(host).copied();
        if seen.is_some_and(|at| now.since(at) <= self.cfg.lease.lease) {
            return Err(ApiError::invalid(format!(
                "host {host} is online; drain it and stop its daemon first"
            )));
        }
        let held: Vec<AgentId> = self
            .store
            .list::<Lease>(keys::LEASE_AGENT_PREFIX)
            .await?
            .into_iter()
            .filter(|d| {
                matches!(
                    &d.value.state,
                    LeaseState::Held { host: h, .. } | LeaseState::Reserved { host: h, .. }
                        if h == host
                )
            })
            .map(|d| d.value.agent)
            .collect();
        if !held.is_empty() {
            let agents: Vec<String> = held.iter().map(ToString::to_string).collect();
            return Err(ApiError::invalid(format!(
                "host {host} still holds agents {}; move them first",
                agents.join(", ")
            )));
        }
        self.store.delete(&key).await?;
        mem.hosts_seen.remove(host);
        mem.host_stats.remove(host);
        tracing::info!(host = %host, "host record removed");
        Ok(doc.value.name)
    }

    pub(crate) async fn move_ready(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        session: SessionId,
        token: FencingToken,
    ) -> Result<(), ApiError> {
        let key = keys::lease_agent(agent);
        let doc = self
            .store
            .get::<Lease>(&key)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no lease for {agent}")))?;
        let held = matches!(&doc.value.state, LeaseState::Held { session: s, .. } if *s == session);
        if !held || doc.value.token != token {
            return Err(borant_core::CoreError::StaleToken.into());
        }
        let from_host = match &doc.value.state {
            LeaseState::Held { host, .. } => Some(host.clone()),
            _ => None,
        };
        self.store
            .put(
                &key,
                &lease::release(&doc.value, self.now()),
                Some(doc.revision),
            )
            .await?;
        let mut a = self.agent(agent).await?;
        a.value.session = None;
        self.store.put(&a.key, &a.value, Some(a.revision)).await?;
        let pending = mem
            .pending_moves
            .get(agent)
            .cloned()
            .unwrap_or(PendingMove {
                target: MoveTarget::default(),
                reason: MoveReason::Planned,
                prompt: None,
            });
        let placed = self
            .place_agent(
                mem,
                &a.value,
                pending.reason,
                pending.target.clone(),
                pending.prompt.clone(),
            )
            .await?;
        if placed {
            // The old session ended and the move may run; the destination is
            // the host that the placement just reserved.
            if let Some(to) = self.reserved_host(agent).await? {
                let event = self
                    .event(
                        self.now(),
                        Severity::Info,
                        SwarmEventBody::MoveReady {
                            from_host,
                            to_host: to,
                            reason: pending.reason,
                        },
                    )
                    .about_agent(agent.clone());
                Self::queue(mem, event);
            }
        } else {
            // Keep the target. The tick retries after the grace period.
            mem.pending_moves.insert(agent.clone(), pending);
            let doc = self.store.get::<Lease>(&key).await?;
            if let Some(d) = doc {
                let expired = Lease {
                    agent: agent.clone(),
                    token: d.value.token,
                    state: LeaseState::Expired {
                        since: self.now(),
                        last_session: None,
                    },
                };
                self.store.put(&key, &expired, Some(d.revision)).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn launch_failed(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        host: &HostName,
        error: &str,
        timeout_secs: Option<f64>,
    ) -> Result<(), ApiError> {
        // The end of the screen that the host captured, redacted again: a
        // sticky prompt can hold a value that the harness printed.
        let screen = redact::redact(error).into_owned();
        tracing::warn!(%agent, %host, screen = %screen, "launch failed");
        mem.failed_hosts
            .entry(agent.clone())
            .or_default()
            .insert(host.clone(), self.now());
        // A forced host that failed must not block the next placement.
        if let Some(p) = mem.pending_moves.get_mut(agent)
            && p.target.host.as_ref() == Some(host)
        {
            p.target.host = None;
        }
        let key = keys::lease_agent(agent);
        if let Some(d) = self.store.get::<Lease>(&key).await?
            && matches!(&d.value.state, LeaseState::Reserved { host: h, .. } if h == host)
        {
            let expired = Lease {
                agent: agent.clone(),
                token: d.value.token,
                state: LeaseState::Expired {
                    since: self.now().minus(self.cfg.lease.grace),
                    last_session: None,
                },
            };
            self.store.put(&key, &expired, Some(d.revision)).await?;
        }
        // The try counts against the spawn of the agent, and the record stops
        // the hub after three of them. The record keeps the last line of the
        // screen, so `borant ps` and the spawner read the cause.
        let (attempts, failed) = self
            .note_launch_failure(mem, agent, host, timeout_secs, Some(&screen))
            .await?;
        let severity = if failed {
            Severity::Error
        } else {
            Severity::Warn
        };
        let resume_line =
            launch_error_text(&screen).unwrap_or_else(|| "the launch failed".to_owned());
        let event = self.event(
            self.now(),
            severity,
            SwarmEventBody::LaunchStuck { screen, attempts },
        );
        Self::queue(mem, event);
        // A resume that fails tells the person or the agent who asked for it,
        // and never counts against the spawn.
        if let Some(by) = mem.resumed_by.remove(agent) {
            let text = format!("the resume of agent {agent} failed on {host}: {resume_line}");
            match by {
                Sender::Agent(who) => {
                    if let Err(e) = self.tell(&who, &text).await {
                        tracing::warn!(%agent, %who, "the resume message failed: {e}");
                    }
                }
                Sender::Human | Sender::Approval(_) => {
                    if let Ok(doc) = self.agent(agent).await
                        && let Some(owner) = self.owner_of(&doc.value).await?
                        && let Err(e) = self.tell(&owner, &text).await
                    {
                        tracing::warn!(%agent, %owner, "the resume message failed: {e}");
                    }
                }
            }
        }
        Ok(())
    }

    /// Counts one failed try of the spawn of `agent`, and fails the record
    /// after three: a launch that keeps landing on the same screen waits for
    /// a person, not for another try. The record keeps the host, the timeout,
    /// and the capped error of each try, so `borant ps` and the spawner read
    /// why it failed. Returns the count and whether the record is `Failed`
    /// now. An agent without a spawn record is not counted, and neither is a
    /// launch of a spawn that is no longer `Queued` or `Launching`: a failed
    /// resume of a running agent is not a spawn try.
    ///
    /// The record that fails also retires its agent, by the path of a normal
    /// retire, so no takeover places it again; the record stays for `ps`.
    ///
    /// True when `host` has no record and no pool host of that name is on its
    /// way: a spawn that names it waits for nothing. A host that registered
    /// once and is offline now still has its record, and the spawn waits for
    /// it to come back.
    pub(crate) async fn host_is_gone(&self, host: &HostName) -> Result<bool, ApiError> {
        if self
            .store
            .get::<HostInfo>(&keys::host(host))
            .await?
            .is_some()
        {
            return Ok(false);
        }
        let booting = self
            .store
            .get::<borant_core::rules::pool::ElasticHost>(&keys::elastic(host))
            .await?
            .is_some_and(|d| {
                matches!(
                    d.value.phase,
                    borant_core::rules::pool::Phase::Requested
                        | borant_core::rules::pool::Phase::Booting
                )
            });
        Ok(!booting)
    }

    /// Fails the spawn of `agent` at once because the host that it names is
    /// gone: a launch that no host can take is no reason to wait for the
    /// timeout of the queue. Goes through the path of the last failed try, so
    /// the spawner hears of it and the agent is retired.
    pub(crate) async fn fail_spawn_on_gone_host(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        host: &HostName,
    ) -> Result<(), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok(());
        };
        let mut record = doc.value;
        if !matches!(record.state, SpawnState::Queued | SpawnState::Launching) {
            return Ok(());
        }
        record.attempts = MAX_LAUNCH_TRIES - 1;
        self.store
            .put(&doc.key, &record, Some(doc.revision))
            .await?;
        let error = format!("host {host} is gone");
        self.note_launch_failure(mem, agent, host, None, Some(&error))
            .await?;
        Ok(())
    }

    /// The spawner gets one direct message, with the write that fails the
    /// record: a repeat on a `Failed` record counts nothing and sends
    /// nothing. A spawn by a person has no agent to tell.
    pub(crate) async fn note_launch_failure(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        host: &HostName,
        timeout_secs: Option<f64>,
        error: Option<&str>,
    ) -> Result<(u32, bool), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok((0, false));
        };
        let mut record = doc.value;
        // A failed spawn waits for a person: the record keeps the count and
        // the hosts of the tries that failed it.
        if record.state == SpawnState::Failed {
            return Ok((record.attempts, true));
        }
        // Only a queued or launching spawn counts its launches: a failed
        // resume of a running agent must not retire it.
        if !matches!(record.state, SpawnState::Queued | SpawnState::Launching) {
            return Ok((record.attempts, false));
        }
        record.attempts = record.attempts.saturating_add(1);
        record.launch_failures.push(LaunchFailure {
            host: host.clone(),
            timeout_secs,
            // Only the last line, short: the record and the queue reply carry
            // the cause, not the screen.
            error: error.and_then(launch_error_text),
        });
        let failed = record.attempts >= MAX_LAUNCH_TRIES;
        if failed {
            record.state = SpawnState::Failed;
        }
        self.store
            .put(&doc.key, &record, Some(doc.revision))
            .await?;
        if !failed {
            return Ok((record.attempts, false));
        }
        let reason = record.fail_reason().unwrap_or_default();
        tracing::warn!(
            %agent,
            attempts = record.attempts,
            reason,
            "a spawn failed its launches, giving up"
        );
        if let Sender::Agent(spawner) = &record.by {
            let text = format!("the spawn of agent {agent} failed: {reason}");
            self.tell(spawner, &text).await?;
        }
        // A spawn that failed its launches waits for a person: the agent is
        // retired by the normal path, and the record stays `Failed` for `ps`.
        if let Ok(doc) = self.agent(agent).await
            && !doc.value.retired
        {
            self.retire_failed_spawn(mem, &doc.value).await?;
        }
        Ok((record.attempts, true))
    }

    pub(crate) async fn list_agents(
        &self,
        mine: Option<&CollectionName>,
    ) -> Result<Value, ApiError> {
        let mut out = Vec::new();
        for a in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            if !crate::calls::allowed(mine, a.value.collection.as_ref()) {
                continue;
            }
            let lease = self
                .store
                .get::<Lease>(&keys::lease_agent(&a.value.id))
                .await?;
            out.push(json!({
                "agent": a.value.id,
                "harness": a.value.preferred_harness,
                "host": a.value.host,
                "channels": a.value.channels,
                "movable": a.value.movable,
                "lease": lease.map(|l| l.value),
                "role": a.value.role,
                "collection": a.value.collection,
                "pinned": a.value.pinned,
                "retired": a.value.retired,
                // A stopped agent keeps its pause across a hub restart, and
                // no takeover or placement touches it until a resume.
                "paused": a.value.paused,
                "model": a.value.model,
                // The member of each bundle that the launch picked, for
                // `borant members` and the e2e scenario.
                "members": a.value.members,
            }));
        }
        Ok(Value::Array(out))
    }

    pub(crate) async fn list_hosts(&self, mem: &Memory) -> Result<Value, ApiError> {
        let now = self.now();
        let mut out = Vec::new();
        for h in self.store.list::<HostInfo>(keys::HOST_PREFIX).await? {
            let seen = mem.hosts_seen.get(&h.value.name).copied();
            out.push(json!({
                "host": h.value,
                "online": seen.is_some_and(|t| now.since(t) <= self.cfg.lease.lease),
                "last_seen": seen,
            }));
        }
        Ok(Value::Array(out))
    }
}
