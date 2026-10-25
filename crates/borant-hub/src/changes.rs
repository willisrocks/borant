//! The PR watch (spec phase 6, "PR watch"): the hub polls the change request
//! of every open or in-progress task that carries a `pr_url`, and tells the
//! worker and the owner what changed. It runs on its own task, outside the
//! request lock, so a slow forge never delays a lease renewal.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Channel, Task, TaskStatus};
use borant_core::ports::ChangeRequests;
use borant_core::ports::PortErrorKind;
use borant_core::ports::changes::{ChangeState, ChangeStatus, CheckState, ReviewState};
use borant_core::proto::ApiError;
use borant_core::{AgentId, TaskId, keys};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::Hub;

/// The property of a task that names its change request.
pub const PR_URL: &str = "pr_url";

/// How often the hub polls the change requests, and the adapter that answers.
#[derive(Clone)]
pub struct ChangeWatch {
    pub changes: Arc<dyn ChangeRequests>,
    /// The poll interval of `[changes].every_secs`.
    pub every: Duration,
}

impl fmt::Debug for ChangeWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangeWatch")
            .field("every", &self.every)
            .finish()
    }
}

/// The key of the last state of a change request, as the spec names it.
fn key(channel: &borant_core::ChannelName, task: &TaskId) -> String {
    format!("pr.{channel}.{task}")
}

/// What the watch remembers about one change request. The flags are per head,
/// so one head gives one message to the worker and one to the owner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Watched {
    /// The status that the last poll read, `None` before the first one.
    #[serde(default)]
    last: Option<ChangeStatus>,
    /// The worker heard that the checks of this head failed, or that a change
    /// was requested on it.
    #[serde(default)]
    told_worker: bool,
    /// The owner heard that the checks of this head pass.
    #[serde(default)]
    told_green: bool,
    /// The owner heard that the change merged or closed.
    #[serde(default)]
    told_done: bool,
}

impl Hub {
    /// The PR watch loop: one pass every `every`, on its own task.
    pub async fn change_loop(self: Arc<Self>, shutdown: CancellationToken) {
        let Some(watch) = self.cfg.changes.clone() else {
            return;
        };
        let mut every = tokio::time::interval(watch.every);
        // A slow pass must not line up ticks: the next one waits its turn.
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = every.tick() => {
                    match self.poll_changes(&watch).await {
                        Ok(0) => {}
                        Ok(polled) => tracing::debug!(polled, "the PR watch polled"),
                        Err(e) => tracing::warn!("the PR watch failed: {}", e.message),
                    }
                }
            }
        }
    }

    /// One pass: reads the change of each task with a `pr_url` and tells what
    /// changed. Public for tests.
    pub async fn poll_changes(&self, watch: &ChangeWatch) -> Result<usize, ApiError> {
        let mut polled = 0;
        for channel in self.open_channels().await? {
            for task in self.tasks(&channel.name).await? {
                let pinned = task.properties.get(PR_URL).and_then(|v| v.as_str());
                let Some(url) = pinned.map(str::to_owned) else {
                    continue;
                };
                if !matches!(task.status, TaskStatus::Open | TaskStatus::InProgress) {
                    continue;
                }
                let status = match watch.changes.status(&url).await {
                    Ok(status) => status,
                    Err(e) if e.is(PortErrorKind::NotFound) => {
                        tracing::debug!(task = %task.id, "the change request is gone");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(
                            task = %task.id,
                            "cannot read the change request: {}",
                            e.message
                        );
                        continue;
                    }
                };
                polled += 1;
                self.note_change(&channel, &task, &url, &status).await?;
            }
        }
        Ok(polled)
    }

    /// Compares one status with the last one, tells what changed, and writes
    /// the new state.
    async fn note_change(
        &self,
        channel: &Channel,
        task: &Task,
        url: &str,
        status: &ChangeStatus,
    ) -> Result<(), ApiError> {
        let key = key(&channel.name, &task.id);
        let doc = self.store.get::<Watched>(&key).await?;
        let mut watched = doc.as_ref().map(|d| d.value.clone()).unwrap_or_default();
        // A new head starts its own round of messages.
        if watched.last.as_ref().is_none_or(|l| l.head != status.head) {
            watched.told_worker = false;
            watched.told_green = false;
        }
        let changed = watched.last.as_ref() != Some(status);
        let head = &status.head;
        // The worker hears about a failure of its head, once per head. Only
        // fails and requested changes count: no check reported is not a fail,
        // and a pending check is not one either.
        let failed = match &status.checks {
            CheckState::Fail { failed } => Some(failed.clone()),
            _ => None,
        };
        let requested = status.review == ReviewState::ChangesRequested;
        if !watched.told_worker && (failed.is_some() || requested) {
            if let Some(worker) = &task.claimed_by {
                let what = match (&failed, requested) {
                    (Some(f), true) => {
                        format!(
                            "checks failed ({}) and changes were requested",
                            f.join(", ")
                        )
                    }
                    (Some(f), false) => format!("checks failed ({})", f.join(", ")),
                    (None, _) => "changes were requested".to_owned(),
                };
                self.say(
                    worker,
                    &format!("PR {url} at {}: {what}. Fix it, then push.", short(head)),
                )
                .await;
            }
            watched.told_worker = true;
        }
        // The owner hears that the checks pass, once per head.
        let pass = matches!(status.checks, CheckState::Pass { .. });
        if !watched.told_green && pass && status.state == ChangeState::Open {
            self.say(
                &channel.owner,
                &format!("PR {url} green at {}", short(head)),
            )
            .await;
            watched.told_green = true;
        }
        // The owner hears about a merge or a close, once.
        if !watched.told_done && matches!(status.state, ChangeState::Merged | ChangeState::Closed) {
            let what = match status.state {
                ChangeState::Merged => "merged",
                _ => "closed without a merge",
            };
            self.say(
                &channel.owner,
                &format!("PR {url} {what} at {}", short(head)),
            )
            .await;
            watched.told_done = true;
        }
        watched.last = Some(status.clone());
        self.store.upsert(&key, &watched).await?;
        if changed {
            let event = self
                .event(
                    self.now(),
                    Severity::Info,
                    SwarmEventBody::ChangeUpdated {
                        state: status.state,
                        head: status.head.clone(),
                        checks: status.checks.clone(),
                        review: status.review,
                    },
                )
                .about_channel(channel.name.clone())
                .about_task(task.id);
            self.publish(event).await;
        }
        Ok(())
    }

    /// One direct message from the hub; a failed one is a warn, not a failed
    /// pass.
    async fn say(&self, agent: &AgentId, text: &str) {
        if let Err(e) = self.tell(agent, text).await {
            tracing::warn!(%agent, "cannot deliver a PR watch message: {}", e.message);
        }
    }

    /// The channels of the swarm, oldest key shape first: `chan.<name>` and
    /// not the member or task keys under it.
    async fn open_channels(&self) -> Result<Vec<Channel>, ApiError> {
        let mut out = Vec::new();
        for doc in self
            .store
            .list::<serde_json::Value>(keys::CHANNEL_PREFIX)
            .await?
        {
            if doc.key.matches('.').count() != 1 {
                continue;
            }
            if let Ok(channel) = serde_json::from_value::<Channel>(doc.value) {
                out.push(channel);
            }
        }
        Ok(out)
    }
}

/// The short form of a head SHA, for a message that a person reads.
fn short(head: &str) -> &str {
    head.get(..12).unwrap_or(head)
}
