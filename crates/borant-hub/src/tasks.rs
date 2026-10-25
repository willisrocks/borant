//! Task calls (spec section 9).

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    CloseReason, Envelope, EnvelopeKind, NewTask, Sender, Target, Task, TaskEvent, TaskEventKind,
    TaskPatch, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::{schema, tasks as rules};
use borant_core::{AgentId, ChannelName, CoreError, MessageId, TaskId, keys, subjects};
use serde_json::{Map, Value};

use crate::store::Doc;
use crate::{Hub, Memory};

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
        self.channel(channel).await?;
        let all = self.tasks(channel).await?;
        if !ready_only {
            return Ok(all);
        }
        let ready = rules::ready(&all);
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
            deadline: None,
            attempts: 0,
            max_attempts: None,
            blocked_by: new.blocked_by,
            parent: new.parent,
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
        _by: &Sender,
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
            t.status = status;
            if status == TaskStatus::Open {
                t.claimed_by = None;
            }
            if !matches!(status, TaskStatus::NeedsHuman | TaskStatus::Stuck) {
                t.attention_msg = None;
            }
        }
        if let Some(msg) = patch.attention_msg {
            t.attention_msg = Some(msg);
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
        match &doc.value.claimed_by {
            Some(other) if other != agent => return Err(CoreError::Claimed(other.clone()).into()),
            _ => {}
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
        // Only the owner of the channel and a person close a task as done.
        if reason == CloseReason::Done && !is_owner(&ch.value.owner, by) {
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
                doc.value.claimed_by = None;
                doc.value.status = TaskStatus::Open;
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
        owner: &AgentId,
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

/// Whether `by` may close a task as done: the owner of the channel, or a
/// person.
fn is_owner(owner: &AgentId, by: &Sender) -> bool {
    match by {
        Sender::Human => true,
        Sender::Agent(agent) => agent == owner,
    }
}
