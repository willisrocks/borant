//! The human inbox, and answering the question of a task (phase 6 spec,
//! "Human inbox").

use borant_core::model::{
    Agent, Envelope, EnvelopeKind, Sender, Target, TaskEventKind, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::{AgentId, ChannelName, MessageId, TaskId, Timestamp, keys, subjects};
use serde_json::{Value, json};

use crate::{Hub, Memory};

impl Hub {
    /// What waits for a person: the tasks in `needs_human` or `stuck` in every
    /// channel, and the agents that are paused with no claim.
    ///
    /// Oldest first in both lists, because the question that has waited
    /// longest is the one to answer. The age of a task is counted from its
    /// last change (which is when it entered `needs_human` or `stuck`); the
    /// age of an agent from its pause.
    pub(crate) async fn inbox(&self) -> Result<Value, ApiError> {
        let now = self.now();
        let mut tasks = Vec::new();
        for (channel, task) in self.all_tasks().await? {
            if !matches!(task.status, TaskStatus::NeedsHuman | TaskStatus::Stuck) {
                continue;
            }
            tasks.push(json!({
                "channel": channel,
                "id": task.id,
                "title": task.title,
                "status": task.status,
                "claimer": task.claimed_by,
                "attention_msg": task.attention_msg,
                "age_ms": millis(now.since(task.updated)),
            }));
        }
        tasks.sort_by(|a, b| {
            b["age_ms"]
                .as_u64()
                .cmp(&a["age_ms"].as_u64())
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });

        let mut agents = Vec::new();
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            let Some(paused) = &agent.paused else {
                continue;
            };
            // A paused agent that still holds a claim is not waiting for a
            // person to unblock it: its work is in flight.
            if self.has_claims(&agent.id, &agent.channels).await? {
                continue;
            }
            agents.push(json!({
                "agent": agent.id,
                "why": paused.why,
                "by": paused.by,
                "at": paused.at,
                "age_ms": millis(now.since(paused.at)),
            }));
        }
        agents.sort_by(|a, b| {
            b["age_ms"]
                .as_u64()
                .cmp(&a["age_ms"].as_u64())
                .then_with(|| a["agent"].as_str().cmp(&b["agent"].as_str()))
        });

        Ok(json!({ "at": now, "tasks": tasks, "agents": agents }))
    }

    /// Answers a task's question.
    ///
    /// The task must be the one that the person read (`expect_updated`), or
    /// the call is a Conflict: a person who answered a stale screen must read
    /// the task again. A task that a claimer holds goes back to `in_progress`
    /// with no `attention_msg`, and the claimer gets the text as a direct
    /// message from a person. With no claimer the status stays and the text
    /// goes to the channel owner. An answer never clears a pause or a retry
    /// limit: those are the `resume` and `task_update` paths.
    ///
    /// `key` is reserved by the protocol for an idempotent answer; it is not
    /// stored here, so a retry with the `expect_updated` of the first read is
    /// a Conflict and the caller reads the task again.
    pub(crate) async fn answer(
        &self,
        mem: &mut Memory,
        channel: &ChannelName,
        id: &TaskId,
        text: String,
        expect_updated: Timestamp,
    ) -> Result<Value, ApiError> {
        let ch = self.channel(channel).await?;
        let mut doc = self.task(channel, id).await?;
        if doc.value.updated != expect_updated {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!(
                    "task {id} changed at {} since you read it at {expect_updated}",
                    doc.value.updated
                ),
            ));
        }
        if doc.value.status == TaskStatus::Closed {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("task {id} is closed"),
            ));
        }
        if text.trim().is_empty() {
            return Err(ApiError::invalid("an answer needs some text"));
        }

        let Some(claimer) = doc.value.claimed_by.clone() else {
            let Some(owner) = ch.value.owner.clone() else {
                return Err(ApiError::invalid(format!(
                    "#{channel} has no owner to send the answer to"
                )));
            };
            self.tell(&owner, &text).await?;
            return Ok(json!({
                "task": doc.value,
                "to": owner,
                "status": "unchanged",
            }));
        };

        let before = doc.value.clone();
        doc.value.status = TaskStatus::InProgress;
        doc.value.attention_msg = None;
        doc.value.updated = self.now();
        self.store
            .put(&doc.key, &doc.value, Some(doc.revision))
            .await?;
        self.task_event(
            channel,
            &ch.value.owner,
            &doc.value,
            TaskEventKind::Updated,
            "answered by a person",
        )
        .await?;
        self.task_status_event(mem, channel, &before, &doc.value);
        self.tell(&claimer, &text).await?;
        Ok(json!({
            "task": doc.value,
            "to": claimer,
            "status": "in_progress",
        }))
    }

    /// One direct message from a person to an agent. The agent's delivery
    /// wakes it if it is idle, as `borant send` does.
    pub(crate) async fn tell(&self, to: &AgentId, text: &str) -> Result<(), ApiError> {
        let envelope = Envelope {
            id: MessageId::new(),
            from: Sender::Human,
            to: Target::Agent(to.clone()),
            kind: EnvelopeKind::Direct,
            text: text.to_owned(),
            urgent: false,
            event: None,
            sent: self.now(),
        };
        self.bus.publish(&subjects::agent(to), &envelope).await?;
        Ok(())
    }
}

/// A count of milliseconds that fits the wire.
fn millis(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
