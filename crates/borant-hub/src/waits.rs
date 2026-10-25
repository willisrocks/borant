//! Durable waits (phase 6 spec, "Wait tool"): an agent asks to be woken when
//! a task reaches one of the states it names. The wait lives in the store, so
//! the tick reads the task itself, and a hub restart or a lost event loses no
//! wake.

use std::time::Duration;

use borant_core::model::{CloseReason, Envelope, EnvelopeKind, Sender, Target, Task, TaskStatus};
use borant_core::proto::ApiError;
use borant_core::{AgentId, ChannelName, MessageId, TaskId, Timestamp, keys, subjects};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Hub;

/// The longest wait: one day.
pub const MAX_TIMEOUT_SECS: u64 = 86_400;

/// One stored wait, at `wait.<agent>.<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitRecord {
    /// The id that the agent got back.
    pub id: String,
    pub agent: AgentId,
    pub channel: ChannelName,
    pub task: TaskId,
    /// The states that satisfy the wait.
    pub until: Vec<TaskStatus>,
    pub deadline: Timestamp,
    pub created: Timestamp,
}

impl Hub {
    /// `ToolCall::Wait`: stores the wait, or answers at once with
    /// `satisfied: true` when the task already reaches one of `until`.
    pub(crate) async fn wait(
        &self,
        agent: &AgentId,
        channel: &ChannelName,
        task: &TaskId,
        until: Vec<TaskStatus>,
        timeout_secs: u64,
    ) -> Result<Value, ApiError> {
        if timeout_secs > MAX_TIMEOUT_SECS {
            return Err(ApiError::invalid(format!(
                "a wait is at most {MAX_TIMEOUT_SECS} seconds"
            )));
        }
        // A wait on a task that does not exist is a mistake, not a wait.
        let doc = self.task(channel, task).await?;
        let id = ulid::Ulid::generate().to_string();
        if until.contains(&doc.value.status) {
            tracing::info!(%agent, channel = %channel, task = %task, "wait satisfied at once");
            return Ok(json!({"wait": id, "satisfied": true}));
        }
        let now = self.now();
        let record = WaitRecord {
            id: id.clone(),
            agent: agent.clone(),
            channel: channel.clone(),
            task: *task,
            until,
            deadline: now.plus(Duration::from_secs(timeout_secs)),
            created: now,
        };
        self.store
            .put(&keys::wait(agent, &id), &record, None)
            .await?;
        tracing::info!(%agent, wait = %id, channel = %channel, task = %task, "wait stored");
        Ok(json!({"wait": id, "satisfied": false}))
    }

    /// Wakes the agent of every wait whose task reached `until` or whose
    /// deadline passed: one direct message, then the wait goes. A wait whose
    /// wake cannot be published stays for the next tick.
    pub async fn tick_waits(&self) -> Result<(), ApiError> {
        let now = self.now();
        for doc in self.store.list::<WaitRecord>(keys::WAIT_PREFIX).await? {
            let w = doc.value;
            let Some(task) = self
                .store
                .get::<Task>(&keys::task(&w.channel, &w.task))
                .await?
            else {
                // A task is never deleted, so a wait that names none is
                // stale and would sit here forever.
                self.store.delete(&doc.key).await?;
                continue;
            };
            let satisfied = w.until.contains(&task.value.status);
            let timed_out = now >= w.deadline;
            if !satisfied && !timed_out {
                continue;
            }
            let text = if satisfied {
                format!(
                    "task {}/{} is now {}",
                    w.channel,
                    w.task,
                    status_word(&task.value)
                )
            } else {
                format!("wait {} timed out: task {}/{}", w.id, w.channel, w.task)
            };
            let env = Envelope {
                id: MessageId::new(),
                from: Sender::Human,
                to: Target::Agent(w.agent.clone()),
                kind: EnvelopeKind::Direct,
                text,
                urgent: false,
                event: None,
                sent: now,
            };
            if let Err(e) = self.bus.publish(&subjects::agent(&w.agent), &env).await {
                tracing::warn!(agent = %w.agent, wait = %w.id, "wait wake lost: {e}");
                continue;
            }
            self.store.delete(&doc.key).await?;
            tracing::info!(agent = %w.agent, wait = %w.id, satisfied, "wait woke the agent");
        }
        Ok(())
    }
}

/// The word of a state for people: a task closed as done says "done".
fn status_word(task: &Task) -> &'static str {
    match task.status {
        TaskStatus::Closed if task.close_reason == Some(CloseReason::Done) => "done",
        TaskStatus::Open => "open",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::NeedsHuman => "needs_human",
        TaskStatus::Stuck => "stuck",
        TaskStatus::Closed => "closed",
    }
}
