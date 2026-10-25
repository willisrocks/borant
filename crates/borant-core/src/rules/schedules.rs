//! `[schedules.<name>]`: a cron rule in a time zone, its next and missed
//! fires, the overlap policy, and `{date}` (phase 9 spec, "Schedules").

use std::str::FromStr;

use chrono::DateTime;
use chrono_tz::Tz;
use croner::Cron;
use croner::errors::CronError;
use croner::parser::{CronParser, Seconds, Year};
use serde::{Deserialize, Serialize};

use crate::config::ScheduleConfig;
use crate::ids::{AgentId, ChannelName};
use crate::time::Timestamp;

/// What a fire makes: a task in a channel, or a message to an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleTarget {
    /// A task from the title and body templates, which the fire expands.
    Task {
        channel: ChannelName,
        title: String,
        body: String,
    },
    /// One message to an agent.
    Message { to: AgentId, text: String },
}

/// `overlap` of `[schedules.<name>]`, `skip` by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    /// No new task while the task of the previous fire is still open.
    #[default]
    Skip,
    /// Hold one fire while the task of the previous fire is open, and make
    /// it once that task closes.
    BufferOne,
}

/// What the hub does with a fire, from [`overlap_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlapAction {
    /// Make the task now. With a buffered fire, this makes that one.
    Fire,
    /// Make nothing and hold no fire.
    Skip,
    /// Hold one fire until the previous task closes.
    Buffer,
}

/// A parsed `[schedules.<name>]` entry, ready for the hub tick.
#[derive(Debug, Clone)]
pub struct Schedule {
    /// The table key, for the state key and the log lines.
    pub name: String,
    /// The five-field cron rule, minute first.
    pub cron: Cron,
    /// The zone that the cron fields are wall clock times in.
    pub time_zone: Tz,
    pub overlap: Overlap,
    pub target: ScheduleTarget,
}

/// Why a `[schedules.<name>]` entry could not parse. Every variant names the
/// schedule, so a hub start fails with the name in the error.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule {name}: invalid cron {cron:?}: {source}")]
    Cron {
        name: String,
        cron: String,
        #[source]
        source: CronError,
    },
    #[error("schedule {name}: unknown time zone {time_zone:?}, want an IANA name")]
    TimeZone { name: String, time_zone: String },
    #[error("schedule {name}: want exactly one of a task or a message target")]
    Target { name: String },
    #[error("schedule {name}: the cron {cron:?} never fires")]
    NeverFires { name: String, cron: String },
}

impl Schedule {
    /// Parses one entry. It proves that the cron fires at least once, so
    /// [`next_fire`] has an answer for every later time.
    pub fn parse(name: &str, config: &ScheduleConfig) -> Result<Self, ScheduleError> {
        let cron = CronParser::builder()
            .seconds(Seconds::Disallowed)
            .year(Year::Disallowed)
            .build()
            .parse(&config.cron)
            .map_err(|source| ScheduleError::Cron {
                name: name.to_owned(),
                cron: config.cron.clone(),
                source,
            })?;
        let time_zone = Tz::from_str(&config.time_zone).map_err(|_| ScheduleError::TimeZone {
            name: name.to_owned(),
            time_zone: config.time_zone.clone(),
        })?;
        let target = match (&config.task, &config.message) {
            (Some(task), None) => ScheduleTarget::Task {
                channel: task.channel.clone(),
                title: task.title.clone(),
                body: task.body.clone(),
            },
            (None, Some(message)) => ScheduleTarget::Message {
                to: message.to.clone(),
                text: message.text.clone(),
            },
            _ => {
                return Err(ScheduleError::Target {
                    name: name.to_owned(),
                });
            }
        };
        // A five-field cron always matches again once it matches once, so
        // this probe from the epoch decides whether it ever fires. The year
        // search limit of the crate bounds the probe.
        if next_fire_opt(&cron, time_zone, Timestamp(0)).is_err() {
            return Err(ScheduleError::NeverFires {
                name: name.to_owned(),
                cron: config.cron.clone(),
            });
        }
        Ok(Self {
            name: name.to_owned(),
            cron,
            time_zone,
            overlap: config.overlap,
            target,
        })
    }

    /// The first fire after `after`.
    pub fn next_fire(&self, after: Timestamp) -> Timestamp {
        next_fire(&self.cron, self.time_zone, after)
    }

    /// The fire to make now, per the missed-fire rule.
    pub fn due_fire(&self, last: Timestamp, now: Timestamp) -> Option<Timestamp> {
        due_fire(last, now, &self.cron, self.time_zone)
    }

    /// What to do with a fire, per the overlap policy.
    pub fn overlap_decision(&self, previous_open: bool, buffered: bool) -> OverlapAction {
        overlap_decision(self.overlap, previous_open, buffered)
    }

    /// `text` with `{date}` replaced by the fire date in the schedule's zone.
    pub fn expand_date(&self, text: &str, fire: Timestamp) -> String {
        expand_date(text, fire, self.time_zone)
    }
}

/// The first fire of `cron` after `after`, as Unix milliseconds.
///
/// The cron fields are wall clock times in `time_zone`, so a fire keeps its
/// wall clock time across a DST change and its UTC time moves. Panics only for
/// a cron that never fires, which [`Schedule::parse`] rejects.
pub fn next_fire(cron: &Cron, time_zone: Tz, after: Timestamp) -> Timestamp {
    next_fire_opt(cron, time_zone, after).expect("a parsed schedule's cron always has a next fire")
}

/// The fire to make now, per the missed-fire rule: the first fire after
/// `last`, when `now` is less than one period past it.
///
/// A fire that is a period or more old is missed for good: the answer is
/// `None`, and the caller advances its last fire to that fire time (or to
/// `now`) so that the next tick considers a later one. A fire that is not due
/// yet is also `None`.
pub fn due_fire(last: Timestamp, now: Timestamp, cron: &Cron, time_zone: Tz) -> Option<Timestamp> {
    let next = next_fire(cron, time_zone, last);
    if now < next {
        return None;
    }
    let period = next_fire(cron, time_zone, next).since(next);
    (now.since(next) < period).then_some(next)
}

/// What to do with a fire while the previous fire's task may still be open.
///
/// `skip` makes no task while the previous one is open. `buffer_one` holds at
/// most one fire while the previous task is open, and makes it once that task
/// closes: a buffered fire with no open task is the fire to make now.
pub fn overlap_decision(policy: Overlap, previous_open: bool, buffered: bool) -> OverlapAction {
    match policy {
        Overlap::Skip => {
            if previous_open {
                OverlapAction::Skip
            } else {
                OverlapAction::Fire
            }
        }
        Overlap::BufferOne => match (previous_open, buffered) {
            (false, _) => OverlapAction::Fire,
            (true, false) => OverlapAction::Buffer,
            (true, true) => OverlapAction::Skip,
        },
    }
}

/// `text` with `{date}` replaced by the date of `fire` in `time_zone`, as
/// `YYYY-MM-DD`.
pub fn expand_date(text: &str, fire: Timestamp, time_zone: Tz) -> String {
    let date = DateTime::from_timestamp_millis(fire.0)
        .unwrap_or(DateTime::UNIX_EPOCH)
        .with_timezone(&time_zone)
        .format("%Y-%m-%d")
        .to_string();
    text.replace("{date}", &date)
}

/// The first fire after `after`, or the crate's error when the cron never
/// fires.
fn next_fire_opt(cron: &Cron, time_zone: Tz, after: Timestamp) -> Result<Timestamp, CronError> {
    let start = DateTime::from_timestamp_millis(after.0)
        .ok_or(CronError::InvalidTime)?
        .with_timezone(&time_zone);
    let next = cron.find_next_occurrence(&start, false)?;
    Ok(Timestamp(next.timestamp_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScheduleMessageConfig, ScheduleTaskConfig};
    use chrono::{TimeZone as _, Utc};

    /// Unix milliseconds of a UTC civil time.
    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Timestamp {
        Timestamp(
            Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
                .unwrap()
                .timestamp_millis(),
        )
    }

    fn cron(text: &str) -> Cron {
        Cron::from_str(text).expect("a test cron")
    }

    fn zone(name: &str) -> Tz {
        Tz::from_str(name).expect("a test zone")
    }

    fn config(cron: &str, time_zone: &str) -> ScheduleConfig {
        ScheduleConfig {
            cron: cron.to_owned(),
            time_zone: time_zone.to_owned(),
            overlap: Overlap::Skip,
            task: Some(ScheduleTaskConfig {
                channel: "reports".parse().unwrap(),
                title: "Morning report for {date}".into(),
                body: "Summarize the last 24 hours.".into(),
            }),
            message: None,
        }
    }

    #[test]
    fn cron_fields_pick_the_minute_hour_day_month_and_weekday() {
        let utc = Tz::UTC;
        // The hour and minute fields: 06:00.
        assert_eq!(
            next_fire(&cron("0 6 * * *"), utc, at(2026, 6, 1, 0, 0)),
            at(2026, 6, 1, 6, 0)
        );
        // The minute field as a step: 10:07 is between 10:00 and 10:15.
        assert_eq!(
            next_fire(&cron("*/15 * * * *"), utc, at(2026, 6, 1, 10, 7)),
            at(2026, 6, 1, 10, 15)
        );
        // Day of month 1 and month 3: the next March 1 after June.
        assert_eq!(
            next_fire(&cron("30 9 1 3 *"), utc, at(2026, 6, 1, 0, 0)),
            at(2027, 3, 1, 9, 30)
        );
        // Weekday 5: Friday 18:00. June 1, 2026 is a Monday.
        assert_eq!(
            next_fire(&cron("0 18 * * 5"), utc, at(2026, 6, 1, 0, 0)),
            at(2026, 6, 5, 18, 0)
        );
    }

    #[test]
    fn a_cron_needs_five_fields() {
        let error = Schedule::parse("nightly-report", &config("0 0 6 * * *", "UTC"))
            .expect_err("six fields");
        assert!(
            matches!(&error, ScheduleError::Cron { name, .. } if name == "nightly-report"),
            "{error}"
        );
        assert!(error.to_string().contains("nightly-report"), "{error}");
    }

    #[test]
    fn a_dst_change_moves_the_utc_time_of_a_fire() {
        let la = zone("America/Los_Angeles");
        let daily = cron("0 6 * * *");
        // 06:00 is 13:00 UTC under PDT and 14:00 UTC under PST. The change
        // is at 02:00 on 2026-11-01.
        assert_eq!(
            next_fire(&daily, la, at(2026, 10, 30, 14, 0)),
            at(2026, 10, 31, 13, 0)
        );
        assert_eq!(
            next_fire(&daily, la, at(2026, 10, 31, 14, 0)),
            at(2026, 11, 1, 14, 0)
        );
    }

    #[test]
    fn a_dst_gap_moves_a_fire_inside_it_to_the_jump() {
        let la = zone("America/Los_Angeles");
        // 02:30 does not exist on 2026-03-08: the clocks jump from 02:00 to
        // 03:00. The crate's fixed-time rule moves the fire by the gap, so it
        // runs at 03:00 PDT, and the schedule does not lose a day.
        let next = next_fire(&cron("30 2 * * *"), la, at(2026, 3, 8, 0, 0));
        assert_eq!(next, at(2026, 3, 8, 10, 0), "{next}");
    }

    #[test]
    fn skip_makes_no_task_while_the_previous_one_is_open() {
        assert_eq!(
            overlap_decision(Overlap::Skip, false, false),
            OverlapAction::Fire
        );
        assert_eq!(
            overlap_decision(Overlap::Skip, true, false),
            OverlapAction::Skip
        );
        // `skip` never holds a fire, so a buffered flag changes nothing.
        assert_eq!(
            overlap_decision(Overlap::Skip, true, true),
            OverlapAction::Skip
        );
    }

    #[test]
    fn buffer_one_holds_at_most_one_fire_and_makes_it_when_the_task_closes() {
        assert_eq!(
            overlap_decision(Overlap::BufferOne, false, false),
            OverlapAction::Fire
        );
        assert_eq!(
            overlap_decision(Overlap::BufferOne, true, false),
            OverlapAction::Buffer
        );
        assert_eq!(
            overlap_decision(Overlap::BufferOne, true, true),
            OverlapAction::Skip
        );
        // The previous task closed: the held fire is the one to make now.
        assert_eq!(
            overlap_decision(Overlap::BufferOne, false, true),
            OverlapAction::Fire
        );
    }

    #[test]
    fn a_missed_fire_inside_one_period_fires_once_and_an_older_one_is_skipped() {
        let utc = Tz::UTC;
        let daily = cron("0 6 * * *");
        let last = at(2026, 6, 1, 6, 0);
        // Not due yet.
        assert_eq!(due_fire(last, at(2026, 6, 2, 5, 0), &daily, utc), None);
        // Already fired.
        assert_eq!(
            due_fire(at(2026, 6, 2, 6, 0), at(2026, 6, 2, 12, 0), &daily, utc),
            None
        );
        // The hub was down at 06:00 and is back 14 hours later: fire it once.
        assert_eq!(
            due_fire(last, at(2026, 6, 2, 20, 0), &daily, utc),
            Some(at(2026, 6, 2, 6, 0))
        );
        // A period or more old: the missed fire is skipped.
        assert_eq!(due_fire(last, at(2026, 6, 3, 8, 0), &daily, utc), None);
    }

    #[test]
    fn the_date_expands_in_the_fire_time_zone() {
        let la = zone("America/Los_Angeles");
        // 2026-10-25T02:00Z is 2026-10-24 19:00 in Los Angeles.
        assert_eq!(
            expand_date("Morning report for {date}", at(2026, 10, 25, 2, 0), la),
            "Morning report for 2026-10-24"
        );
        assert_eq!(
            expand_date("no date", at(2026, 10, 25, 2, 0), la),
            "no date"
        );
    }

    #[test]
    fn parse_names_the_schedule_and_keeps_the_target() {
        let parsed = Schedule::parse(
            "nightly-report",
            &config("0 6 * * *", "America/Los_Angeles"),
        )
        .expect("a schedule");
        assert_eq!(parsed.name, "nightly-report");
        assert_eq!(parsed.time_zone, zone("America/Los_Angeles"));
        assert_eq!(parsed.overlap, Overlap::Skip);
        assert_eq!(
            parsed.target,
            ScheduleTarget::Task {
                channel: "reports".parse().unwrap(),
                title: "Morning report for {date}".into(),
                body: "Summarize the last 24 hours.".into(),
            }
        );

        let mut both = config("0 6 * * *", "UTC");
        both.message = Some(ScheduleMessageConfig {
            to: "w-sched".parse().unwrap(),
            text: "ping".into(),
        });
        let error = Schedule::parse("nightly-report", &both).expect_err("two targets");
        assert!(matches!(error, ScheduleError::Target { .. }), "{error}");
        assert!(error.to_string().contains("nightly-report"), "{error}");

        let mut neither = config("0 6 * * *", "UTC");
        neither.task = None;
        let error = Schedule::parse("nightly-report", &neither).expect_err("no target");
        assert!(matches!(error, ScheduleError::Target { .. }), "{error}");

        let error = Schedule::parse("nightly-report", &config("0 6 * * *", "Not/AZone"))
            .expect_err("a bad zone");
        assert!(matches!(error, ScheduleError::TimeZone { .. }), "{error}");
        assert!(error.to_string().contains("nightly-report"), "{error}");

        let error = Schedule::parse("nightly-report", &config("0 0 30 2 *", "UTC"))
            .expect_err("a cron that never fires");
        assert!(matches!(error, ScheduleError::NeverFires { .. }), "{error}");
        assert!(error.to_string().contains("nightly-report"), "{error}");
    }
}
