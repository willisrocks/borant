//! `[schedules.<name>]` from `hub.toml`: the pass of the hub tick that makes
//! the task of a cron fire, or sends its message (phase 9 spec, "Schedules").
//!
//! The pass runs at most once a minute, outside the hub request lock, and a
//! schedule that fails is a warn: the other schedules and the tick go on.
//!
//! The state of each schedule lives at `sched.<name>`: the fire that the hub
//! last handled, the task that it made for the overlap rule, and the one fire
//! that waits for that task (`buffer_one`). The state key is written before a
//! message goes out, so a hub that restarts does not send it again; a task
//! carries `sched.<name>.<fire>` as its create key, so the idempotent create
//! makes it once across a restart too.

use std::time::Duration;

use borant_core::model::{Envelope, EnvelopeKind, NewTask, Sender, Target, Task, TaskStatus};
use borant_core::proto::ApiError;
use borant_core::rules::schedules::{OverlapAction, Schedule, ScheduleTarget};
use borant_core::{ChannelName, MessageId, TaskId, Timestamp, keys, subjects};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Hub;

/// The pass runs at most this often: each schedule is checked once a minute.
const SCHEDULE_EVERY: Duration = Duration::from_secs(60);

/// The task of one fire, for the overlap rule of the next one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireTask {
    pub channel: ChannelName,
    pub id: TaskId,
}

/// `sched.<name>`: the fires of one schedule that the hub already handled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduleState {
    /// Where the schedule looks for its next fire: the last fire time, or
    /// the moment of the first pass. A schedule with no state starts at its
    /// first tick, so it never fires for the time before the hub saw it.
    #[serde(default)]
    pub cursor: Option<Timestamp>,
    /// The fire that the hub last made, skipped, or buffered, for `borant
    /// schedules`. The first pass of a schedule has none.
    #[serde(default)]
    pub last: Option<Timestamp>,
    /// The task that the last fire made, for `overlap`.
    #[serde(default)]
    pub task: Option<FireTask>,
    /// The one fire that waits for the open task of `task` (`buffer_one`).
    #[serde(default)]
    pub buffered: Option<Timestamp>,
}

impl Hub {
    /// Runs the schedule pass when its minute has come. The pass takes the
    /// memory lock only to check the time, and never fails the tick.
    pub(crate) async fn tick_schedules(&self) -> Result<(), ApiError> {
        if self.cfg.schedules.is_empty() {
            return Ok(());
        }
        let now = self.now();
        {
            let mut mem = self.mem.lock().await;
            if mem
                .last_schedules
                .is_some_and(|last| now.since(last) < SCHEDULE_EVERY)
            {
                return Ok(());
            }
            mem.last_schedules = Some(now);
        }
        for schedule in &self.cfg.schedules {
            if let Err(e) = self.fire_schedule(schedule, now).await {
                tracing::warn!(schedule = %schedule.name, "schedule failed: {e}");
            }
        }
        Ok(())
    }

    /// One schedule, one pass: the buffered fire if it can go, else the one
    /// that is due now.
    async fn fire_schedule(&self, schedule: &Schedule, now: Timestamp) -> Result<(), ApiError> {
        let key = keys::schedule(&schedule.name);
        let stored = self.store.get::<ScheduleState>(&key).await?;
        let revision = stored.as_ref().map(|d| d.revision);
        let mut state = stored.map(|d| d.value).unwrap_or_default();
        let Some(cursor) = state.cursor else {
            // The first pass starts the schedule at now and records it: a
            // fire between this tick and the next one is not lost, and the
            // hub does not fire for the time before it saw the schedule.
            state.cursor = Some(now);
            return self.save(&key, &state, revision).await;
        };
        let next = schedule.next_fire(cursor);
        let due = (now >= next)
            .then(|| schedule.due_fire(cursor, now))
            .flatten();

        match &schedule.target {
            ScheduleTarget::Task {
                channel,
                title,
                body,
            } => {
                let previous_open = if state.buffered.is_some() || due.is_some() {
                    self.previous_open(&state).await?
                } else {
                    false
                };
                // The fire that waited for the previous task goes as soon as
                // that task closes, before the next fire is due.
                if let Some(buffered) = state.buffered
                    && !previous_open
                {
                    let task = self
                        .make_task(schedule, channel, title, body, buffered)
                        .await?;
                    tracing::info!(
                        schedule = %schedule.name,
                        fire = %buffered.0,
                        task = %task.id,
                        "schedule made the waiting task"
                    );
                    state.task = Some(FireTask {
                        channel: channel.clone(),
                        id: task.id,
                    });
                    state.last = Some(buffered);
                    state.cursor = Some(buffered);
                    state.buffered = None;
                    return self.save(&key, &state, revision).await;
                }
                let Some(fire) = due else {
                    if now >= next {
                        self.skip_missed(schedule, &mut state, revision, &key, now, next)
                            .await?;
                    }
                    return Ok(());
                };
                match schedule.overlap_decision(previous_open, state.buffered.is_some()) {
                    OverlapAction::Fire => {
                        let task = self.make_task(schedule, channel, title, body, fire).await?;
                        tracing::info!(
                            schedule = %schedule.name,
                            fire = %fire.0,
                            task = %task.id,
                            "schedule made a task"
                        );
                        state.task = Some(FireTask {
                            channel: channel.clone(),
                            id: task.id,
                        });
                        state.cursor = Some(fire);
                        state.last = Some(fire);
                    }
                    OverlapAction::Buffer => {
                        tracing::info!(
                            schedule = %schedule.name,
                            fire = %fire.0,
                            "schedule buffered a fire"
                        );
                        state.buffered = Some(fire);
                        state.cursor = Some(fire);
                        state.last = Some(fire);
                    }
                    OverlapAction::Skip => {
                        tracing::info!(
                            schedule = %schedule.name,
                            fire = %fire.0,
                            "schedule skipped a fire while its task is open"
                        );
                        state.cursor = Some(fire);
                        state.last = Some(fire);
                    }
                }
                self.save(&key, &state, revision).await
            }
            ScheduleTarget::Message { to, text } => {
                let Some(fire) = due else {
                    if now >= next {
                        self.skip_missed(schedule, &mut state, revision, &key, now, next)
                            .await?;
                    }
                    return Ok(());
                };
                state.cursor = Some(fire);
                state.last = Some(fire);
                // The record goes first: a hub that restarts after it does
                // not send the message twice. A publish that fails is a
                // warn, and the fire is spent.
                self.save(&key, &state, revision).await?;
                let env = Envelope {
                    id: MessageId::new(),
                    from: Sender::Human,
                    to: Target::Agent(to.clone()),
                    kind: EnvelopeKind::Direct,
                    text: schedule.expand_date(text, fire),
                    urgent: false,
                    event: None,
                    sent: now,
                };
                self.bus.publish(&subjects::agent(to), &env).await?;
                tracing::info!(
                    schedule = %schedule.name,
                    fire = %fire.0,
                    agent = %to,
                    "schedule sent a message"
                );
                Ok(())
            }
        }
    }

    /// A fire a period or more old is missed for good: one info line, and the
    /// cursor moves to now so that the next tick looks at a later fire.
    async fn skip_missed(
        &self,
        schedule: &Schedule,
        state: &mut ScheduleState,
        revision: Option<u64>,
        key: &str,
        now: Timestamp,
        next: Timestamp,
    ) -> Result<(), ApiError> {
        tracing::info!(
            schedule = %schedule.name,
            fire = %next.0,
            "skipped a missed fire, older than one period"
        );
        state.cursor = Some(now);
        self.save(key, state, revision).await
    }

    /// True while the task of the last fire is not closed.
    async fn previous_open(&self, state: &ScheduleState) -> Result<bool, ApiError> {
        let Some(t) = &state.task else {
            return Ok(false);
        };
        Ok(self
            .store
            .get::<Task>(&keys::task(&t.channel, &t.id))
            .await?
            .is_some_and(|d| d.value.status != TaskStatus::Closed))
    }

    /// Makes the task of the fire at `fire`: the title and the body with
    /// `{date}`, under the fire's create key, so a restart makes it once.
    async fn make_task(
        &self,
        schedule: &Schedule,
        channel: &ChannelName,
        title: &str,
        body: &str,
        fire: Timestamp,
    ) -> Result<Task, ApiError> {
        let new = NewTask {
            title: schedule.expand_date(title, fire),
            body: schedule.expand_date(body, fire),
            ..NewTask::default()
        };
        let key = format!("sched.{}.{}", schedule.name, rfc3339(fire));
        self.task_create(&Sender::Human, channel, new, Some(key))
            .await
    }

    async fn save(
        &self,
        key: &str,
        state: &ScheduleState,
        revision: Option<u64>,
    ) -> Result<(), ApiError> {
        self.store.put(key, state, revision).await.map(|_| ())
    }

    /// `ToolCall::Schedules`: each schedule with its next and last fire, for
    /// `borant schedules`.
    pub(crate) async fn schedules(&self) -> Result<Value, ApiError> {
        let now = self.now();
        let mut out = Vec::with_capacity(self.cfg.schedules.len());
        for schedule in &self.cfg.schedules {
            let state = self
                .store
                .get::<ScheduleState>(&keys::schedule(&schedule.name))
                .await?
                .map(|d| d.value)
                .unwrap_or_default();
            let last = state.last;
            let next = schedule.next_fire(state.cursor.unwrap_or(now));
            out.push(json!({
                "name": schedule.name,
                "cron": schedule.cron.to_string(),
                "time_zone": schedule.time_zone.name(),
                "overlap": schedule.overlap,
                "target": target_json(&schedule.target),
                "next_fire": rfc3339(next),
                "last_fire": last.map(rfc3339),
            }));
        }
        Ok(Value::Array(out))
    }
}

/// The target of a schedule, as `borant schedules` shows it.
fn target_json(target: &ScheduleTarget) -> Value {
    match target {
        ScheduleTarget::Task {
            channel,
            title,
            body,
        } => json!({
            "kind": "task",
            "channel": channel,
            "title": title,
            "body": body,
        }),
        ScheduleTarget::Message { to, text } => json!({
            "kind": "message",
            "to": to,
            "text": text,
        }),
    }
}

/// A fire time as RFC 3339 UTC, the form of a task create key.
fn rfc3339(at: Timestamp) -> String {
    chrono::DateTime::from_timestamp_millis(at.0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
