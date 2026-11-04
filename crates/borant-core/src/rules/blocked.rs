//! The blocked-work alarm (blocked-work alarm spec, "Blocked spawns" and
//! "Stalled cards").
//!
//! A queued spawn is *blocked* when it has waited [`BLOCKED_AFTER`] and no
//! host refuses it only for capacity: a full host, a full disk, low memory,
//! or a channel at its cap clear with a wait or a drain. Every other refusal
//! names something that a person must do, so the hub tells the person, the
//! spawner, and the chief once on the first sighting and then at most every
//! [`REPEAT_EVERY`].
//!
//! A claimed task is *stalled* when nothing changed it for
//! `[alerts] stall_minutes` and its claimer can no longer work it: its spawn
//! is queued and blocked, or its stopped session was fenced. A dead claimer
//! is not a stalled card: the hub ends it through the liveness sweep, and the
//! claim timeout returns its claim to open with one attempt counted.

use std::fmt;
use std::time::Duration;

use crate::ids::{AgentId, BundleName, ChannelName, Tag};
use crate::model::{Sender, SpawnRecord, SpawnState, Task};
use crate::time::Timestamp;

/// A spawn that waits this long is blocked, when capacity does not explain
/// the wait.
pub const BLOCKED_AFTER: Duration = Duration::from_secs(15 * 60);

/// How often the hub tells about a spawn that stays blocked.
pub const REPEAT_EVERY: Duration = Duration::from_secs(4 * 60 * 60);

/// The refusal text that a spawn with no refusal at all carries: no host is
/// registered or online, so no host named one.
pub const NO_HOST: &str = "no host is online";

/// Why a spawn is blocked: the refusal that a person can act on, classified
/// by the text that [`super::schedule::Reject`] prints. The order of the
/// variants is the order in which the alarm names the cause: a refusal that
/// only a configuration change clears comes before a host that is merely
/// offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    MissingBundle(BundleName),
    NoMemberBundle(BundleName),
    MissingTag(Tag),
    NoHarness,
    NoSandbox,
    NoRepo,
    RecentlyFailed,
    Offline,
    Draining,
    /// A refusal text that this version does not know, for example one from a
    /// newer hub.
    Other(String),
}

impl Cause {
    /// How short the line is that a person must act on: the lowest rank wins.
    fn rank(&self) -> u8 {
        match self {
            Self::MissingBundle(_) => 0,
            Self::NoMemberBundle(_) => 1,
            Self::MissingTag(_) => 2,
            Self::NoHarness => 3,
            Self::NoSandbox => 4,
            Self::NoRepo => 5,
            Self::RecentlyFailed => 6,
            Self::Offline => 7,
            Self::Draining => 8,
            Self::Other(_) => 9,
        }
    }

    /// The one action that unblocks the spawn (blocked-work alarm spec, "The
    /// action comes from the reason").
    pub fn action(&self, agent: &AgentId) -> String {
        match self {
            Self::MissingBundle(bundle) => format!(
                "add bundle {bundle} to a host that the spawn may use, or spawn without {bundle}"
            ),
            Self::NoMemberBundle(bundle) => format!(
                "give a host a member of bundle {bundle} for the launch harness, or spawn without {bundle}"
            ),
            Self::MissingTag(tag) => format!("tag a host with {tag}, or drop {tag} from the role"),
            Self::NoHarness => "install the harness on a host".to_owned(),
            Self::NoSandbox => "install the sandbox on a host".to_owned(),
            Self::NoRepo => "fix the access of a host to the repository".to_owned(),
            Self::RecentlyFailed => {
                format!("`borant resume {agent}` after fixing the launch error")
            }
            Self::Offline => "bring a host online".to_owned(),
            Self::Draining => "undrain a host".to_owned(),
            Self::Other(_) => "check the hosts for the refusal".to_owned(),
        }
    }
}

/// A spawn that waits on something that a person must fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// How long the spawn has waited in the queue.
    pub waited: Duration,
    /// The refusal that a person can act on.
    pub cause: Cause,
    /// That refusal, as `borant ps --queued` prints it.
    pub reason: String,
}

impl Blocked {
    /// What a person does about it.
    pub fn action(&self, agent: &AgentId) -> String {
        self.cause.action(agent)
    }

    /// The alarm text: the spawn, its wait, the refusal, and the one action.
    pub fn text(&self, agent: &AgentId) -> String {
        format!(
            "spawn {agent} blocked {}: {}. Unblock: {}",
            waited_label(self.waited),
            self.reason,
            self.action(agent)
        )
    }
}

/// True when a wait or a drain alone clears the refusal: a host at its
/// limit, a channel at its cap, a host low on memory, or a full disk. The
/// texts are the ones that [`super::schedule::Reject`] prints, so the alarm
/// reads the same text that `borant ps --queued` shows.
pub fn is_capacity(reason: &str) -> bool {
    reason.starts_with("full (")
        || reason.starts_with("channel full (")
        || reason.starts_with("low memory (")
        || reason.starts_with("disk full (")
}

/// The cause of one refusal text.
pub fn cause_of(reason: &str) -> Cause {
    let named = |prefix: &str| reason.strip_prefix(prefix);
    if let Some(bundle) = named("missing bundle ").and_then(|b| b.parse().ok()) {
        return Cause::MissingBundle(bundle);
    }
    if let Some(bundle) = named("no member bundle ").and_then(|b| b.parse().ok()) {
        return Cause::NoMemberBundle(bundle);
    }
    if let Some(tag) = named("missing tag ").and_then(|t| t.parse().ok()) {
        return Cause::MissingTag(tag);
    }
    match reason {
        "no harness" => Cause::NoHarness,
        "no sandbox" => Cause::NoSandbox,
        "cannot reach the repo" => Cause::NoRepo,
        "failed recently" => Cause::RecentlyFailed,
        "offline" => Cause::Offline,
        "draining" => Cause::Draining,
        other => Cause::Other(other.to_owned()),
    }
}

/// The blocked spawn that `record` is at `now`, or `None` while capacity
/// explains the wait.
pub fn blocked(record: &SpawnRecord, now: Timestamp) -> Option<Blocked> {
    if record.state != SpawnState::Queued {
        return None;
    }
    let waited = now.since(record.queued_at);
    if waited < BLOCKED_AFTER {
        return None;
    }
    // The channel cap at enqueue, then the first failed rule of each host at
    // the last placement try.
    let reasons = record
        .queue_reason
        .as_deref()
        .into_iter()
        .chain(record.last_reject.iter().map(|(_, why)| why.as_str()));
    let (cause, reason) = match reasons
        .filter(|why| !is_capacity(why))
        .min_by_key(|why| cause_of(why).rank())
    {
        Some(why) => (cause_of(why), why.to_owned()),
        // No refusal at all: no host is registered or online. A spawn that
        // only capacity refused was filtered out above.
        None if record.last_reject.is_empty() && record.queue_reason.is_none() => {
            (Cause::Offline, NO_HOST.to_owned())
        }
        None => return None,
    };
    Some(Blocked {
        waited,
        cause,
        reason,
    })
}

/// The state of a stalled task's claimer, as the alarm names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimerState {
    /// Its spawn waits in the queue on something that a person must fix.
    Queued(Blocked),
    /// Its stopped session was fenced, so its turn cannot end.
    Fenced,
    /// Its session looks hung: a busy turn with no tool call and no text for
    /// `[alerts] hung_minutes`, or one that lost its turn when its host slept.
    Hung(crate::rules::hung::Hung),
}

impl fmt::Display for ClaimerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queued(blocked) => write!(f, "queued and blocked ({})", blocked.reason),
            Self::Fenced => f.write_str("fenced"),
            Self::Hung(hung) => write!(f, "hung ({})", hung.why()),
        }
    }
}

/// The `attention_msg` of a stalled task: its claimer and state, and the
/// tasks that wait on it through `blocked_by`. A hung claimer also gets the
/// one command that ends it.
pub fn stalled_text(claimer: &AgentId, state: &ClaimerState, holds: &[String]) -> String {
    let mut out = format!("its claimer {claimer} is {state}");
    if matches!(state, ClaimerState::Hung(_)) {
        out.push_str(&format!("; run `borant restart {claimer}`"));
    }
    if !holds.is_empty() {
        out.push_str(&format!("; holds: {}", holds.join(", ")));
    }
    out
}

/// One direct message about a stalled task, to its owner and its chief.
pub fn stalled_message(channel: &ChannelName, task: &Task, attention: &str) -> String {
    format!(
        "task #{channel}/{} ({}) is stalled and stuck: {attention}.",
        task.id, task.title
    )
}

/// The spawner of a spawn: the agent that it names, if it was an agent.
pub fn spawner(by: &Sender) -> Option<AgentId> {
    match by {
        Sender::Agent(agent) => Some(agent.clone()),
        Sender::Human | Sender::Approval(_) => None,
    }
}

/// A wait as `borant board` and the alarm print it: seconds, minutes, hours,
/// or days.
pub fn waited_label(waited: Duration) -> String {
    let secs = waited.as_secs();
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LaunchFailure, SpawnSpec};
    use crate::rules::roles::Resolved;
    use crate::rules::schedule::Reject;

    fn record(state: SpawnState, wait_secs: i64, rejects: &[&str]) -> SpawnRecord {
        let at = Timestamp(1_000_000);
        SpawnRecord {
            agent: "w1".parse().unwrap(),
            spec: SpawnSpec {
                channel: "api".parse().unwrap(),
                prompt: String::new(),
                harness: None,
                host: None,
                name: None,
                model: None,
                role: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                bundles: Vec::new(),
            },
            resolved: Resolved::default(),
            by: Sender::Human,
            queued_at: Timestamp(at.0 - wait_secs * 1_000),
            priority: 0,
            state,
            attempts: 0,
            last_reject: rejects
                .iter()
                .enumerate()
                .map(|(i, why)| {
                    let host: crate::HostName = format!("h{i}").parse().unwrap();
                    (host, (*why).to_owned())
                })
                .collect(),
            queue_reason: None,
            launch_failures: Vec::new(),
            recoveries: 0,
        }
    }

    fn agent() -> AgentId {
        "w1".parse().unwrap()
    }

    #[test]
    fn a_short_wait_is_not_blocked() {
        let r = record(SpawnState::Queued, 14 * 60, &["missing bundle github-work"]);
        assert_eq!(blocked(&r, Timestamp(1_000_000)), None);
    }

    #[test]
    fn a_spawn_that_no_longer_queues_is_not_blocked() {
        let r = record(SpawnState::Ended, 3_600, &["missing bundle github-work"]);
        assert_eq!(blocked(&r, Timestamp(1_000_000)), None);
    }

    #[test]
    fn capacity_alone_is_not_blocked() {
        let r = record(
            SpawnState::Queued,
            3_600,
            &[
                "full (4/4)",
                "low memory (100 MB free, 1024 reserved)",
                "disk full (96%)",
            ],
        );
        assert_eq!(blocked(&r, Timestamp(1_000_000)), None);

        let mut capped = record(SpawnState::Queued, 3_600, &[]);
        capped.queue_reason = Some("channel full (2/2)".to_owned());
        assert_eq!(blocked(&capped, Timestamp(1_000_000)), None);
    }

    #[test]
    fn one_refusal_that_capacity_does_not_explain_blocks_the_spawn() {
        let r = record(
            SpawnState::Queued,
            3_600,
            &["full (4/4)", "missing bundle github-work"],
        );
        let b = blocked(&r, Timestamp(1_000_000)).expect("blocked");
        assert_eq!(b.reason, "missing bundle github-work");
        assert_eq!(
            b.text(&agent()),
            "spawn w1 blocked 1h: missing bundle github-work. \
             Unblock: add bundle github-work to a host that the spawn may use, \
             or spawn without github-work"
        );
    }

    #[test]
    fn the_alarm_names_the_most_actionable_refusal() {
        let r = record(
            SpawnState::Queued,
            3_600,
            &["offline", "failed recently", "missing tag vps"],
        );
        let b = blocked(&r, Timestamp(1_000_000)).expect("blocked");
        assert_eq!(b.cause, Cause::MissingTag("vps".parse().unwrap()));
        assert_eq!(b.reason, "missing tag vps");
        assert_eq!(
            b.action(&agent()),
            "tag a host with vps, or drop vps from the role"
        );
    }

    #[test]
    fn no_host_at_all_is_offline() {
        let r = record(SpawnState::Queued, 3_600, &[]);
        let b = blocked(&r, Timestamp(1_000_000)).expect("blocked");
        assert_eq!(b.cause, Cause::Offline);
        assert_eq!(b.reason, NO_HOST);
        assert_eq!(b.action(&agent()), "bring a host online");
    }

    /// The classifier reads the text of `Reject`, so every variant of the
    /// enum must land on the capacity side or the blocked side as intended.
    #[test]
    fn every_refusal_classifies() {
        let capacity = [
            Reject::Full { active: 4, max: 4 },
            Reject::LowMemory {
                avail_mb: 10,
                reserve_mb: 1024,
            },
            Reject::LowDisk { used_pct: 96 },
        ];
        for reject in &capacity {
            assert!(is_capacity(&reject.to_string()), "{reject} is capacity");
        }
        let blocked = [
            Reject::Offline,
            Reject::Draining,
            Reject::NoRepo,
            Reject::NoHarness,
            Reject::MissingTag("vps".parse().unwrap()),
            Reject::MissingBundle("github-work".parse().unwrap()),
            Reject::NoMemberBundle("claude-auth".parse().unwrap()),
            Reject::NoSandbox,
            Reject::RecentlyFailed,
        ];
        for reject in &blocked {
            assert!(!is_capacity(&reject.to_string()), "{reject} blocks");
            assert_ne!(
                cause_of(&reject.to_string()),
                Cause::Other(reject.to_string()),
                "{reject} is classified"
            );
        }
        assert_eq!(
            cause_of("missing bundle github-work"),
            Cause::MissingBundle("github-work".parse().unwrap())
        );
        assert_eq!(
            cause_of("no member bundle claude-auth"),
            Cause::NoMemberBundle("claude-auth".parse().unwrap())
        );
    }

    #[test]
    fn the_resume_action_quotes_the_command() {
        let cause = Cause::RecentlyFailed;
        assert_eq!(
            cause.action(&agent()),
            "`borant resume w1` after fixing the launch error"
        );
    }

    #[test]
    fn a_stalled_text_names_the_claimer_its_state_and_what_it_holds() {
        let holds = vec!["the migration".to_owned(), "the docs".to_owned()];
        assert_eq!(
            stalled_text(&agent(), &ClaimerState::Fenced, &holds),
            "its claimer w1 is fenced; holds: the migration, the docs"
        );
        assert_eq!(
            stalled_text(&agent(), &ClaimerState::Fenced, &[]),
            "its claimer w1 is fenced"
        );
        let queued = ClaimerState::Queued(Blocked {
            waited: Duration::from_secs(3_600),
            cause: Cause::MissingBundle("github-work".parse().unwrap()),
            reason: "missing bundle github-work".to_owned(),
        });
        assert_eq!(
            queued.to_string(),
            "queued and blocked (missing bundle github-work)"
        );
    }

    #[test]
    fn the_wait_reads_in_one_unit() {
        assert_eq!(waited_label(Duration::from_secs(45)), "45s");
        assert_eq!(waited_label(Duration::from_secs(15 * 60)), "15m");
        assert_eq!(waited_label(Duration::from_secs(3 * 3_600)), "3h");
        assert_eq!(waited_label(Duration::from_secs(2 * 86_400)), "2d");
    }

    #[test]
    fn a_spawner_is_named_only_when_an_agent_spawned() {
        assert_eq!(spawner(&Sender::Human), None);
        assert_eq!(
            spawner(&Sender::Agent(agent())),
            Some("w1".parse().unwrap())
        );
    }

    #[test]
    fn a_launch_failure_of_a_queued_spawn_keeps_its_count() {
        // A refused spawn that also failed a launch names the failure, and
        // the alarm still reads the refusal that blocks it.
        let mut r = record(SpawnState::Queued, 3_600, &["failed recently"]);
        r.launch_failures.push(LaunchFailure {
            host: "h1".parse().unwrap(),
            timeout_secs: None,
            error: Some("clone failed".into()),
        });
        assert_eq!(
            blocked(&r, Timestamp(1_000_000)).unwrap().reason,
            "failed recently"
        );
        assert_eq!(
            r.last_launch_error().unwrap().error.as_deref(),
            Some("clone failed")
        );
    }
}
