//! The human inbox, and answering the question of a task (phase 6 spec,
//! "Human inbox").

use borant_core::events::AnswerVia;
use borant_core::model::{
    Agent, Envelope, EnvelopeKind, Sender, Target, TaskEventKind, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::redact;
use borant_core::{AgentId, ChannelName, MessageId, SessionId, TaskId, Timestamp, keys, subjects};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Hub, Memory};

/// How long the hub keeps an answer key with its result (mod actions spec,
/// "An answer that the mod can confirm").
const ANSWER_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// How often the hub drops the answer keys that outlived their day.
const ANSWER_PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// One answer that an idempotency key stands for, with the result that it
/// returned, so a retried draft sends nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnswerRecord {
    /// The text of the first answer. Another text under the same key is an
    /// edited draft, which needs a new key.
    text: String,
    /// The reply that the first answer returned.
    result: Value,
    /// When the first answer ran, on the hub clock.
    at: Timestamp,
}

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
    /// `key` makes the answer idempotent: the hub keeps the key with its
    /// result for a day, a retry with the same key and text returns that result
    /// and sends nothing, and the same key with other text is a Conflict, so an
    /// edited draft takes a new key. The key is checked before
    /// `expect_updated`, because the retry follows the task change of the
    /// first call. The key also names the durable `task.answered` event.
    pub(crate) async fn answer(
        &self,
        mem: &mut Memory,
        channel: &ChannelName,
        id: &TaskId,
        text: String,
        expect_updated: Timestamp,
        key: Option<&str>,
    ) -> Result<Value, ApiError> {
        let key = key.filter(|k| !k.is_empty());
        if let Some(key) = key
            && let Some(result) = self.answered_with(key, &text).await?
        {
            return Ok(result);
        }
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

        let result = match doc.value.claimed_by.clone() {
            Some(claimer) => {
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
                Self::queue(
                    mem,
                    self.task_answered(
                        channel,
                        id,
                        key.map(str::to_owned),
                        &Sender::Human,
                        AnswerVia::Person,
                    ),
                );
                json!({
                    "task": doc.value,
                    "to": claimer,
                    "status": "in_progress",
                })
            }
            None => {
                let Some(owner) = ch.value.owner.clone() else {
                    return Err(ApiError::invalid(format!(
                        "#{channel} has no owner to send the answer to"
                    )));
                };
                self.tell(&owner, &text).await?;
                Self::queue(
                    mem,
                    self.task_answered(
                        channel,
                        id,
                        key.map(str::to_owned),
                        &Sender::Human,
                        AnswerVia::Person,
                    ),
                );
                json!({
                    "task": doc.value,
                    "to": owner,
                    "status": "unchanged",
                })
            }
        };
        if let Some(key) = key {
            let record = AnswerRecord {
                text,
                result: result.clone(),
                at: self.now(),
            };
            self.store.upsert(&keys::answer(key), &record).await?;
        }
        Ok(result)
    }

    /// Records a chief's relay of the person's decision on a task, and sends
    /// it to the task's owner as a hub-attested message (chief approvals
    /// spec).
    ///
    /// The call layer checks that the caller's role has `approves = true`;
    /// this method checks only the task. The hub redacts the quote, records
    /// `task.approved`, and sends the claimer, or the channel owner when
    /// nobody holds the task, a direct message under [`Sender::Approval`], so
    /// the agent can tell it from agent text.
    ///
    /// A task that waits in `needs_human` is answered too, as a board answer
    /// does: the hub clears the question, sets a claimed task back to
    /// `in_progress`, and records `task.answered` with `via: chief`.
    pub(crate) async fn approve(
        &self,
        mem: &mut Memory,
        channel: &ChannelName,
        id: &TaskId,
        quote: &str,
        by: &AgentId,
        session: SessionId,
    ) -> Result<Value, ApiError> {
        let quote = redact::redact(quote).into_owned();
        if quote.trim().is_empty() {
            return Err(ApiError::invalid("an approval needs the person's words"));
        }
        let ch = self.channel(channel).await?;
        let mut doc = self.task(channel, id).await?;
        if doc.value.status == TaskStatus::Closed {
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("task {id} is closed"),
            ));
        }
        let to = doc
            .value
            .claimed_by
            .clone()
            .or_else(|| ch.value.owner.clone())
            .ok_or_else(|| {
                ApiError::invalid(format!("#{channel} has no owner to send the approval to"))
            })?;
        // The hub records the approval before it sends the message, so the log
        // holds the decision even when the delivery is late.
        Self::queue(mem, self.task_approved(channel, id, by, session, &quote));
        let answered = doc.value.status == TaskStatus::NeedsHuman;
        if answered {
            let before = doc.value.clone();
            if doc.value.claimed_by.is_some() {
                doc.value.status = TaskStatus::InProgress;
            }
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
                "answered by the chief's approval",
            )
            .await?;
            self.task_status_event(mem, channel, &before, &doc.value);
        }
        self.tell_from(Sender::Approval(by.clone()), &to, &quote)
            .await?;
        if answered {
            Self::queue(
                mem,
                self.task_answered(
                    channel,
                    id,
                    None,
                    &Sender::Approval(by.clone()),
                    AnswerVia::Chief,
                ),
            );
        }
        // A claimed question goes back to work; a question that nobody holds,
        // such as a saga gate, keeps its status and only loses its message.
        let status = if answered && doc.value.claimed_by.is_some() {
            "in_progress"
        } else {
            "unchanged"
        };
        Ok(json!({
            "task": doc.value,
            "to": to,
            "status": status,
        }))
    }

    /// The result of the answer that `key` already sent, for a retry of the
    /// same draft; `None` when the key is new or older than [`ANSWER_TTL`].
    /// The same key with another text is a Conflict.
    async fn answered_with(&self, key: &str, text: &str) -> Result<Option<Value>, ApiError> {
        let Some(doc) = self.store.get::<AnswerRecord>(&keys::answer(key)).await? else {
            return Ok(None);
        };
        if self.now().since(doc.value.at) >= ANSWER_TTL {
            return Ok(None);
        }
        if doc.value.text == text {
            return Ok(Some(doc.value.result));
        }
        Err(ApiError::new(
            ApiErrorCode::Conflict,
            format!(
                "answer key {key} already answered with another text: an edited draft takes a new key"
            ),
        ))
    }

    /// Drops the answer keys that outlived their day, so a key per draft does
    /// not fill the store. At most once an hour.
    pub(crate) async fn prune_answer_keys(&self) -> Result<(), ApiError> {
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_answer_prune
                .is_some_and(|at| now.since(at) < ANSWER_PRUNE_EVERY)
            {
                return Ok(());
            }
            mem.last_answer_prune = Some(now);
        }
        for doc in self.store.list::<AnswerRecord>(keys::ANSWER_PREFIX).await? {
            if now.since(doc.value.at) >= ANSWER_TTL {
                self.store.delete(&doc.key).await?;
            }
        }
        Ok(())
    }

    /// One direct message from a person to an agent. The agent's delivery
    /// wakes it if it is idle, as `borant send` does.
    pub(crate) async fn tell(&self, to: &AgentId, text: &str) -> Result<(), ApiError> {
        self.tell_from(Sender::Human, to, text).await
    }

    /// One direct message from `from` to an agent, for an answer or an
    /// approval. The agent's delivery wakes it if it is idle.
    pub(crate) async fn tell_from(
        &self,
        from: Sender,
        to: &AgentId,
        text: &str,
    ) -> Result<(), ApiError> {
        let envelope = Envelope {
            id: MessageId::new(),
            from,
            to: Target::Agent(to.clone()),
            kind: EnvelopeKind::Direct,
            text: text.to_owned(),
            urgent: false,
            task: None,
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
