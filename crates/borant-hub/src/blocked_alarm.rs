//! The blocked-work alarm (blocked-work alarm spec).
//!
//! Every 60 s the hub looks for work that waits on a person and that nobody
//! told them about: a spawn that sits in the queue for a refusal that a wait
//! cannot clear, and a claimed task whose claimer can no longer work it. The
//! spawn alarm tells the person once through the `[alerts]` notifier, and the
//! spawner and the chief of the collection by direct message, and puts the
//! bound task in `stuck`. It repeats at most every four hours while the spawn
//! stays blocked.
//!
//! The rule that classifies a wait lives in `borant_core::rules::blocked`;
//! this module only reads the records, writes the task, and tells.

use std::collections::HashSet;
use std::time::Duration;

use borant_core::model::{
    Agent, Lease, LeaseState, SpawnRecord, SpawnState, Task, TaskEventKind, TaskStatus,
};
use borant_core::ports::{Notice, Priority};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::{blocked, roles};
use borant_core::{AgentId, ChannelName, CollectionName, TaskId, Timestamp, keys};

use crate::{Hub, Memory};

/// How often the blocked-work pass runs.
pub(crate) const BLOCKED_EVERY: Duration = Duration::from_secs(60);

/// The task that the alarm set to `stuck` for one blocked spawn, with the
/// status and the attention text that it hid, so that it gives them back when
/// the spawn leaves the queue. The alarm undoes only its own write: the text
/// it wrote must still be the attention text.
#[derive(Debug, Clone)]
pub(crate) struct StuckTask {
    pub channel: ChannelName,
    pub id: TaskId,
    /// `Open` for an unclaimed task, `InProgress` for a claimed one.
    pub before: TaskStatus,
    /// The attention text that the alarm replaced.
    pub attention: Option<String>,
    /// The alarm's own text, which the task must still carry.
    pub text: String,
}

impl Hub {
    /// Runs the blocked-work pass when its time has come, at most once a
    /// [`BLOCKED_EVERY`]. A failure is a warn, never a failed tick.
    pub(crate) async fn tick_blocked(&self) -> Result<(), ApiError> {
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_blocked
                .is_some_and(|last| now.since(last) < BLOCKED_EVERY)
            {
                return Ok(());
            }
            mem.last_blocked = Some(now);
        }
        if let Err(e) = self.blocked_pass(now).await {
            tracing::warn!("blocked-work pass failed: {e}");
        }
        Ok(())
    }

    async fn blocked_pass(&self, now: Timestamp) -> Result<(), ApiError> {
        self.report_blocked_spawns(now).await?;
        self.report_stalled_cards(now).await;
        Ok(())
    }

    /// The queued spawns that wait on a person: one alert, the spawner and
    /// the chief, and the bound task in `stuck`, at most every
    /// [`blocked::REPEAT_EVERY`] while it stays blocked.
    ///
    /// A spawn that leaves the queue gives its bound task back first: an
    /// `Open` task that the alarm stuck is one that the new worker must
    /// claim, and `task_claim` refuses a stalled task that nobody claims.
    async fn report_blocked_spawns(&self, now: Timestamp) -> Result<(), ApiError> {
        let records = self.store.list::<SpawnRecord>(keys::SPAWN_PREFIX).await?;
        let queued: HashSet<&AgentId> = records
            .iter()
            .filter(|doc| doc.value.state == SpawnState::Queued)
            .map(|doc| &doc.value.agent)
            .collect();
        // A spawn that is placed, cancels, or ends is no longer blocked.
        let left: Vec<AgentId> = {
            let mem = self.mem.lock().await;
            mem.blocked_tasks
                .keys()
                .filter(|agent| !queued.contains(agent))
                .cloned()
                .collect()
        };
        for agent in left {
            let mut mem = self.mem.lock().await;
            self.release_blocked_task(&mut mem, &agent).await;
            drop(mem);
            self.flush().await;
        }
        // A spawn that leaves the queue is forgotten, so a later block tells
        // at once again.
        {
            let mut mem = self.mem.lock().await;
            mem.blocked_told.retain(|agent, _| queued.contains(agent));
        }
        for doc in &records {
            let record = &doc.value;
            let Some(blocked) = blocked::blocked(record, now) else {
                continue;
            };
            {
                let mem = self.mem.lock().await;
                if mem
                    .blocked_told
                    .get(&record.agent)
                    .is_some_and(|at| now.since(*at) < blocked::REPEAT_EVERY)
                {
                    continue;
                }
            }
            let agent = &record.agent;
            let text = blocked.text(agent);
            tracing::warn!(
                %agent,
                channel = %record.spec.channel,
                reason = %blocked.reason,
                "a spawn is blocked"
            );
            self.notify_blocked(agent, &record.spec.channel, &text)
                .await;
            self.tell_blocked(record, &text).await;
            self.stick_bound_task(record, &text).await?;
            self.mem
                .lock()
                .await
                .blocked_told
                .insert(agent.clone(), now);
        }
        Ok(())
    }

    /// The one alert for the person, through the notifier of `[alerts]`.
    async fn notify_blocked(&self, agent: &AgentId, channel: &ChannelName, text: &str) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        let notice = Notice::new("A spawn is blocked", text.to_owned())
            .with_priority(Priority::High)
            .with_tags(["blocked_spawn".to_owned(), format!("#{channel}")]);
        if let Err(e) = notifier.notify(&notice).await {
            // The next pass tells again after `REPEAT_EVERY`.
            tracing::warn!(%agent, "the blocked-spawn alert failed: {e}");
        }
    }

    /// One direct message to the spawner and one to the chief of the spawn's
    /// collection, when each exists and is not the spawn.
    async fn tell_blocked(&self, record: &SpawnRecord, text: &str) {
        let mut told: HashSet<AgentId> = HashSet::new();
        if let Some(spawner) = blocked::spawner(&record.by) {
            told.insert(spawner);
        }
        let collection = self
            .channel(&record.spec.channel)
            .await
            .ok()
            .and_then(|doc| doc.value.collection);
        if let Some(chief) = self.chief_of(&collection).await {
            told.insert(chief);
        }
        told.remove(&record.agent);
        for who in told {
            let Ok(doc) = self.agent(&who).await else {
                continue;
            };
            if doc.value.retired {
                continue;
            }
            if let Err(e) = self.tell(&who, text).await {
                tracing::warn!(agent = %record.agent, %who, "the blocked-spawn message failed: {e}");
            }
        }
    }

    /// The chief of a collection: the agent that the collection holds whose
    /// role approves saga gates, which is how `hub.toml` marks the chief.
    /// `None` when the collection has none.
    pub(crate) async fn chief_of(&self, collection: &Option<CollectionName>) -> Option<AgentId> {
        let mut found: Option<AgentId> = None;
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await.ok()? {
            let agent = doc.value;
            if agent.retired || &agent.collection != collection {
                continue;
            }
            if !roles::approves(agent.role.as_ref(), &self.cfg.roles) {
                continue;
            }
            if found.as_ref().is_none_or(|first| agent.id < *first) {
                found = Some(agent.id);
            }
        }
        found
    }

    /// Sets the task that the spawn binds to `stuck`, with the alarm text, so
    /// it shows in `needs_you` and in `borant inbox`. A task that a person
    /// already holds keeps its status. The status and the text that the
    /// alarm hides are kept for [`Self::release_bound_task`].
    async fn stick_bound_task(&self, record: &SpawnRecord, text: &str) -> Result<(), ApiError> {
        let Some(bound) = &record.spec.task else {
            return Ok(());
        };
        let key = keys::task(&bound.channel, &bound.id);
        let Some(doc) = self.store.get::<Task>(&key).await? else {
            return Ok(());
        };
        let before = doc.value;
        if !matches!(before.status, TaskStatus::Open | TaskStatus::InProgress) {
            // A task that this hub instance already stuck keeps its record.
            // One that a restarted hub stuck keeps the alarm's own text, so
            // the release still knows what to give back.
            if before.status == TaskStatus::Stuck
                && before.attention_msg.as_deref() == Some(text)
                && !self
                    .mem
                    .lock()
                    .await
                    .blocked_tasks
                    .contains_key(&record.agent)
            {
                let previous = if before.claimed_by.is_some() {
                    TaskStatus::InProgress
                } else {
                    TaskStatus::Open
                };
                self.mem.lock().await.blocked_tasks.insert(
                    record.agent.clone(),
                    StuckTask {
                        channel: bound.channel.clone(),
                        id: bound.id,
                        before: previous,
                        attention: None,
                        text: text.to_owned(),
                    },
                );
            }
            return Ok(());
        }
        let mut next = before.clone();
        next.status = TaskStatus::Stuck;
        next.attention_msg = Some(text.to_owned());
        next.updated = self.now();
        match self.store.put(&key, &next, Some(doc.revision)).await {
            Ok(_) => {}
            // A request changed the task between the read and the write.
            Err(e) if e.code == ApiErrorCode::Conflict => return Ok(()),
            Err(e) => return Err(e),
        }
        self.mem.lock().await.blocked_tasks.insert(
            record.agent.clone(),
            StuckTask {
                channel: bound.channel.clone(),
                id: bound.id,
                before: before.status,
                attention: before.attention_msg.clone(),
                text: text.to_owned(),
            },
        );
        tracing::warn!(agent = %record.agent, task = %next.id, "the spawn of a task is blocked");
        let owner = match self.channel(&bound.channel).await {
            Ok(doc) => doc.value.owner,
            Err(e) => {
                tracing::warn!(channel = %bound.channel, "the channel of a bound task is gone: {e}");
                None
            }
        };
        if let Err(e) = self
            .task_event(
                &bound.channel,
                &owner,
                &next,
                TaskEventKind::Updated,
                "its spawn is blocked",
            )
            .await
        {
            tracing::warn!(task = %next.id, "task event lost: {e}");
        }
        {
            let mut mem = self.mem.lock().await;
            self.task_status_event(&mut mem, &bound.channel, &before, &next);
        }
        self.flush().await;
        Ok(())
    }

    /// Gives the task of a spawn that left the queue back the status and the
    /// attention text that the alarm hid, when they are still the alarm's
    /// own: an `Open` task waits for the worker that the placement starts,
    /// and `task_claim` refuses a stalled task that nobody holds. The record
    /// comes out of `mem`, so a caller that holds the request's memory (a
    /// cancel) calls this without a second lock.
    pub(crate) async fn release_blocked_task(&self, mem: &mut Memory, agent: &AgentId) {
        let Some(stuck) = mem.blocked_tasks.remove(agent) else {
            return;
        };
        if let Some((before, next)) = self.restore_task(agent, &stuck).await {
            self.task_status_event(mem, &stuck.channel, &before, &next);
        }
    }

    /// Puts the task of `stuck` back, when the alarm still owns the change.
    /// Returns the status change for its event.
    async fn restore_task(&self, agent: &AgentId, stuck: &StuckTask) -> Option<(Task, Task)> {
        let key = keys::task(&stuck.channel, &stuck.id);
        let Ok(Some(doc)) = self.store.get::<Task>(&key).await else {
            return None;
        };
        let before = doc.value;
        // A person, the claimer, or another rule changed the task since: only
        // the alarm's own change is undone.
        if before.status != TaskStatus::Stuck
            || before.attention_msg.as_deref() != Some(stuck.text.as_str())
        {
            return None;
        }
        let mut next = before.clone();
        next.status = stuck.before;
        next.attention_msg = stuck.attention.clone();
        next.updated = self.now();
        match self.store.put(&key, &next, Some(doc.revision)).await {
            Ok(_) => {}
            Err(e) if e.code == ApiErrorCode::Conflict => return None,
            Err(e) => {
                tracing::warn!(%agent, task = %stuck.id, "the blocked-spawn release failed: {e}");
                return None;
            }
        }
        tracing::info!(%agent, task = %next.id, "the spawn left the queue: the task is back");
        let owner = match self.channel(&stuck.channel).await {
            Ok(doc) => doc.value.owner,
            Err(e) => {
                tracing::warn!(channel = %stuck.channel, "the channel of a bound task is gone: {e}");
                None
            }
        };
        if let Err(e) = self
            .task_event(
                &stuck.channel,
                &owner,
                &next,
                TaskEventKind::Updated,
                "its spawn left the queue",
            )
            .await
        {
            tracing::warn!(task = %next.id, "task event lost: {e}");
        }
        Some((before, next))
    }

    /// The claimed tasks whose claimer can no longer work them: it is queued
    /// and blocked, dead, or fenced, and nothing changed the task for
    /// `[alerts] stall_minutes`.
    async fn report_stalled_cards(&self, now: Timestamp) {
        let stall = Duration::from_secs(self.cfg.alerts.stall_minutes.saturating_mul(60));
        let tasks = match self.all_tasks().await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!("the stalled-card scan failed: {e}");
                return;
            }
        };
        let mut stalled: Vec<(ChannelName, Task, AgentId, blocked::ClaimerState)> = Vec::new();
        for (channel, task) in &tasks {
            if task.status != TaskStatus::InProgress {
                continue;
            }
            let Some(claimer) = &task.claimed_by else {
                continue;
            };
            if now.since(task.updated) < stall {
                continue;
            }
            let Some(state) = self.claimer_state(claimer, now).await else {
                continue;
            };
            stalled.push((channel.clone(), task.clone(), claimer.clone(), state));
        }
        // A task that is no longer stalled may report again later.
        let ids: HashSet<TaskId> = stalled.iter().map(|s| s.1.id).collect();
        {
            let mut mem = self.mem.lock().await;
            mem.stalled_told.retain(|id| ids.contains(id));
        }
        for (channel, task, claimer, state) in stalled {
            {
                let mem = self.mem.lock().await;
                if mem.stalled_told.contains(&task.id) {
                    continue;
                }
            }
            let holds: Vec<String> = tasks
                .iter()
                .filter(|(c, other)| {
                    c == &channel
                        && other.status != TaskStatus::Closed
                        && other.blocked_by.contains(&task.id)
                })
                .map(|(_, other)| other.title.clone())
                .collect();
            let attention = blocked::stalled_text(&claimer, &state, &holds);
            if !self.mark_task_stuck(&channel, &task, &attention).await {
                continue;
            }
            tracing::warn!(task = %task.id, claimer = %claimer, "a claimed task is stalled");
            self.tell_stalled(&channel, &task, &claimer, &attention)
                .await;
            self.mem.lock().await.stalled_told.insert(task.id);
        }
    }

    /// The state of a claimer that can no longer work its claim, or `None`
    /// while it can.
    ///
    /// A dead claimer is not one of these: the hub ends it through the
    /// liveness sweep, and the claim timeout returns its claim to open with
    /// one attempt counted, or a pinned agent keeps it for its own host. A
    /// task that stays `in_progress` through those rules is waiting on the
    /// retry machinery, not on a person.
    async fn claimer_state(
        &self,
        claimer: &AgentId,
        now: Timestamp,
    ) -> Option<blocked::ClaimerState> {
        let Ok(doc) = self.agent(claimer).await else {
            return None;
        };
        let agent = doc.value;
        let record = self
            .store
            .get::<SpawnRecord>(&keys::spawn(&agent.id))
            .await
            .ok()
            .flatten()
            .map(|doc| doc.value);
        if let Some(blocked) = record.as_ref().and_then(|r| blocked::blocked(r, now)) {
            return Some(blocked::ClaimerState::Queued(blocked));
        }
        let state = self
            .store
            .get::<Lease>(&keys::lease_agent(&agent.id))
            .await
            .ok()
            .flatten()
            .map(|doc| doc.value.state);
        match &state {
            // A stopped agent whose turn outlived its grace is fenced, so its
            // claim cannot end on its own.
            Some(LeaseState::Held { session, .. }) => {
                let fenced =
                    agent.paused.is_some() && self.mem.lock().await.stopped.contains(session);
                fenced.then_some(blocked::ClaimerState::Fenced)
            }
            // Nothing holds it, or the hub is launching it again.
            _ => None,
        }
    }

    /// Writes `attention` and `stuck` on the task of `channel` that `task`
    /// names, when it is still claimed and in progress. Returns whether it
    /// changed the task.
    async fn mark_task_stuck(&self, channel: &ChannelName, task: &Task, attention: &str) -> bool {
        let key = keys::task(channel, &task.id);
        let Ok(Some(doc)) = self.store.get::<Task>(&key).await else {
            return false;
        };
        let before = doc.value;
        if before.status != TaskStatus::InProgress {
            return false;
        }
        let mut next = before.clone();
        next.status = TaskStatus::Stuck;
        next.attention_msg = Some(attention.to_owned());
        next.updated = self.now();
        match self.store.put(&key, &next, Some(doc.revision)).await {
            Ok(_) => {}
            Err(e) if e.code == ApiErrorCode::Conflict => return false,
            Err(e) => {
                tracing::warn!(task = %task.id, "the stalled-card write failed: {e}");
                return false;
            }
        }
        let owner = match self.channel(channel).await {
            Ok(doc) => doc.value.owner,
            Err(_) => None,
        };
        if let Err(e) = self
            .task_event(
                channel,
                &owner,
                &next,
                TaskEventKind::Updated,
                "its claimer stalled",
            )
            .await
        {
            tracing::warn!(task = %next.id, "task event lost: {e}");
        }
        {
            let mut mem = self.mem.lock().await;
            self.task_status_event(&mut mem, channel, &before, &next);
        }
        self.flush().await;
        true
    }

    /// One direct message about the stalled task, to the owner of its channel
    /// and to the chief of the collection.
    async fn tell_stalled(
        &self,
        channel: &ChannelName,
        task: &Task,
        claimer: &AgentId,
        attention: &str,
    ) {
        let text = blocked::stalled_message(channel, task, attention);
        let mut told: HashSet<AgentId> = HashSet::new();
        let collection = match self.channel(channel).await {
            Ok(doc) => {
                if let Some(owner) = doc.value.owner {
                    told.insert(owner);
                }
                doc.value.collection
            }
            Err(e) => {
                tracing::warn!(%channel, "the channel of a stalled task is gone: {e}");
                None
            }
        };
        if let Some(chief) = self.chief_of(&collection).await {
            told.insert(chief);
        }
        told.remove(claimer);
        for who in told {
            if let Err(e) = self.tell(&who, &text).await {
                tracing::warn!(task = %task.id, %who, "the stalled-card message failed: {e}");
            }
        }
    }
}
