//! Config records shared with the daemons: `[alerts]`, `[notifiers.<name>]`,
//! `[channels.<name>]`, `[changes]`, and `[schedules.<name>]` of `hub.toml`
//! and `host.toml` (phase 2 spec, "Records and protocol"; phase 6 spec,
//! "Interfaces"; phase 9 spec, "Schedules").

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ids::{AgentId, ChannelName};
use crate::rules::schedules::Overlap;

fn five_minutes() -> u64 {
    5
}

fn two_minutes() -> u64 {
    2
}

fn thirty_minutes() -> u64 {
    30
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
            host_offline_minutes: five_minutes(),
            hub_down_minutes: two_minutes(),
            disk_warn_pct: eighty_five(),
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
