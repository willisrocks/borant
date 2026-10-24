//! Placement, planned moves, drains, and spawns (spec sections 13 and 14).

use std::collections::HashMap;

use borant_core::model::{
    Agent, Harness, HostInfo, Lease, LeaseState, MoveReason, SessionEvent, SessionEventBody,
};
use borant_core::proto::{ApiError, HostCommand, LaunchOrder, MoveSelector, MoveTarget};
use borant_core::rules::lease;
use borant_core::rules::schedule::{self, HostView};
use borant_core::{
    AgentId, ChannelName, EventId, FencingToken, HostName, SessionId, keys, subjects,
};
use serde_json::{Value, json};

use crate::{Hub, Memory, PendingMove};

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

impl Hub {
    async fn host_views(&self, mem: &Memory, agent: &AgentId) -> Result<Vec<HostView>, ApiError> {
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
        let failed = mem.failed_hosts.get(agent);
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
                    has_repo_access: failed.is_none_or(|f| !f.contains(&name)),
                    info: d.value,
                }
            })
            .collect())
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
        let views = self.host_views(mem, &agent.id).await?;
        let Some((host, harness)) = schedule::place(
            agent.preferred_harness,
            target.host.as_ref(),
            target.harness,
            &views,
        ) else {
            tracing::debug!(agent = %agent.id, "no host qualifies for the agent now");
            return Ok(false);
        };

        let key = keys::lease_agent(&agent.id);
        let doc = self.store.get::<Lease>(&key).await?;
        let base = doc.as_ref().map(|d| d.value.clone()).unwrap_or(Lease {
            agent: agent.id.clone(),
            token: FencingToken(0),
            state: LeaseState::Released { since: now },
        });
        let reserved = lease::reserve(&base, &host, reason, now, &self.cfg.lease);
        self.store
            .put(&key, &reserved, doc.as_ref().map(|d| d.revision))
            .await?;

        let events = self.sessions.read(&agent.id).await.unwrap_or_default();
        let wip = events.iter().rev().find_map(|(_, e)| match &e.body {
            SessionEventBody::Checkpoint { sha, .. } => Some(sha.clone()),
            _ => None,
        });
        let from_harness = events.iter().rev().map(|(_, e)| e.harness).next();
        let prompt = match (reason, prompt) {
            (MoveReason::Spawn, Some(p)) => p,
            (_, Some(p)) => p,
            (_, None) => continue_prompt(&agent.id, &host, from_harness, harness),
        };
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
                harness,
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
        mem.pending_moves.remove(&agent.id);
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

    pub(crate) async fn move_agents(
        &self,
        mem: &mut Memory,
        selector: &MoveSelector,
        target: MoveTarget,
    ) -> Result<Vec<AgentId>, ApiError> {
        let mut moved = Vec::new();
        for id in self.selected(selector).await? {
            let lease = self.store.get::<Lease>(&keys::lease_agent(&id)).await?;
            match lease.map(|l| l.value.state) {
                Some(LeaseState::Held { session, host, .. }) => {
                    if target.host.as_ref() == Some(&host) && target.harness.is_none() {
                        continue;
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
        self.move_agents(
            mem,
            &MoveSelector::FromHost(host.clone()),
            MoveTarget::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn(
        &self,
        mem: &mut Memory,
        by: &AgentId,
        channel: &ChannelName,
        prompt: String,
        harness: Option<Harness>,
        host: Option<HostName>,
        name: Option<AgentId>,
    ) -> Result<Value, ApiError> {
        self.channel(channel).await?;
        let parent = self.agent(by).await?.value;
        let harness = harness.unwrap_or(parent.preferred_harness);
        let id = match name {
            Some(n) => n,
            None => self.new_agent_id(harness).await?,
        };
        let agent = Agent {
            id: id.clone(),
            preferred_harness: harness,
            movable: parent.repo.is_some(),
            repo: parent.repo.clone(),
            channels: Vec::new(),
            session: None,
            host: None,
            harness_session_id: None,
            created: self.now(),
        };
        self.store
            .put(&keys::agent(&id), &agent, None)
            .await
            .map_err(|e| ApiError::new(e.code, format!("agent {id}: {}", e.message)))?;
        self.join(&id, channel).await?;
        let agent = self.agent(&id).await?.value;
        let placed = self
            .place_agent(
                mem,
                &agent,
                MoveReason::Spawn,
                MoveTarget {
                    host,
                    harness: Some(harness),
                },
                Some(prompt),
            )
            .await?;
        Ok(json!({ "agent": id, "placed": placed }))
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
        if !self
            .place_agent(
                mem,
                &a.value,
                MoveReason::Planned,
                pending.target.clone(),
                pending.prompt.clone(),
            )
            .await?
        {
            // Keep the target. The tick retries after the grace period.
            mem.pending_moves.insert(agent.clone(), pending);
            let doc = self.store.get::<Lease>(&key).await?;
            if let Some(d) = doc {
                let expired = Lease {
                    agent: agent.clone(),
                    token: d.value.token,
                    state: LeaseState::Expired { since: self.now() },
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
        tracing::warn!(%agent, %host, error, "launch failed");
        mem.failed_hosts
            .entry(agent.clone())
            .or_default()
            .insert(host.clone());
        let key = keys::lease_agent(agent);
        if let Some(d) = self.store.get::<Lease>(&key).await?
            && matches!(&d.value.state, LeaseState::Reserved { host: h, .. } if h == host)
        {
            let expired = Lease {
                agent: agent.clone(),
                token: d.value.token,
                state: LeaseState::Expired {
                    since: self.now().minus(self.cfg.lease.grace),
                },
            };
            self.store.put(&key, &expired, Some(d.revision)).await?;
        }
        Ok(())
    }

    pub(crate) async fn list_agents(&self) -> Result<Value, ApiError> {
        let mut out = Vec::new();
        for a in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
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
