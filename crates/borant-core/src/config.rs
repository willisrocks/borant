//! Config records shared with the daemons: `[alerts]`, `[notifiers.<name>]`,
//! `[channels.<name>]`, `[changes]`, and `[schedules.<name>]` of `hub.toml`
//! and `host.toml` (phase 2 spec, "Records and protocol"; phase 6 spec,
//! "Interfaces"; phase 9 spec, "Schedules").

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::BundleName;
use crate::ids::{AgentId, ChannelName};
use crate::rules::schedules::Overlap;

/// A duration of `hub.toml` or `host.toml`: `"10s"`, `"20m"`, `"1h"`,
/// `"1h30m"`, `"2d"`, or a plain number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span(pub Duration);

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl serde::de::Visitor<'_> for V {
            type Value = Span;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a duration such as \"20m\", or a number of seconds")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Span, E> {
                parse_span(v).map(Span).map_err(E::custom)
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs(v)))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs(u64::try_from(v).unwrap_or(0))))
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs_f64(v.max(0.0))))
            }
        }

        d.deserialize_any(V)
    }
}

/// Parses the text of a [`Span`]: `"10s"`, `"20m"`, `"1h"`, `"2d"`, `"1h30m"`,
/// or a number of seconds.
pub fn parse_span(text: &str) -> Result<Duration, String> {
    let mut total = 0.0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        if c == ' ' {
            continue;
        }
        let value: f64 = number
            .parse()
            .map_err(|_| format!("{text:?} is not a duration"))?;
        number.clear();
        let unit = match c {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86_400.0,
            _ => return Err(format!("{text:?}: the unit {c:?} is not s, m, h, or d")),
        };
        total += value * unit;
    }
    if !number.is_empty() {
        let value: f64 = number
            .parse()
            .map_err(|_| format!("{text:?} is not a duration"))?;
        total += value;
    }
    if total <= 0.0 {
        return Err(format!("{text:?} is not a duration"));
    }
    Ok(Duration::from_secs_f64(total))
}

fn five_minutes() -> u64 {
    5
}

fn two_minutes() -> u64 {
    2
}

fn thirty_minutes() -> u64 {
    30
}

fn twenty_minutes() -> u64 {
    crate::rules::hung::DEFAULT_HUNG_MINUTES
}

fn eighty_five() -> u8 {
    85
}

fn yes() -> bool {
    true
}

/// `[alerts]` of `hub.toml` and `host.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsConfig {
    /// The notifier to use, a name of `[notifiers.<name>]` in the same file.
    pub notifier: Option<String>,
    /// Watch the hub from this host and send `hub.unreachable`. The laptop
    /// sets it; an always-on host is watched by the hub instead.
    #[serde(default)]
    pub watch_hub: bool,
    /// Alert when a task enters `needs_human`.
    #[serde(default = "yes")]
    pub needs_human: bool,
    /// A busy session with no event for this long is stalled.
    #[serde(default = "thirty_minutes")]
    pub stall_minutes: u64,
    /// A busy session with no tool call and no text for this long is hung: its
    /// turn will not end by itself. A host that slept this long flags its
    /// sessions the same way, whatever they sent before the sleep.
    #[serde(default = "twenty_minutes")]
    pub hung_minutes: u64,
    /// An always-on host with no heartbeat for this long is offline.
    #[serde(default = "five_minutes")]
    pub host_offline_minutes: u64,
    /// The host with `watch_hub` sends `hub.unreachable` after this long.
    #[serde(default = "two_minutes")]
    pub hub_down_minutes: u64,
    /// A host whose disk is this full gets the `host_disk` alert.
    #[serde(default = "eighty_five")]
    pub disk_warn_pct: u8,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            notifier: None,
            watch_hub: false,
            needs_human: true,
            stall_minutes: thirty_minutes(),
            hung_minutes: twenty_minutes(),
            host_offline_minutes: five_minutes(),
            hub_down_minutes: two_minutes(),
            disk_warn_pct: eighty_five(),
        }
    }
}

fn six_hours() -> Span {
    Span(Duration::from_secs(6 * 60 * 60))
}

fn build_dirs() -> Vec<String> {
    ["target", "node_modules", ".next", "dist", ".turbo"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// `[cleanup]` of `host.toml`: the rebuildable output that the host removes
/// from the work trees of agents that ended, so a host's disk does not fill.
/// A work tree with an uncommitted change or an unpushed commit keeps its
/// output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupConfig {
    /// Whether the janitor removes output. On by default: the output is
    /// rebuildable, and a hidden timer on a server is not the rule.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How long after the end of the agent its output may go, `6h` by
    /// default. The disk guard runs a pass with a zero age when the disk
    /// passes the warn line, for ended agents only.
    #[serde(default = "six_hours")]
    pub min_age: Span,
    /// The directory names to remove below a checkout, ignoring the ones that
    /// git does not ignore.
    #[serde(default = "build_dirs")]
    pub dirs: Vec<String>,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_age: six_hours(),
            dirs: build_dirs(),
        }
    }
}

/// One `[notifiers.<name>]` entry: how to reach a person.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifierConfig {
    /// The adapter, for example `exec`.
    pub adapter: String,
    /// The program of an exec adapter, with its arguments.
    #[serde(default)]
    pub command: Vec<String>,
    /// The env of the program, on top of an empty environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// A credential file name in `$CREDENTIALS_DIRECTORY`.
    #[serde(default)]
    pub credential: Option<String>,
}

/// One `[channels.<name>]` entry of `hub.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    /// How many agents the channel may hold. A spawn past it queues.
    #[serde(default)]
    pub max_agents: Option<u32>,
    /// The bundles that every agent of the channel gets, after the bundles of
    /// its collection and its role.
    #[serde(default)]
    pub bundles: Vec<BundleName>,
}

/// `[changes]` of `hub.toml`: the PR watch. The hub polls the change requests
/// of tasks with a `pr_url` through this adapter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangesConfig {
    /// The adapter, for example `exec`.
    #[serde(default)]
    pub adapter: Option<String>,
    /// The program of an exec adapter.
    #[serde(default)]
    pub program: Option<String>,
    /// The name of the credential that the program reads, a file in the
    /// credentials directory of the hub unit.
    #[serde(default)]
    pub credential: Option<String>,
    /// How often the hub polls, `60` seconds by default.
    #[serde(default = "sixty_secs")]
    pub every_secs: u64,
}

fn sixty_secs() -> u64 {
    60
}

impl Default for ChangesConfig {
    fn default() -> Self {
        Self {
            adapter: None,
            program: None,
            credential: None,
            every_secs: sixty_secs(),
        }
    }
}

/// `[mirror]` of `hub.toml`: the issue mirror. The hub copies sagas, epics,
/// and tasks out to a tracker through this adapter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    /// The adapter, for example `exec`.
    #[serde(default)]
    pub adapter: Option<String>,
    /// The program of an exec adapter.
    #[serde(default)]
    pub program: Option<String>,
    /// The name of the credential that the program reads, a file in the
    /// credentials directory of the hub unit.
    #[serde(default)]
    pub credential: Option<String>,
    /// How often the hub sweeps, `60` seconds by default.
    #[serde(default = "sixty_secs")]
    pub every_secs: u64,
    /// The most tracker calls of one sweep, `50` by default, so a backfill
    /// stays well under the limits of a tracker.
    #[serde(default = "fifty")]
    pub max_writes: usize,
    /// `[mirror.config]`: the settings that the program reads, passed with
    /// every request.
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
}

fn fifty() -> usize {
    50
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            adapter: None,
            program: None,
            credential: None,
            every_secs: sixty_secs(),
            max_writes: fifty(),
            config: BTreeMap::new(),
        }
    }
}

/// One `[schedules.<name>]` entry of `hub.toml`: a cron rule in a time zone
/// that the hub fires, making a task or sending a message.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    /// Five cron fields: minute, hour, day of month, month, weekday.
    pub cron: String,
    /// An IANA time zone name, for example `America/Los_Angeles`.
    pub time_zone: String,
    /// What to do while the task of the previous fire is still open.
    #[serde(default)]
    pub overlap: Overlap,
    /// The task target. Exactly one of `task` and `message` is set.
    #[serde(default)]
    pub task: Option<ScheduleTaskConfig>,
    /// The message target. Exactly one of `task` and `message` is set.
    #[serde(default)]
    pub message: Option<ScheduleMessageConfig>,
}

/// `[schedules.<name>.task]`: the task that a fire makes, from templates.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleTaskConfig {
    /// The channel that holds the task.
    pub channel: ChannelName,
    /// The title, where `{date}` is the fire date in the schedule's zone.
    pub title: String,
    /// The body, where `{date}` is the fire date in the schedule's zone.
    pub body: String,
}

/// `[schedules.<name>.message]`: the message that a fire sends, once.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleMessageConfig {
    /// The agent that receives the message.
    pub to: AgentId,
    /// The text, where `{date}` is the fire date in the schedule's zone.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[cleanup]` of `host.toml`: every key has its default, and a file may
    /// override each one. The record reads the same shapes in TOML and JSON.
    #[test]
    fn cleanup_config_has_defaults_and_reads_the_file() {
        let empty: CleanupConfig = serde_json::from_str("{}").unwrap();
        let default = CleanupConfig::default();
        assert!(default.enabled);
        assert_eq!(default.min_age, Span(Duration::from_secs(6 * 3600)));
        assert_eq!(
            default.dirs,
            ["target", "node_modules", ".next", "dist", ".turbo"]
        );
        assert_eq!(empty, default);

        let set: CleanupConfig = serde_json::from_str(
            r#"{"enabled": false, "min_age": "2h", "dirs": ["out", "build"]}"#,
        )
        .unwrap();
        assert!(!set.enabled);
        assert_eq!(set.min_age, Span(Duration::from_secs(2 * 3600)));
        assert_eq!(set.dirs, ["out", "build"]);

        let bad = serde_json::from_str::<CleanupConfig>(r#"{"mins": "1h"}"#);
        assert!(bad.is_err(), "an unknown key is refused");
    }
}
