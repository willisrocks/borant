//! The schedule pass of the hub tick (phase 9 spec, "Schedules"): one task per
//! fire across a restart, `skip` and `buffer_one`, a message target that sends
//! once, and a missed fire inside one period.

mod common;

use std::sync::Arc;
use std::time::Duration;

use borant_core::Clock;
use borant_core::config::{ScheduleConfig, ScheduleMessageConfig, ScheduleTaskConfig};
use borant_core::model::{ChannelKind, CloseReason, Envelope, Harness, Task};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{Registered, ToolCall};
use borant_core::rules::schedules::{Overlap, Schedule};
use borant_core::{ChannelName, TaskId, Timestamp, keys};
use borant_hub::{Hub, HubConfig, ScheduleState};
use common::Fx;

/// One day of Unix milliseconds: the cron below fires at each midnight.
const DAY: i64 = 86_400_000;

/// The channel of the task schedules.
fn reports() -> ChannelName {
    "reports".parse().unwrap()
}

/// A daily task schedule at UTC midnight.
fn task_schedule(overlap: Overlap) -> Schedule {
    Schedule::parse(
        "nightly-report",
        &ScheduleConfig {
            cron: "0 0 * * *".into(),
            time_zone: "UTC".into(),
            overlap,
            task: Some(ScheduleTaskConfig {
                channel: reports(),
                title: "Report for {date}".into(),
                body: "Summarize the last day.".into(),
            }),
            message: None,
        },
    )
    .expect("the test schedule")
}

/// A daily schedule that sends a message to `to`.
fn message_schedule(to: &str) -> Schedule {
    Schedule::parse(
        "ping",
        &ScheduleConfig {
            cron: "0 0 * * *".into(),
            time_zone: "UTC".into(),
            overlap: Overlap::Skip,
            task: None,
            message: Some(ScheduleMessageConfig {
                to: to.parse().unwrap(),
                text: "ping for {date}".into(),
            }),
        },
    )
    .expect("the test schedule")
}

fn cfg(schedule: &Schedule) -> HubConfig {
    HubConfig {
        schedules: vec![schedule.clone()],
        ..HubConfig::default()
    }
}

/// A hub with the schedule and the channel of its task, and a channel owner.
async fn fixture(schedule: &Schedule) -> Fx {
    let fx = Fx::with(cfg(schedule));
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: reports(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
        },
    )
    .await
    .unwrap();
    fx
}

/// Ticks `hub` at `at`, moving the clock of the fixture first.
async fn tick_at(hub: &Hub, fx: &Fx, at: Timestamp) {
    let now = fx.clock.now();
    if at > now {
        fx.clock.advance(at.since(now));
    }
    hub.tick().await.expect("the tick");
}

/// A second hub on the store and the clock of `fx`: a hub that restarted,
/// with its memory empty.
fn restarted(fx: &Fx, schedule: &Schedule) -> Arc<Hub> {
    Arc::new(Hub::new(
        Arc::new(fx.state.clone()),
        Arc::new(fx.bus.clone()),
        Arc::new(fx.sessions.clone()),
        fx.clock.clone(),
        cfg(schedule),
    ))
}

async fn state(fx: &Fx, name: &str) -> ScheduleState {
    let entry = fx
        .state
        .get(&keys::schedule(name))
        .await
        .unwrap()
        .expect("the schedule state");
    serde_json::from_slice(&entry.value).unwrap()
}

async fn tasks(fx: &Fx) -> Vec<Task> {
    let v = fx
        .call(
            None,
            ToolCall::TaskList {
                channel: reports(),
                ready_only: false,
            },
        )
        .await
        .expect("the task list");
    serde_json::from_value(v).unwrap()
}

async fn close(fx: &Fx, id: TaskId) {
    fx.call(
        None,
        ToolCall::TaskClose {
            channel: reports(),
            id,
            reason: CloseReason::Done,
            message: "done".into(),
        },
    )
    .await
    .expect("the close");
}

/// The direct messages of an agent, acked.
async fn messages(fx: &Fx, agent: &Registered) -> Vec<Envelope> {
    let mut out = Vec::new();
    for d in fx
        .bus
        .pull(&agent.agent, 10, Duration::from_millis(50))
        .await
        .unwrap()
    {
        out.push(d.envelope.clone());
        d.acker.ack().await.unwrap();
    }
    out
}

/// One task per fire, and a hub that restarts on the same store makes no
/// second one.
#[tokio::test]
async fn schedules_a_fire_makes_one_task_across_a_restart() {
    let schedule = task_schedule(Overlap::Skip);
    let fx = fixture(&schedule).await;

    // The first pass starts the schedule at now: it does not fire for the
    // time before the hub saw it.
    fx.hub.tick().await.unwrap();
    assert!(tasks(&fx).await.is_empty());
    let started = state(&fx, "nightly-report").await;
    assert_eq!(started.cursor, Some(fx.clock.now()));
    assert_eq!(started.last, None, "the start is not a fire");

    tick_at(&fx.hub, &fx, Timestamp(DAY + 30_000)).await;
    let made = tasks(&fx).await;
    assert_eq!(made.len(), 1);
    assert_eq!(
        made[0].create_key.as_deref(),
        Some("sched.nightly-report.1970-01-02T00:00:00Z")
    );
    assert_eq!(made[0].title, "Report for 1970-01-02");
    assert_eq!(made[0].body, "Summarize the last day.");
    assert_eq!(
        state(&fx, "nightly-report").await.last,
        Some(Timestamp(DAY))
    );

    // A restarted hub at the same store, its memory empty, makes no second
    // task for the fire.
    let restarted = restarted(&fx, &schedule);
    tick_at(&restarted, &fx, Timestamp(DAY + 90_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1, "one task per fire");
}

/// `skip`: no task while the task of the previous fire is open, and the
/// skipped fire is not made later.
#[tokio::test]
async fn schedules_skip_makes_no_task_while_the_previous_one_is_open() {
    let schedule = task_schedule(Overlap::Skip);
    let fx = fixture(&schedule).await;
    fx.hub.tick().await.unwrap();
    tick_at(&fx.hub, &fx, Timestamp(DAY + 30_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1);

    tick_at(&fx.hub, &fx, Timestamp(2 * DAY + 30_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1, "the open task holds the slot");
    let kept = state(&fx, "nightly-report").await;
    assert_eq!(kept.last, Some(Timestamp(2 * DAY)));
    assert_eq!(kept.buffered, None, "skip holds no fire");

    let first = tasks(&fx).await.remove(0);
    close(&fx, first.id).await;
    tick_at(&fx.hub, &fx, Timestamp(3 * DAY + 30_000)).await;
    let all = tasks(&fx).await;
    assert_eq!(all.len(), 2);
    let next = all.iter().find(|t| t.id != first.id).expect("the new task");
    assert_eq!(next.title, "Report for 1970-01-04");
    assert_eq!(
        next.create_key.as_deref(),
        Some("sched.nightly-report.1970-01-04T00:00:00Z"),
        "the skipped day is not made later"
    );
}

/// `buffer_one`: at most one fire waits, and it is made when the previous
/// task closes.
#[tokio::test]
async fn schedules_buffer_one_makes_the_waiting_fire_when_the_task_closes() {
    let schedule = task_schedule(Overlap::BufferOne);
    let fx = fixture(&schedule).await;
    fx.hub.tick().await.unwrap();
    tick_at(&fx.hub, &fx, Timestamp(DAY + 30_000)).await;
    tick_at(&fx.hub, &fx, Timestamp(2 * DAY + 30_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1, "one task waits for the open one");
    assert_eq!(
        state(&fx, "nightly-report").await.buffered,
        Some(Timestamp(2 * DAY))
    );

    // The third day is due too, and the buffer holds one fire already.
    tick_at(&fx.hub, &fx, Timestamp(3 * DAY + 30_000)).await;
    let held = state(&fx, "nightly-report").await;
    assert_eq!(held.buffered, Some(Timestamp(2 * DAY)), "one fire waits");
    assert_eq!(tasks(&fx).await.len(), 1);

    let first = tasks(&fx).await.remove(0);
    close(&fx, first.id).await;
    tick_at(&fx.hub, &fx, Timestamp(3 * DAY + 90_000)).await;
    let all = tasks(&fx).await;
    assert_eq!(all.len(), 2);
    let waiting = all
        .iter()
        .find(|t| t.id != first.id)
        .expect("the waiting task");
    assert_eq!(waiting.title, "Report for 1970-01-03");
    assert_eq!(
        waiting.create_key.as_deref(),
        Some("sched.nightly-report.1970-01-03T00:00:00Z")
    );
    assert_eq!(state(&fx, "nightly-report").await.buffered, None);
}

/// A message target records the fire before it sends, so a restart sends one
/// message.
#[tokio::test]
async fn schedules_a_message_target_sends_once_across_a_restart() {
    let schedule = message_schedule("a1");
    let fx = Fx::with(cfg(&schedule));
    fx.host("h1", &[Harness::Fake]).await;
    let a1 = fx.register("h1", Some("a1"), None).await;

    fx.hub.tick().await.unwrap();
    tick_at(&fx.hub, &fx, Timestamp(DAY + 30_000)).await;
    let sent = messages(&fx, &a1).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].text, "ping for 1970-01-02");
    assert_eq!(state(&fx, "ping").await.last, Some(Timestamp(DAY)));

    let restarted = restarted(&fx, &schedule);
    tick_at(&restarted, &fx, Timestamp(DAY + 90_000)).await;
    assert!(
        messages(&fx, &a1).await.is_empty(),
        "no second message for the same fire"
    );
}

/// A fire that the hub missed by less than one period fires once at the next
/// tick, with the date of the fire and not of the tick.
#[tokio::test]
async fn schedules_a_missed_fire_inside_one_period_fires_once() {
    let schedule = task_schedule(Overlap::Skip);
    let fx = fixture(&schedule).await;
    fx.hub.tick().await.unwrap();

    // The hub was down over the midnight fire, and comes back at noon.
    tick_at(&fx.hub, &fx, Timestamp(DAY + 12 * 3_600_000)).await;
    let made = tasks(&fx).await;
    assert_eq!(made.len(), 1);
    assert_eq!(
        made[0].create_key.as_deref(),
        Some("sched.nightly-report.1970-01-02T00:00:00Z"),
        "the fire time, not the tick time"
    );
    assert_eq!(made[0].title, "Report for 1970-01-02");
    // The next pass sees no fire: the missed one was made once.
    tick_at(&fx.hub, &fx, Timestamp(DAY + 12 * 3_600_000 + 90_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1);

    // A fire more than a period old is skipped for good.
    tick_at(&fx.hub, &fx, Timestamp(DAY + 2 * DAY + 12 * 3_600_000)).await;
    assert_eq!(tasks(&fx).await.len(), 1, "the old fire is skipped");
}

/// `ToolCall::Schedules` lists each schedule with its next and last fire.
#[tokio::test]
async fn schedules_the_call_lists_each_schedule_with_its_fires() {
    let schedule = task_schedule(Overlap::Skip);
    let fx = fixture(&schedule).await;
    fx.hub.tick().await.unwrap();

    let before = fx.call(None, ToolCall::Schedules).await.unwrap();
    let list = before.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "nightly-report");
    assert_eq!(list[0]["cron"], "0 0 * * *");
    assert_eq!(list[0]["time_zone"], "UTC");
    assert_eq!(list[0]["overlap"], "skip");
    assert_eq!(list[0]["target"]["kind"], "task");
    assert_eq!(list[0]["target"]["channel"], "reports");
    assert_eq!(list[0]["next_fire"], "1970-01-02T00:00:00Z");
    assert!(list[0]["last_fire"].is_null());

    tick_at(&fx.hub, &fx, Timestamp(DAY + 30_000)).await;
    let after = fx.call(None, ToolCall::Schedules).await.unwrap();
    assert_eq!(after[0]["last_fire"], "1970-01-02T00:00:00Z");
    assert_eq!(after[0]["next_fire"], "1970-01-03T00:00:00Z");
}
