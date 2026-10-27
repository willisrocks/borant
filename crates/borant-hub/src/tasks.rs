//! Task calls (spec section 9).

use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    CloseReason, Envelope, EnvelopeKind, NewTask, Sender, Target, Task, TaskEvent, TaskEventKind,
    TaskPatch, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::{saga, schema, tasks as rules};
use borant_core::{AgentId, ChannelName, CoreError, MessageId, TaskId, keys, subjects};
use serde_json::{Map, Value};

use crate::store::Doc;
use crate::{Hub, Memory};

/// The deadline pass runs at most this often.
const DEADLINE_EVERY: Duration = Duration::from_secs(10);

impl Hub {
    pub(crate) async fn tasks(&self, channel: &ChannelName) -> Result<Vec<Task>, ApiError> {
        Ok(self
            .store
            .list::<Task>(&keys::tasks_prefix(channel))
            .await?
            .into_iter()
            .map(|d| d.value)
            .collect())
    }

    pub(crate) async fn task(
        &self,
        channel: &ChannelName,
        id: &TaskId,
    ) -> Result<Doc<Task>, ApiError> {
        self.store
            .get::<Task>(&keys::task(channel, id))
            .await?
            .ok_or_else(|| CoreError::UnknownTask(*id).into())
    }

    pub(crate) async fn task_list(
        &self,
        channel: &ChannelName,
        ready_only: bool,
    ) -> Result<Vec<Task>, ApiError> {
        let ch = self.channel(channel).await?;
        let all = self.tasks(channel).await?;
        if !ready_only {
            return Ok(all);
        }
        // Rule 2: an unapproved saga has no ready task.
        let ready = saga::ready_in(&ch.value, &all);
        Ok(all.into_iter().filter(|t| ready.contains(&t.id)).collect())
    }

    pub(crate) async fn task_create(
        &self,
        _by: &Sender,
        channel: &ChannelName,
        new: NewTask,
        key: Option<String>,
    ) -> Result<Task, ApiError> {
        let ch = self.channel(channel).await?;
        if new.title.trim().is_empty() {
            return Err(ApiError::invalid("a task needs a title"));
        }
        schema::validate(&ch.value.schema, &new.properties)?;
        let all = self.tasks(channel).await?;
        // A retried create with the same key returns the first task; another
        // title under the same key is a conflict.
        if let Some(key) = &key
            && let Some(had) = all.iter().find(|t| t.create_key.as_ref() == Some(key))
        {
            if had.title == new.title {
                return Ok(had.clone());
            }
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("task key {key} was used for another title"),
            ));
        }
        let id = TaskId::new();
        rules::check_edges(&all, id, &new.blocked_by)?;
        if let Some(p) = new.parent
            && !all.iter().any(|t| t.id == p)
        {
            return Err(CoreError::UnknownTask(p).into());
        }
        // The kind of the channel decides the parent that a create stores,
        // and an epic channel parents a loose task to its root.
        let parent = saga::check_create(&ch.value, &all, &new)?;
        let now = self.now();
        let task = Task {
            id,
            title: new.title,
            body: new.body,
            status: TaskStatus::Open,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            deadline: new.deadline,
            attempts: 0,
            max_attempts: new.max_attempts,
            blocked_by: new.blocked_by,
            parent,
            kind: new.kind,
            issue: new.issue,
            properties: new.properties,
            created: now,
            updated: now,
            create_key: key,
        };
        self.store
            .put(&keys::task(channel, &id), &task, None)
            .await?;
        self.task_event(
            channel,
            &ch.value.owner,
            &task,
            TaskEventKind::Created,
            "created",
        )
        .await?;
        Ok(task)
    }

    pub(crate) async fn task_update(
        &self,
        mem: &mut Memory,
        by: &Sender,
        channel: &ChannelName,
        id: &TaskId,
        patch: TaskPatch,
    ) -> Result<Task, ApiError> {
        let ch = self.channel(channel).await?;
        let mut doc = self.task(channel, id).await?;
        let before = doc.value.clone();
        let t = &mut doc.value;
        if t.status == TaskStatus::Closed {
            return Err(CoreError::TaskClosed.into());
        }
        if let Some(title) = patch.title {
            t.title = title;
        }
        if let Some(body) = patch.body {
            t.body = body;
        }
        if let Some(status) = patch.status {
            rules::check_status_change(t.status, status)?;
            if status == TaskStatus::Open {
                // A reopen ends the claim that the task still holds, one
                // attempt. A task at its retry limit stays in front of its
                // owner until the owner or a person reopens it, and that
                // reopen starts the count again.
                let from = t.status;
                if at_retry_limit(t) {
                    if !is_owner_or_person(&ch.value.owner, by) {
                        return Err(ApiError::new(
                            ApiErrorCode::Conflict,
                            format!(
                                "task {} reached its retry limit ({} tries); only the owner reopens it",
                                t.id, t.attempts
                            ),
                        ));
                    }
                    t.attempts = 0;
                    t.claimed_by = None;
                    t.status = status;
                } else {
                    t.status = release_claim(t, rules::AttemptEnd::Reopened(from));
                }
            } else {
                t.status = status;
            }
            if !matches!(t.status, TaskStatus::NeedsHuman | TaskStatus::Stuck) {
                t.attention_msg = None;
            }
        }
        if let Some(msg) = patch.attention_msg {
            t.attention_msg = Some(msg);
        }
        if let Some(deadline) = patch.deadline {
            t.deadline = Some(deadline);
        }
        if let Some(max) = patch.max_attempts {
            t.max_attempts = Some(max);
        }
        if let Some(issue) = patch.issue {
            t.issue = Some(issue);
        }
        if let Some(blocked_by) = patch.blocked_by {
            let all = self.tasks(channel).await?;
            let others: Vec<Task> = all.into_iter().filter(|o| o.id != *id).collect();
            rules::check_edges(&others, *id, &blocked_by)?;
            t.blocked_by = blocked_by;
        }
        if let Some(parent) = patch.parent {
            if parent == *id {
                return Err(CoreError::SelfEdge.into());
            }
            self.task(channel, &parent).await?;
            // The new parent must not be a child of this task.
            let all = self.tasks(channel).await?;
            saga::check_parent(&ch.value, &all, t, parent)?;
            let mut at = Some(parent);
            let mut path = vec![*id, parent];
            while let Some(p) = at {
                at = all.iter().find(|x| x.id == p).and_then(|x| x.parent);
                if let Some(next) = at {
                    path.push(next);
                    if next == *id {
                        return Err(CoreError::Cycle { path }.into());
                    }
                    if path.len() > all.len() + 2 {
                        break;
                    }
                }
            }
            t.parent = Some(parent);
        }
        if let Some(props) = patch.properties {
            let mut merged: Map<String, Value> = t.properties.clone();
            for (k, v) in props {
                if v.is_null() {
                    merged.remove(&k);
                } else {
                    merged.insert(k, v);
                }
            }
            schema::validate(&ch.value.schema, &merged)?;
            t.properties = merged;
        }
        t.updated = self.now();
        self.store
            .put(&doc.key, &doc.value, Some(doc.revision))
            .await?;
        self.task_event(
            channel,
            &ch.value.owner,
            &doc.value,
            TaskEventKind::Updated,
            "updated",
        )
        .await?;
        self.task_status_event(mem, channel, &before, &doc.value);
        // Rule 2: the gate goes to a question, so the approvers of the
        // collection hear once, on the change into `needs_human`.
        if before.status != TaskStatus::NeedsHuman
            && doc.value.status == TaskStatus::NeedsHuman
            && ch.value.gate == Some(*id)
            && let Err(e) = self.notify_approvers(&ch.value, &doc.value).await
        {
            tracing::warn!(%channel, task = %id, "the approver message was lost: {e}");
        }
        // A worker bound to this task with `stop_on_close` retires when the
        // update closes the task, not on any other field.
        if doc.value.status == TaskStatus::Closed {
            self.retire_for_task(mem, channel, id).await?;
        }
        Ok(doc.value)
    }

    /// Queues `task.status` when the status or the claim changed. A title, a
    /// body, or a property update publishes nothing.
    pub(crate) fn task_status_event(
        &self,
        mem: &mut Memory,
        channel: &ChannelName,
        before: &Task,
        after: &Task,
    ) {
        if before.status == after.status && before.claimed_by == after.claimed_by {
            return;
        }
        let severity = match after.status {
            TaskStatus::Stuck | TaskStatus::NeedsHuman => Severity::Warn,
            _ => Severity::Info,
        };
        let mut event = self
            .event(
                self.now(),
                severity,
                SwarmEventBody::TaskStatus {
                    from: before.status,
                    to: after.status,
                    claimed_by: after.claimed_by.clone(),
                    attention_msg: after.attention_msg.clone(),
                },
            )
            .about_channel(channel.clone())
            .about_task(after.id);
        if let Some(by) = &after.claimed_by {
            event = event.about_agent(by.clone());
        }
        Self::queue(mem, event);
    }

    pub(crate) async fn task_claim(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        channel: &ChannelName,
        id: &TaskId,
    ) -> Result<Task, ApiError> {
        let ch = self.channel(channel).await?;
        let mut doc = self.task(channel, id).await?;
        let before = doc.value.clone();
        // Rules 2 and 5, choice C7: a claim sees the gate and the blockers of
        // the channel.
        let all = self.tasks(channel).await?;
        saga::check_claim(&ch.value, &all, &doc.value)?;
        match &doc.value.claimed_by {
            Some(other) if other != agent => return Err(CoreError::Claimed(other.clone()).into()),
            _ => {}
        }
        // At its retry limit the task waits for its owner: no claim until the
        // owner or a person reopens it.
        if at_retry_limit(&doc.value) {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "task {} reached its retry limit ({} tries); only the owner reopens it",
                    doc.value.id, doc.value.attempts
                ),
            ));
        }
        // A question or a stall belongs to its claimer: another agent may not
        // claim over it.
        if matches!(doc.value.status, TaskStatus::NeedsHuman | TaskStatus::Stuck)
            && doc.value.claimed_by.as_ref() != Some(agent)
        {
            let who = match &doc.value.claimed_by {
                Some(other) => format!("claimed by {other}"),
                None => "unclaimed".to_string(),
            };
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "task {} is {} and {who}",
                    doc.value.id,
                    match doc.value.status {
                        TaskStatus::NeedsHuman => "in needs_human",
                        _ => "stuck",
                    }
                ),
            ));
        }
        if doc.value.status == TaskStatus::Closed {
            return Err(CoreError::TaskClosed.into());
        }
        doc.value.claimed_by = Some(agent.clone());
        doc.value.status = TaskStatus::InProgress;
        doc.value.updated = self.now();
        self.store
            .put(&doc.key, &doc.value, Some(doc.revision))
            .await?;
        self.task_event(
            channel,
            &ch.value.owner,
            &doc.value,
            TaskEventKind::Claimed,
            &format!("claimed by {agent}"),
        )
        .await?;
        self.task_status_event(mem, channel, &before, &doc.value);
        Ok(doc.value)
    }

    pub(crate) async fn task_close(
        &self,
        mem: &mut Memory,
        by: &Sender,
        channel: &ChannelName,
        id: &TaskId,
        reason: CloseReason,
        message: String,
    ) -> Result<Task, ApiError> {
        let ch = self.channel(channel).await?;
        // Rule 3 and choice C4: the gate of a saga closes, for every reason,
        // only by a person or an approver. The owner of the channel is bound
        // by the same rule as anyone else.
        if ch.value.gate == Some(*id) {
            if !self.is_approver(by).await? {
                return Err(ApiError::new(
                    ApiErrorCode::Denied,
                    "only a person or an approver closes the gate of a saga",
                ));
            }
        } else if reason == CloseReason::Done && !is_owner_or_person(&ch.value.owner, by) {
            // Only the owner of the channel and a person close a task as
            // done. A channel with no owner has only a person: an agent
            // always needs one.
            return Err(ApiError::new(
                ApiErrorCode::Denied,
                "only the owner closes a task as done",
            ));
        }
        let all = self.tasks(channel).await?;
        let mut doc = self.task(channel, id).await?;
        let before = doc.value.clone();
        // A retried close is idempotent: the same reason returns the task.
        match rules::close_repeat(&doc.value, reason) {
            rules::CloseOutcome::Close => {}
            rules::CloseOutcome::Repeat => return Ok(doc.value),
            rules::CloseOutcome::Conflict(had) => {
                return Err(ApiError::new(
                    ApiErrorCode::Conflict,
                    format!("task {} is already closed as {had:?}", doc.value.id),
                ));
            }
        }
        rules::check_close(&all, &doc.value)?;
        doc.value.status = TaskStatus::Closed;
        doc.value.close_reason = Some(reason);
        doc.value.close_message = Some(message);
        doc.value.attention_msg = None;
        doc.value.updated = self.now();
        self.store
            .put(&doc.key, &doc.value, Some(doc.revision))
            .await?;
        self.task_event(
            channel,
            &ch.value.owner,
            &doc.value,
            TaskEventKind::Closed,
            "closed",
        )
        .await?;
        self.task_status_event(mem, channel, &before, &doc.value);
        // A worker bound to this task with `stop_on_close` retires now.
        self.retire_for_task(mem, channel, id).await?;
        Ok(doc.value)
    }

    /// Clears every claim of an agent and sets the tasks to `open`.
    pub(crate) async fn unclaim_all(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
        channels: &[ChannelName],
    ) -> Result<usize, ApiError> {
        let mut n = 0;
        for c in channels {
            let Ok(ch) = self.channel(c).await else {
                continue;
            };
            for mut doc in self.store.list::<Task>(&keys::tasks_prefix(c)).await? {
                if doc.value.claimed_by.as_ref() != Some(agent)
                    || doc.value.status == TaskStatus::Closed
                {
                    continue;
                }
                let before = doc.value.clone();
                doc.value.status = release_claim(&mut doc.value, rules::AttemptEnd::ClaimEnded);
                doc.value.updated = self.now();
                self.store
                    .put(&doc.key, &doc.value, Some(doc.revision))
                    .await?;
                self.task_event(
                    c,
                    &ch.value.owner,
                    &doc.value,
                    TaskEventKind::Unclaimed,
                    &format!("released: {agent} did not return"),
                )
                .await?;
                self.task_status_event(mem, c, &before, &doc.value);
                n += 1;
            }
        }
        Ok(n)
    }

    /// Clears every claim of an agent in every channel.
    pub(crate) async fn unclaim_everywhere(
        &self,
        mem: &mut Memory,
        agent: &AgentId,
    ) -> Result<usize, ApiError> {
        let channels: Vec<ChannelName> = self
            .store
            .list::<serde_json::Value>(keys::CHANNEL_PREFIX)
            .await?
            .into_iter()
            .filter(|d| d.key.matches('.').count() == 1)
            .filter_map(|d| {
                d.key
                    .strip_prefix(keys::CHANNEL_PREFIX)
                    .and_then(|c| c.parse().ok())
            })
            .collect();
        self.unclaim_all(mem, agent, &channels).await
    }

    /// Puts every task whose deadline passed in front of its owner: `stuck`
    /// with "deadline passed" once per miss, and one direct message to the
    /// owner. It reassigns nothing (phase 6 spec, "Tasks").
    pub(crate) async fn tick_deadlines(&self) -> Result<(), ApiError> {
        let now = self.now();
        {
            // The pass lists every task, so it runs at most every
            // [`DEADLINE_EVERY`]; a deadline is minutes away, not seconds.
            let mut mem = self.mem.lock().await;
            if mem
                .last_deadlines
                .is_some_and(|last| now.since(last) < DEADLINE_EVERY)
            {
                return Ok(());
            }
            mem.last_deadlines = Some(now);
        }
        for (channel, task) in self.all_tasks().await? {
            let Some(deadline) = task.deadline else {
                continue;
            };
            if deadline > now
                || matches!(
                    task.status,
                    TaskStatus::Closed | TaskStatus::Stuck | TaskStatus::NeedsHuman
                )
            {
                continue;
            }
            let key = keys::task(&channel, &task.id);
            let Some(doc) = self.store.get::<Task>(&key).await? else {
                continue;
            };
            // A request may have moved the task since the scan.
            let before = doc.value;
            if before.deadline.is_none_or(|d| d > now)
                || matches!(
                    before.status,
                    TaskStatus::Closed | TaskStatus::Stuck | TaskStatus::NeedsHuman
                )
            {
                continue;
            }
            let ch = self.channel(&channel).await?;
            let owner = ch.value.owner;
            let mut next = before.clone();
            next.status = TaskStatus::Stuck;
            next.attention_msg = Some("deadline passed".to_owned());
            next.updated = now;
            self.store.put(&key, &next, Some(doc.revision)).await?;
            if let Err(e) = self
                .task_event(
                    &channel,
                    &owner,
                    &next,
                    TaskEventKind::Updated,
                    "deadline passed",
                )
                .await
            {
                tracing::warn!(task = %next.id, "task event lost: {e}");
            }
            let event = self
                .event(
                    now,
                    Severity::Warn,
                    SwarmEventBody::TaskStatus {
                        from: before.status,
                        to: TaskStatus::Stuck,
                        claimed_by: next.claimed_by.clone(),
                        attention_msg: next.attention_msg.clone(),
                    },
                )
                .about_channel(channel.clone())
                .about_task(next.id);
            self.publish(event).await;
            // The owner hears about the miss once; the stuck status keeps the
            // next pass from sending it again. A channel with no owner has
            // nobody to tell.
            if let Some(owner) = owner {
                let env = Envelope {
                    id: MessageId::new(),
                    from: Sender::Human,
                    to: Target::Agent(owner.clone()),
                    kind: EnvelopeKind::Direct,
                    text: format!(
                        "task {channel}/{} ({}) passed its deadline; it is stuck",
                        next.id, next.title
                    ),
                    urgent: false,
                    event: None,
                    sent: now,
                };
                if let Err(e) = self.bus.publish(&subjects::agent(&owner), &env).await {
                    tracing::warn!(task = %next.id, "deadline message to the owner failed: {e}");
                }
            }
            tracing::warn!(task = %next.id, %channel, "the deadline passed: the task is stuck");
        }
        Ok(())
    }

    /// True if the agent has a claim on an open task in one of its channels.
    pub(crate) async fn has_claims(
        &self,
        agent: &AgentId,
        channels: &[ChannelName],
    ) -> Result<bool, ApiError> {
        for c in channels {
            if self
                .tasks(c)
                .await?
                .iter()
                .any(|t| t.claimed_by.as_ref() == Some(agent) && t.status != TaskStatus::Closed)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) async fn task_event(
        &self,
        channel: &ChannelName,
        owner: &Option<AgentId>,
        task: &Task,
        kind: TaskEventKind,
        what: &str,
    ) -> Result<(), ApiError> {
        let status = serde_json::to_value(task.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let mut text = format!(
            "Task '{}' ({}) {what}. Status: {status}.",
            task.title, task.id
        );
        if let Some(m) = &task.attention_msg {
            text.push_str(&format!(" Reason: {m}"));
        }
        let env = Envelope {
            id: MessageId::new(),
            from: Sender::Human,
            to: Target::Channel(channel.clone()),
            kind: EnvelopeKind::Event,
            text,
            urgent: false,
            event: Some(TaskEvent {
                channel: channel.clone(),
                task: task.id,
                title: task.title.clone(),
                kind,
                status: task.status,
                attention_msg: task.attention_msg.clone(),
                owner: owner.clone(),
            }),
            sent: self.now(),
        };
        if let Err(e) = self.bus.publish(&subjects::tasks(channel), &env).await {
            // The write succeeded. A lost event is a known limit (spec 6).
            tracing::warn!(%channel, task = %task.id, "task event lost: {e}");
        }
        Ok(())
    }
}

/// Whether `by` is the owner of the channel or a person.
fn is_owner_or_person(owner: &Option<AgentId>, by: &Sender) -> bool {
    match by {
        Sender::Human => true,
        Sender::Agent(agent) => owner.as_ref() == Some(agent),
    }
}

/// True when the task has used up its retry limit.
fn at_retry_limit(task: &Task) -> bool {
    task.max_attempts.is_some_and(|max| task.attempts >= max)
}

/// Applies one ended claim to the task: it counts an attempt, and at the retry
/// limit the task goes back in front of its owner with `needs_human`. Returns
/// the status the caller must set, and clears the claim.
fn release_claim(task: &mut Task, transition: rules::AttemptEnd) -> TaskStatus {
    let counted = rules::attempt_ended(task, transition);
    task.claimed_by = None;
    if !counted {
        return TaskStatus::Open;
    }
    task.attempts += 1;
    if at_retry_limit(task) {
        task.attention_msg = Some(format!("retry limit reached ({} tries)", task.attempts));
        return TaskStatus::NeedsHuman;
    }
    TaskStatus::Open
}
