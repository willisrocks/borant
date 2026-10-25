//! The hub alert pass (phase 2 spec, "Alerts"): it gathers what the rules
//! need, evaluates them, and keeps the open alerts in KV at `alert.<key>`.
//!
//! The pass runs from the tick, at most every [`ALERT_EVERY`]. Notices go
//! through the hub notifier without holding `mem`, and every fire and resolve
//! publishes an `alert.fired` or `alert.resolved` event.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, Task, TaskStatus};
use borant_core::ports::Notifier;
use borant_core::proto::ApiError;
use borant_core::rules::alerts::{self, AgentAlert, AlertAction, AlertView, Refusal, TaskAlert};
use borant_core::{AgentId, ChannelName, Timestamp, keys};
use serde_json::json;

use crate::Hub;

/// The alert pass runs at most this often.
pub(crate) const ALERT_EVERY: Duration = Duration::from_secs(30);

/// The KV prefix of the open alerts. The rest of the key is the alert key,
/// `<rule>:<subject>`.
const ALERT_PREFIX: &str = "alert.";

impl Hub {
    /// Sets the notifier of the alert pass. Without one, the hub still keeps
    /// the open alerts in KV; it only skips the notice.
    pub fn with_notifier(mut self, notifier: Arc<dyn Notifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Runs the alert pass when its time has come.
    pub(crate) async fn tick_alerts(&self) -> Result<(), ApiError> {
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_alerts
                .is_some_and(|last| now.since(last) < ALERT_EVERY)
            {
                return Ok(());
            }
            mem.last_alerts = Some(now);
        }
        if let Err(e) = self.alert_pass(now).await {
            tracing::warn!("alert pass failed: {e}");
        }
        Ok(())
    }

    async fn alert_pass(&self, now: Timestamp) -> Result<(), ApiError> {
        let (view, tasks) = self.alert_view(now).await?;
        let open: BTreeSet<String> = self
            .store
            .0
            .list(ALERT_PREFIX)
            .await?
            .into_iter()
            .map(|e| e.key.trim_start_matches(ALERT_PREFIX).to_owned())
            .collect();

        for action in alerts::evaluate(&view, &open, &self.cfg.alerts, now) {
            match action {
                AlertAction::Fire { key, notice } => {
                    // A failed notice leaves the alert closed, so the next
                    // pass sends it again.
                    if let Some(notifier) = &self.notifier
                        && let Err(e) = notifier.notify(&notice).await
                    {
                        tracing::warn!(alert = %key, "notice failed: {e}");
                        continue;
                    }
                    let rule = rule_of(&key);
                    let subject = subject_of(&key);
                    self.store
                        .upsert(
                            &format!("{ALERT_PREFIX}{key}"),
                            &json!({
                                "rule": rule,
                                "subject": subject,
                                "at": now,
                                "notice": notice,
                            }),
                        )
                        .await?;
                    let event = self.event(
                        now,
                        Severity::Warn,
                        SwarmEventBody::AlertFired {
                            rule: rule.to_owned(),
                            subject: subject.to_owned(),
                        },
                    );
                    self.publish(event).await;
                    if rule == alerts::AGENT_STALLED {
                        self.mark_claim_stuck(&tasks, subject, now).await?;
                    }
                }
                AlertAction::Resolve { key } => {
                    self.store.delete(&format!("{ALERT_PREFIX}{key}")).await?;
                    let event = self.event(
                        now,
                        Severity::Info,
                        SwarmEventBody::AlertResolved {
                            rule: rule_of(&key).to_owned(),
                            subject: subject_of(&key).to_owned(),
                        },
                    );
                    self.publish(event).await;
                }
            }
        }
        Ok(())
    }

    /// The rules' inputs: the live view plus what only the hub keeps. The
    /// task scan also serves `agent_stalled`, which marks the claimed task of
    /// a stalled agent as stuck.
    async fn alert_view(
        &self,
        now: Timestamp,
    ) -> Result<(AlertView, Vec<(ChannelName, Task)>), ApiError> {
        let swarm = self.view().await?;
        let agents: Vec<AgentAlert> = self
            .store
            .list::<Agent>(keys::AGENT_PREFIX)
            .await?
            .into_iter()
            .map(|d| AgentAlert {
                agent: d.value.id,
                pinned: d.value.pinned,
            })
            .collect();
        let tasks = self.all_tasks().await?;
        let needs_human = tasks
            .iter()
            .filter(|(_, task)| task.status == TaskStatus::NeedsHuman)
            .map(|(channel, task)| TaskAlert {
                id: task.id,
                channel: channel.clone(),
                title: task.title.clone(),
                attention_msg: task.attention_msg.clone(),
            })
            .collect();
        let restart_grace = {
            let mem = self.mem.lock().await;
            mem.started
                .is_some_and(|started| now.since(started) < self.cfg.lease.lease)
        };
        let refused: Vec<Refusal> = {
            let mem = self.mem.lock().await;
            mem.elastic_refused.values().cloned().collect()
        };
        let view = AlertView {
            swarm,
            agents,
            needs_human,
            limited_bundles: self.limited_bundles(now).await?,
            budget_blocked: self.budget_blocked().await?,
            refused,
            restart_grace,
        };
        Ok((view, tasks))
    }

    /// Every task of every channel, with its channel. The inbox lists from
    /// this too, so a person and an alert pass see the same tasks.
    pub(crate) async fn all_tasks(&self) -> Result<Vec<(ChannelName, Task)>, ApiError> {
        let mut tasks = Vec::new();
        for entry in self.store.0.list(keys::CHANNEL_PREFIX).await? {
            let Some(rest) = entry.key.strip_prefix(keys::CHANNEL_PREFIX) else {
                continue;
            };
            let Some((channel, _)) = rest.split_once(".task.") else {
                continue;
            };
            let Ok(channel) = channel.parse::<ChannelName>() else {
                continue;
            };
            match serde_json::from_slice::<Task>(&entry.value) {
                Ok(task) => tasks.push((channel, task)),
                Err(e) => tracing::warn!(key = %entry.key, "skip bad task: {e}"),
            }
        }
        Ok(tasks)
    }

    /// Sets the open task that the stalled agent claimed to `stuck`.
    async fn mark_claim_stuck(
        &self,
        tasks: &[(ChannelName, Task)],
        agent: &str,
        now: Timestamp,
    ) -> Result<(), ApiError> {
        let Ok(agent) = agent.parse::<AgentId>() else {
            return Ok(());
        };
        let Some((channel, task)) = tasks
            .iter()
            .find(|(_, task)| task.claimed_by.as_ref() == Some(&agent))
        else {
            return Ok(());
        };
        if task.status == TaskStatus::Closed || task.status == TaskStatus::Stuck {
            return Ok(());
        }
        let key = keys::task(channel, &task.id);
        let Some(doc) = self.store.get::<Task>(&key).await? else {
            return Ok(());
        };
        let mut next = doc.value;
        next.status = TaskStatus::Stuck;
        next.updated = now;
        self.store.put(&key, &next, Some(doc.revision)).await?;
        tracing::warn!(agent = %agent, task = %next.id, "the stalled agent's claim is stuck");
        Ok(())
    }
}

/// The rule of an alert key, `<rule>:<subject>`.
fn rule_of(key: &str) -> &str {
    key.split_once(':').map(|(rule, _)| rule).unwrap_or(key)
}

/// The subject of an alert key, `<rule>:<subject>`.
fn subject_of(key: &str) -> &str {
    key.split_once(':')
        .map(|(_, subject)| subject)
        .unwrap_or(key)
}
