//! Placement, planned moves, drains, and spawns (spec sections 13 and 14).

use std::collections::HashMap;

use borant_core::events::{RejectedHost, Severity, SwarmEventBody};
use borant_core::model::{
    Agent, Harness, HostInfo, Lease, LeaseState, MemberState, MoveReason, Project, SessionEvent,
    SessionEventBody, SpawnRecord, SpawnState,
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
    keys, subjects,
};
use serde_json::{Value, json};

use crate::members::Assign;
use crate::{Hub, Memory, PendingMove};

/// The launches of one spawn that the hub counts before it gives up.
const MAX_LAUNCH_TRIES: u32 = 3;

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
    /// Stores the refused hosts of a placement in the spawn record of the
    /// agent, for `borant ps --queued`. An agent without a spawn record is not
    /// stored, and a record that another request changed is left to the next
    /// try.
    async fn note_reject(
        &self,
        agent: &AgentId,
        rejected: &[(HostName, Reject)],
    ) -> Result<(), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok(());
        };
        let mut record = doc.value;
        record.last_reject = rejected
            .iter()
            .map(|(host, why)| (host.clone(), why.to_string()))
            .collect();
        match self.store.put(&key, &record, Some(doc.revision)).await {
            Ok(_) => Ok(()),
            Err(e) if e.code == ApiErrorCode::Conflict => Ok(()),
            Err(e) => Err(e),
        }
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
        let failed = agent.and_then(|a| mem.failed_hosts.get(a));
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
                    // says the first, and `NoRepo` would blame the clone.
                    has_repo_access: true,
                    recently_failed: failed.is_some_and(|f| f.contains(&name)),
                    mem_avail_mb: mem.host_stats.get(&name).and_then(|s| s.mem_avail_mb),
                    disk_used_pct: mem.host_stats.get(&name).and_then(|s| s.disk_used_pct),
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
        let now = self.now();
        // A retired agent is gone for good: no spawn, move, takeover, or wake
        // places it again.
        if agent.retired || agent.paused.is_some() {
            tracing::debug!(agent = %agent.id, "placement skipped: the agent is retired");
            return Ok(false);
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
            return Ok(false);
        }
        // A pinned agent is never replaced while its session may still run.
        // Only a clean end (a released lease) lets the hub place it again.
        if agent.pinned {
            let key = keys::lease_agent(&agent.id);
            let released = self
                .store
                .get::<Lease>(&key)
                .await?
                .is_some_and(|d| matches!(d.value.state, LeaseState::Released { .. }));
            if !released {
                tracing::info!(agent = %agent.id, "placement skipped: the agent is pinned");
                return Ok(false);
            }
        }
        let views = self.host_views(mem, &agent.id).await?;
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
                let event = self
                    .event(
                        now,
                        Severity::Warn,
                        SwarmEventBody::PlacementRefused {
                            reason,
                            rejected: rejected_hosts(&rejected),
                        },
                    )
                    .about_agent(agent.id.clone());
                Self::queue(mem, event);
                self.note_reject(&agent.id, &rejected).await?;
                return Ok(false);
            }
        };

        let key = keys::lease_agent(&agent.id);
        let doc = self.store.get::<Lease>(&key).await?;
        if let Some(d) = &doc
            && matches!(d.value.state, LeaseState::Held { .. })
        {
            // A live session holds the agent. Never take its lease.
            tracing::warn!(agent = %agent.id, "placement skipped: a live session holds the lease");
            return Ok(false);
        }
        let Some(info) = views
            .iter()
            .find(|v| v.info.name == host)
            .map(|v| v.info.clone())
        else {
            return Ok(false);
        };
        // The member of each bundle with several members, for this host and
        // harness. A bundle whose members are all limited keeps the agent
        // pending: the tick places it again until a member resets.
        let members = match self.assign_members(mem, agent, &info, harness).await? {
            Assign::Members(members) => members,
            Assign::Wait => return Ok(false),
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
        let wip = events.iter().rev().find_map(|(_, e)| match &e.body {
            SessionEventBody::Checkpoint { sha, .. } => Some(sha.clone()),
            _ => None,
        });
        let from_harness = events
            .iter()
            .rev()
            .find(|(_, e)| e.session.is_some())
            .map(|(_, e)| e.harness);
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
        Ok(true)
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
                    let Some((to, _)) = schedule::place(
                        agent.preferred_harness,
                        target.harness,
                        &req,
                        &bundles,
                        &views,
                    )
                    .ok() else {
                        return Err(ApiError::unavailable(format!(
                            "no host can take agent {id} now"
                        )));
                    };
                    *planned.entry(to.clone()).or_default() += 1;
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
                Some(LeaseState::Reserved { .. }) => continue,
                _ => {
                    let agent = self.agent(&id).await?.value;
                    if !self
                        .place_agent(mem, &agent, MoveReason::Planned, target.clone(), None)
                        .await?
                    {
                        return Err(ApiError::unavailable(format!(
                            "no host can take agent {id} now"
                        )));
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
    ) -> Result<(), ApiError> {
        // The end of the screen that the host captured, redacted again: a
        // sticky prompt can hold a value that the harness printed.
        let screen = redact::redact(error).into_owned();
        tracing::warn!(%agent, %host, screen = %screen, "launch failed");
        mem.failed_hosts
            .entry(agent.clone())
            .or_default()
            .insert(host.clone());
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
        // the hub after three of them.
        let (attempts, failed) = self.note_launch_failure(agent).await?;
        let severity = if failed {
            Severity::Error
        } else {
            Severity::Warn
        };
        let event = self.event(
            self.now(),
            severity,
            SwarmEventBody::LaunchStuck { screen, attempts },
        );
        Self::queue(mem, event);
        Ok(())
    }

    /// Counts one failed try of the spawn of `agent`, and fails the record
    /// after three: a launch that keeps landing on the same screen waits for
    /// a person, not for another try. Returns the count and whether the
    /// record is `Failed` now. An agent without a spawn record is not
    /// counted.
    pub(crate) async fn note_launch_failure(
        &self,
        agent: &AgentId,
    ) -> Result<(u32, bool), ApiError> {
        let key = keys::spawn(agent);
        let Some(doc) = self.store.get::<SpawnRecord>(&key).await? else {
            return Ok((0, false));
        };
        let mut record = doc.value;
        record.attempts = record.attempts.saturating_add(1);
        let failed = record.attempts >= MAX_LAUNCH_TRIES;
        if failed && record.state != SpawnState::Failed {
            record.state = SpawnState::Failed;
            tracing::warn!(
                %agent,
                attempts = record.attempts,
                "a spawn failed three launches, giving up"
            );
        }
        self.store
            .put(&doc.key, &record, Some(doc.revision))
            .await?;
        Ok((record.attempts, failed))
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
