//! The spawn queue (phase 3 spec, "The persisted queue" and "Idempotency").
//!
//! A spawn writes `spawn.<agent>` before the first placement try, so a hub
//! that restarts mid-queue still places the agent. Every tick places the
//! `Queued` records by priority and then by age, and a record that does not
//! fit does not block the ones after it. Planned moves keep their own path in
//! `Memory.pending_moves`.

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    Agent, ContinuePolicy, ContinueState, Harness, Lease, LeaseState, MoveReason, NewTask,
    RepoInfo, Sender, SpawnRecord, SpawnSpec, SpawnState, TaskBinding, TaskRef,
};
use borant_core::proto::{ApiError, ApiErrorCode, MoveTarget};
use borant_core::rules::lease;
use borant_core::rules::roles::{self, OnePer, SpawnInput};
use borant_core::rules::schedule;
use borant_core::rules::spawn::{agent_name_from_key, spawn_key_matches};
use borant_core::{AgentId, BundleName, ChannelName, FencingToken, HostName, RoleName, keys};
use serde_json::{Value, json};

use crate::{Hub, Memory};

/// The arguments of one spawn, from `ToolCall::Spawn` or the CLI.
#[derive(Debug, Clone)]
pub(crate) struct SpawnRequest {
    pub channel: ChannelName,
    pub prompt: String,
    pub harness: Option<Harness>,
    pub host: Option<HostName>,
    pub name: Option<AgentId>,
    pub model: Option<String>,
    pub role: Option<RoleName>,
    pub key: Option<String>,
    pub task: Option<TaskBinding>,
    pub stop_on_close: Option<bool>,
    /// The continues of a bound worker (phase 11 spec).
    pub continues: Option<ContinuePolicy>,
    pub priority: Option<i32>,
    /// The bundles of the spawn level. It is the last merge level, so a host
    /// must define every one of them at the launch.
    pub bundles: Vec<BundleName>,
}

/// One line of the refusal reasons, for `borant ps`; `None` when nothing
/// refused the record.
fn reject_text(reasons: &[(HostName, String)]) -> Option<String> {
    if reasons.is_empty() {
        return None;
    }
    Some(
        reasons
            .iter()
            .map(|(host, why)| format!("{host}: {why}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

impl Hub {
    /// Queues one spawn and tries to place it once. A second spawn with the
    /// same key and the same arguments returns the first agent.
    pub(crate) async fn spawn(
        &self,
        mem: &mut Memory,
        by: Sender,
        req: SpawnRequest,
    ) -> Result<Value, ApiError> {
        let ch = self.channel(&req.channel).await?.value;
        // A paused channel takes no new agent.
        if ch.paused {
            return Err(crate::pause::channel_paused(&req.channel));
        }
        let parent = match &by {
            Sender::Agent(id) => Some(self.agent(id).await?.value),
            Sender::Human => None,
        };
        let resolved = roles::resolve(
            &SpawnInput {
                role: req.role.clone(),
                harness: req.harness,
                model: req.model.clone(),
            },
            parent.as_ref(),
            &self.cfg.roles,
        )
        .map_err(|e| ApiError::invalid(e.to_string()))?;
        // A person's spawn takes the collection of its channel.
        let mut resolved = resolved;
        if resolved.collection.is_none() {
            resolved.collection = ch.collection.clone();
        }
        if let Some(parent) = &parent
            && !roles::may_spawn(
                parent.role.as_ref(),
                resolved.role.as_ref(),
                &self.cfg.roles,
            )
        {
            return Err(ApiError::denied(format!(
                "the role of {} may not spawn this agent",
                parent.id
            )));
        }
        // A child never gets more access than its parent: an agent names only
        // bundles that its own next launch would carry, a person any bundle.
        if let Some(parent) = &parent {
            let held = self.launch_bundles(parent);
            for bundle in &req.bundles {
                if !held.contains(bundle) {
                    return Err(ApiError::denied(format!(
                        "bundle {bundle} is not yours to give"
                    )));
                }
            }
        }
        let harness = match resolved
            .harness
            .or_else(|| parent.as_ref().map(|p| p.preferred_harness))
        {
            Some(harness) => harness,
            None => {
                return Err(ApiError::invalid(
                    "a spawn by a person needs a harness or a role with one",
                ));
            }
        };
        let model = resolved.model.clone();
        let prompt = roles::brief(&req.prompt, resolved.skill.as_deref());
        // A helper role runs on the host of its parent.
        let host = match req.host.clone() {
            Some(host) => Some(host),
            None if resolved.placement.on_parent_host => {
                parent.as_ref().and_then(|p| p.host.clone())
            }
            None => None,
        };
        let name = match req.name.clone() {
            Some(name) => Some(name),
            None => req.key.as_deref().map(agent_name_from_key),
        };
        let priority = req.priority.unwrap_or(0);
        let spec = SpawnSpec {
            channel: req.channel.clone(),
            prompt,
            harness: Some(harness),
            host,
            name,
            model,
            role: resolved.role.clone(),
            task: None,
            stop_on_close: req.stop_on_close,
            continues: req.continues,
            priority: Some(priority),
            bundles: req.bundles.clone(),
        };

        // A retry with a key returns the first agent, or conflicts.
        if let Some(key) = &req.key
            && let Some(existing) = self.agent_by_spawn_key(key).await?
        {
            self.same_spawn(&existing, &spec, req.task.as_ref()).await?;
            return Ok(json!({ "agent": { "agent": existing.id, "placed": true } }));
        }

        if let Some(role) = &resolved.role
            && let Some(one_per) = resolved.one_per
            && self
                .role_taken(
                    role,
                    one_per,
                    resolved.collection.as_ref(),
                    ch.project.as_ref(),
                    None,
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

        // The bound task: an existing one, or a new one with the title.
        let task = match &req.task {
            None => None,
            Some(TaskBinding::Existing { channel, id }) => {
                let task = self.task(channel, id).await?;
                Some(TaskRef {
                    channel: channel.clone(),
                    id: task.value.id,
                })
            }
            Some(TaskBinding::New { channel, title }) => {
                // A keyed spawn makes one task even across a retry that the
                // agent lookup does not catch.
                let create_key = req.key.as_ref().map(|k| format!("spawn:{k}"));
                let created = self
                    .task_create(
                        &by,
                        channel,
                        NewTask {
                            title: title.clone(),
                            ..NewTask::default()
                        },
                        create_key,
                    )
                    .await?;
                Some(TaskRef {
                    channel: channel.clone(),
                    id: created.id,
                })
            }
        };
        let stop_on_close = req.stop_on_close.unwrap_or(task.is_some());

        // A channel with a project gives the repository; without one, the new
        // agent works in the repository of its parent.
        let repo = match &ch.project {
            Some(p) => {
                let project = self.project(p).await?;
                Some(RepoInfo {
                    branch: ch.branch.clone().unwrap_or(project.default_branch),
                    remote_url: project.remote_url,
                    subdir: String::new(),
                })
            }
            None => parent.as_ref().and_then(|p| p.repo.clone()),
        };
        let id = match &spec.name {
            Some(name) => name.clone(),
            None => self.new_agent_id(harness).await?,
        };
        let agent = Agent {
            id: id.clone(),
            spawn_key: req.key.clone(),
            task: task.clone(),
            stop_on_close,
            continues: req.continues.map(ContinueState::new),
            retired: false,
            paused: None,
            preferred_harness: harness,
            movable: repo.is_some(),
            repo,
            channels: Vec::new(),
            session: None,
            host: None,
            harness_session_id: None,
            created: self.now(),
            model_harness: Some(harness),
            model: spec.model.clone(),
            project: ch.project,
            members: Default::default(),
            pinned: resolved.pinned,
            role: resolved.role.clone(),
            collection: resolved.collection.clone(),
            bundles: req.bundles.clone(),
            parent: parent.as_ref().map(|p| p.id.clone()),
        };
        self.store
            .put(&keys::agent(&id), &agent, None)
            .await
            .map_err(|e| ApiError::new(e.code, format!("agent {id}: {}", e.message)))?;
        self.join(&id, &req.channel).await?;
        // The join added the channel, so the first placement merges the
        // channel's bundles too.
        let agent = self.agent(&id).await?.value;

        let record = SpawnRecord {
            agent: id.clone(),
            spec: SpawnSpec { task, ..spec },
            resolved,
            by: by.clone(),
            queued_at: self.now(),
            priority,
            state: SpawnState::Queued,
            attempts: 0,
            last_reject: Vec::new(),
            queue_reason: None,
            launch_failures: Vec::new(),
        };
        self.store.upsert(&keys::spawn(&id), &record).await?;
        let spawned = self
            .event(
                self.now(),
                Severity::Info,
                SwarmEventBody::AgentSpawned {
                    by,
                    harness: agent.preferred_harness,
                },
            )
            .about_agent(id.clone())
            .about_channel(req.channel.clone());
        Self::queue(mem, spawned);
        let queued = self
            .event(
                self.now(),
                Severity::Info,
                SwarmEventBody::SpawnQueued {
                    key: agent.spawn_key.clone(),
                    priority,
                },
            )
            .about_agent(id.clone());
        Self::queue(mem, queued);

        // The agent has no session yet. An expired lease says so, and the
        // takeover, prune, and pin rules read it like any waiting agent.
        let lease_key = keys::lease_agent(&id);
        if self.store.get::<Lease>(&lease_key).await?.is_none() {
            let lease = Lease {
                agent: id.clone(),
                token: FencingToken(0),
                state: LeaseState::Expired {
                    since: self.now().minus(self.cfg.lease.grace),
                    last_session: None,
                },
            };
            self.store.upsert(&lease_key, &lease).await?;
        }

        // The first placement try, before the tick. A channel at its cap keeps
        // the spawn queued, with the reason that `spawn_queue` shows.
        let members = self.channel_members(&req.channel).await?;
        let placed = match self.channel_full(&req.channel, members) {
            Some(reason) => {
                let mut held = record.clone();
                held.queue_reason = Some(reason.clone());
                self.store.upsert(&keys::spawn(&id), &held).await?;
                tracing::info!(agent = %id, channel = %req.channel, reason, "spawn queued");
                false
            }
            None => self.place_record(mem, &record, &agent).await?,
        };
        // The reply nests the answer, as the CLI and the host read it.
        Ok(json!({ "agent": { "agent": id, "placed": placed } }))
    }

    /// True when the spawn of the agent is still queued or launching, so it
    /// holds the one_per slot of its role even before a host takes it.
    pub(crate) async fn spawn_pending(&self, agent: &AgentId) -> Result<bool, ApiError> {
        Ok(self
            .store
            .get::<SpawnRecord>(&keys::spawn(agent))
            .await?
            .is_some_and(|d| matches!(d.value.state, SpawnState::Queued | SpawnState::Launching)))
    }

    /// The agent that a spawn key already made, if any.
    async fn agent_by_spawn_key(&self, key: &str) -> Result<Option<Agent>, ApiError> {
        Ok(self
            .store
            .list::<Agent>(keys::AGENT_PREFIX)
            .await?
            .into_iter()
            .map(|d| d.value)
            .find(|a| a.spawn_key.as_deref() == Some(key)))
    }

    /// Whether a retry of a keyed spawn names the same spawn. The task id is
    /// the first agent's on both sides; a new task binding must also name the
    /// same title.
    async fn same_spawn(
        &self,
        existing: &Agent,
        spec: &SpawnSpec,
        binding: Option<&TaskBinding>,
    ) -> Result<(), ApiError> {
        let conflict = || {
            ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "the key {} names a different spawn",
                    existing.spawn_key.as_deref().unwrap_or("-")
                ),
            )
        };
        let record = self
            .store
            .get::<SpawnRecord>(&keys::spawn(&existing.id))
            .await?;
        let Some(record) = record else {
            // The record of a finished spawn is gone; the key is enough.
            return Ok(());
        };
        let mut previous = record.value.spec.clone();
        previous.task = None;
        let mut wanted = spec.clone();
        wanted.task = None;
        spawn_key_matches(&previous, &wanted).map_err(|_| conflict())?;
        if let Some(TaskBinding::New { title, .. }) = binding {
            if let Some(task) = &record.value.spec.task {
                let found = self.task(&task.channel, &task.id).await?;
                if &found.value.title != title {
                    return Err(conflict());
                }
            } else {
                return Err(conflict());
            }
        }
        Ok(())
    }

    /// The spawns that wait for a placement or a launch, in placement order:
    /// highest priority first, then the oldest.
    async fn pending_spawns(&self) -> Result<Vec<SpawnRecord>, ApiError> {
        let mut queued: Vec<SpawnRecord> = self
            .store
            .list::<SpawnRecord>(keys::SPAWN_PREFIX)
            .await?
            .into_iter()
            .map(|d| d.value)
            .filter(|r| r.state == SpawnState::Queued || r.state == SpawnState::Launching)
            .collect();
        queued.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.queued_at.cmp(&b.queued_at))
                .then_with(|| a.agent.cmp(&b.agent))
        });
        Ok(queued)
    }

    /// How many spawns wait for a placement or a launch, for
    /// `borant_spawn_queue_length`.
    pub(crate) async fn spawn_queue_length(&self) -> Result<usize, ApiError> {
        Ok(self.pending_spawns().await?.len())
    }

    /// Places the queued spawns, highest priority first, then the oldest. A
    /// record that does not fit does not block the ones after it.
    pub(crate) async fn place_queued(&self) -> Result<(), ApiError> {
        for record in self.pending_spawns().await? {
            let Ok(agent) = self.agent(&record.agent).await else {
                // The agent row and the record disagree: drop the record.
                self.store.delete(&keys::spawn(&record.agent)).await?;
                continue;
            };
            if record.state == SpawnState::Launching && self.launch_in_flight(&record.agent).await?
            {
                continue;
            }
            let placed = {
                // One record at a time, so a request is served between them.
                let mut mem = self.mem.lock().await;
                let placed = self.place_record(&mut mem, &record, &agent.value).await?;
                drop(mem);
                self.flush().await;
                placed
            };
            if placed {
                let mut record = record;
                record.state = SpawnState::Launching;
                record.last_reject.clear();
                self.store
                    .upsert(&keys::spawn(&record.agent), &record)
                    .await?;
            }
        }
        Ok(())
    }

    /// Drops the spawn records of agents that are gone or retired, so
    /// `borant ps` does not list a spawn that stopped. The hub runs this at
    /// start, for the records that a hub before this version left behind, and
    /// a retire drops its own record. Returns how many it dropped.
    pub async fn drop_retired_spawns(&self) -> Result<usize, ApiError> {
        let mut dropped = 0;
        for doc in self.store.list::<SpawnRecord>(keys::SPAWN_PREFIX).await? {
            let stopped = match self
                .store
                .get::<Agent>(&keys::agent(&doc.value.agent))
                .await?
            {
                Some(a) => a.value.retired,
                // A record with no agent row has nothing to place.
                None => true,
            };
            if stopped {
                self.store.delete(&doc.key).await?;
                dropped += 1;
            }
        }
        if dropped > 0 {
            tracing::info!(dropped, "dropped the spawn records of stopped agents");
        }
        Ok(dropped)
    }

    /// True while a launch of the record may still register: the host holds
    /// the lease, or reserved it. A `Launching` record without either goes
    /// back to the queue, so a launch that failed without a `LaunchFailed`
    /// (a host that died) is tried again.
    async fn launch_in_flight(&self, agent: &AgentId) -> Result<bool, ApiError> {
        Ok(self
            .store
            .get::<Lease>(&keys::lease_agent(agent))
            .await?
            .is_some_and(|d| {
                matches!(
                    d.value.state,
                    LeaseState::Held { .. } | LeaseState::Reserved { .. }
                )
            }))
    }

    /// One placement try for one record. `Queued` stays `Queued` when no host
    /// fits; a placed record becomes `Launching`.
    async fn place_record(
        &self,
        mem: &mut Memory,
        record: &SpawnRecord,
        agent: &Agent,
    ) -> Result<bool, ApiError> {
        let key = keys::lease_agent(&record.agent);
        let doc = self.store.get::<Lease>(&key).await?;
        if doc.as_ref().is_some_and(|d| {
            matches!(
                d.value.state,
                LeaseState::Held { .. } | LeaseState::Reserved { .. }
            )
        }) {
            return Ok(true);
        }
        let mut target = MoveTarget {
            host: record.spec.host.clone(),
            harness: record.spec.harness,
        };
        // A forced host that failed this agent must not block the retry; the
        // set clears when a session of the agent registers.
        if target.host.as_ref().is_some_and(|host| {
            mem.failed_hosts
                .get(&record.agent)
                .is_some_and(|hosts| hosts.contains(host))
        }) {
            target.host = None;
        }
        let placed = self
            .place_agent(
                mem,
                agent,
                MoveReason::Spawn,
                target.clone(),
                Some(record.spec.prompt.clone()),
            )
            .await?;
        let mut next = record.clone();
        if placed {
            next.state = SpawnState::Launching;
            next.last_reject.clear();
        } else {
            // The watchdog may have failed the record between the list and
            // this try; a failed spawn waits for a person.
            let current = self
                .store
                .get::<SpawnRecord>(&keys::spawn(&record.agent))
                .await?;
            if current.is_some_and(|d| d.value.state == SpawnState::Failed) {
                return Ok(false);
            }
            next.state = SpawnState::Queued;
            next.last_reject = self.refusals(mem, agent, &target).await?;
        }
        self.store
            .upsert(&keys::spawn(&record.agent), &next)
            .await?;
        Ok(placed)
    }

    /// Why each host refuses the agent now, as `spawn_queue` reports it.
    async fn refusals(
        &self,
        mem: &Memory,
        agent: &Agent,
        target: &MoveTarget,
    ) -> Result<Vec<(HostName, String)>, ApiError> {
        let views = self.host_views(mem, &agent.id).await?;
        let req = self.placement_req(agent, target.host.as_ref()).await;
        match schedule::place(
            agent.preferred_harness,
            target.harness,
            &req,
            &self.launch_bundles(agent),
            &views,
        ) {
            Ok(_) => Ok(Vec::new()),
            Err(list) => Ok(list
                .into_iter()
                .map(|(host, why)| (host, why.to_string()))
                .collect()),
        }
    }

    /// The queue, as `spawn_queue` and `borant ps` show it: the placement
    /// order first.
    pub(crate) async fn spawn_queue(&self) -> Result<Value, ApiError> {
        let mut records: Vec<SpawnRecord> = self
            .store
            .list::<SpawnRecord>(keys::SPAWN_PREFIX)
            .await?
            .into_iter()
            .map(|d| d.value)
            .collect();
        records.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.queued_at.cmp(&b.queued_at))
                .then_with(|| a.agent.cmp(&b.agent))
        });
        let rows: Vec<Value> = records
            .iter()
            .map(|r| {
                json!({
                    "agent": r.agent,
                    "state": r.state,
                    "priority": r.priority,
                    "queued_at": r.queued_at,
                    "attempts": r.attempts,
                    // `borant ps` shows one line of why the hosts refuse it,
                    // or why a failed spawn gave up.
                    "last_reject": reject_text(&r.last_reject),
                    "fail_reason": r.fail_reason(),
                    // The last try that named an error, so `ps --json` and
                    // the reason of a queued spawn carry its cause.
                    "last_error": r.last_launch_error().map(|f| json!({
                        "host": f.host,
                        "text": f.error,
                    })),
                    "queue_reason": r.queue_reason,
                    "by": r.by,
                    "task": r.spec.task,
                    "spec": { "channel": r.spec.channel },
                })
            })
            .collect();
        Ok(Value::Array(rows))
    }

    /// Cancels a spawn. A queued record goes away; a running agent is stopped
    /// and its claims go back to open. A repeat succeeds.
    pub(crate) async fn cancel(
        &self,
        mem: &mut Memory,
        by: &Sender,
        agent: &AgentId,
        close_task: Option<borant_core::model::CloseReason>,
    ) -> Result<Value, ApiError> {
        let key = keys::spawn(agent);
        let record = self.store.get::<SpawnRecord>(&key).await?;
        let existing = self.store.get::<Agent>(&keys::agent(agent)).await?;
        let queued = record
            .as_ref()
            .is_some_and(|d| d.value.state == SpawnState::Queued);
        let mut stopped = false;
        if let Some(doc) = &existing {
            // The task first, so a close reports the task as it was.
            self.cancel_task(mem, by, &doc.value, close_task).await?;
            let lease_key = keys::lease_agent(agent);
            let lease = self.store.get::<Lease>(&lease_key).await?;
            match lease.as_ref().map(|l| &l.value.state) {
                // A running agent stops the way a retired one does: its
                // claims go back to open, and its session stops now when it
                // is idle, else at its next turn end or at the retire grace.
                Some(LeaseState::Held { .. }) => {
                    stopped = self.retire(mem, &doc.value).await?;
                }
                // A launch that has not registered gives its slot back.
                Some(LeaseState::Reserved { .. }) | Some(LeaseState::Expired { .. }) => {
                    let released = lease::release(&lease.as_ref().unwrap().value, self.now());
                    let revision = lease.as_ref().map(|l| l.revision);
                    self.store.put(&lease_key, &released, revision).await?;
                }
                _ => {}
            }
            if queued {
                // A queued spawn never started: drop its record, its
                // membership, and the agent row itself.
                self.unclaim_everywhere(mem, agent).await?;
                for channel in &doc.value.channels {
                    self.store.delete(&keys::member(channel, agent)).await?;
                }
                self.store.delete(&keys::agent(agent)).await?;
                self.store.delete(&lease_key).await?;
            }
            let event = self
                .event(self.now(), Severity::Warn, SwarmEventBody::SpawnCancelled)
                .about_agent(agent.clone());
            Self::queue(mem, event);
        }
        self.store.delete(&key).await?;
        // `stopped` is true only when a stop order went out; a session that
        // still has to end its turn says when its stop comes.
        let mut reply = json!({ "agent": agent, "cancelled": true, "stopped": stopped });
        if !stopped {
            reply["stopping"] = json!("at the turn end");
        }
        Ok(reply)
    }

    /// Closes the task of a cancelled worker, when the call asks for it and
    /// the task is still open.
    async fn cancel_task(
        &self,
        mem: &mut Memory,
        by: &Sender,
        agent: &Agent,
        reason: Option<borant_core::model::CloseReason>,
    ) -> Result<(), ApiError> {
        let (Some(reason), Some(task)) = (reason, agent.task.as_ref()) else {
            return Ok(());
        };
        let doc = self.task(&task.channel, &task.id).await?;
        match borant_core::rules::tasks::close_repeat(&doc.value, reason) {
            borant_core::rules::tasks::CloseOutcome::Close => {
                self.task_close(
                    mem,
                    by,
                    &task.channel,
                    &task.id,
                    reason,
                    format!("the spawn of {} was cancelled", agent.id),
                )
                .await?;
            }
            // A closed task with the same reason is the same cancel.
            borant_core::rules::tasks::CloseOutcome::Repeat => {}
            borant_core::rules::tasks::CloseOutcome::Conflict(had) => {
                return Err(ApiError::new(
                    ApiErrorCode::Conflict,
                    format!("task {} is closed with {had:?}", task.id),
                ));
            }
        }
        Ok(())
    }
}
