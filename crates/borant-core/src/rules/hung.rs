//! A hung session: an agent that is in a turn with no tool call and no text
//! for `[alerts] hung_minutes`, or one that lost its turn when its host slept.
//!
//! The host delivers messages to a busy session through its hooks, so a busy
//! session with no hook for that long has read nothing that was sent to it.
//! The rule only flags: a person restarts the agent, and the hub never kills a
//! session that it only suspects.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::Activity;
use crate::time::Timestamp;

/// The default of `[alerts] hung_minutes`.
pub const DEFAULT_HUNG_MINUTES: u64 = 20;

/// The last sign of life of a session: its last event, or the last tool call
/// or text of its activity report, whichever is later. `None` when it reported
/// nothing, which is no data, not silence.
pub fn last_alive(
    last_event_at: Option<Timestamp>,
    activity: Option<&Activity>,
) -> Option<Timestamp> {
    let tool = activity.and_then(|a| a.last_tool_at);
    let text = activity.and_then(|a| a.last_text_at);
    [last_event_at, tool, text].into_iter().flatten().max()
}

/// Why a session looks hung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hung {
    /// The last sign of life of the session, when it reported one.
    #[serde(default)]
    pub last_alive: Option<Timestamp>,
    /// The host slept after that sign of life, so the turn lost its progress.
    #[serde(default)]
    pub host_slept: bool,
    /// How long the session has been quiet, from `last_alive` to the check.
    #[serde(default)]
    pub quiet_secs: u64,
}

impl Hung {
    /// Why the session looks hung, without the agent: "no tool call or text
    /// for 25 minutes", or the sleep that stopped its turn.
    pub fn why(&self) -> String {
        if self.host_slept {
            "its host slept and the turn made no progress since".to_owned()
        } else {
            format!("no tool call or text for {} minutes", self.quiet_secs / 60)
        }
    }

    /// The line of the alert and the blocked-work alarm: why the session looks
    /// hung, and the one command that ends it.
    pub fn line(&self, agent: &crate::AgentId) -> String {
        format!(
            "{agent} looks hung: {}; run `borant restart {agent}`",
            self.why()
        )
    }
}

/// Whether a session looks hung at `now`.
///
/// Only a busy session can hang: an idle one waits for the person. A tool that
/// runs is a sign of life, however long it runs, so a session with one is
/// never hung. A host that slept after the last sign of life left the turn
/// where it was; the session stays hung until it moves again, which is what
/// makes a lid-close sleep visible.
pub fn hung_at(
    busy: bool,
    tool_running: bool,
    last_alive: Option<Timestamp>,
    slept_at: Option<Timestamp>,
    minutes: u64,
    now: Timestamp,
) -> Option<Hung> {
    if !busy || tool_running {
        return None;
    }
    let quiet = last_alive.map_or(Duration::ZERO, |last| now.since(last));
    let slept = slept_at.is_some_and(|slept| last_alive.is_some_and(|last| slept > last));
    if !slept && (last_alive.is_none() || quiet < Duration::from_secs(minutes.saturating_mul(60))) {
        return None;
    }
    Some(Hung {
        last_alive,
        host_slept: slept,
        quiet_secs: u64::try_from(quiet.as_secs()).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_000_000_000);

    fn minutes(n: u64) -> Timestamp {
        NOW.minus(Duration::from_secs(n * 60))
    }

    #[test]
    fn a_quiet_busy_session_is_hung_at_the_threshold() {
        let hung = hung_at(true, false, Some(minutes(20)), None, 20, NOW).expect("hung");
        assert!(!hung.host_slept);
        assert_eq!(hung.quiet_secs, 20 * 60);
        assert!(hung_at(true, false, Some(minutes(19)), None, 20, NOW).is_none());
    }

    #[test]
    fn an_idle_session_a_running_tool_and_a_silent_report_are_not_hung() {
        assert!(hung_at(false, false, Some(minutes(120)), None, 20, NOW).is_none());
        assert!(hung_at(true, true, Some(minutes(120)), None, 20, NOW).is_none());
        // No report at all is no data, not silence.
        assert!(hung_at(true, false, None, None, 20, NOW).is_none());
    }

    #[test]
    fn a_host_that_slept_hangs_the_turn_that_did_not_move() {
        // The host slept ten minutes after the last tool call, and the session
        // has said nothing since: the turn lost that time.
        let slept = minutes(50);
        let hung = hung_at(true, false, Some(minutes(60)), Some(slept), 20, NOW).expect("hung");
        assert!(hung.host_slept);
        // A session that moved after the sleep is fine.
        assert!(hung_at(true, false, Some(minutes(5)), Some(slept), 20, NOW).is_none());
        // A sleep before the last sign of life says nothing.
        assert!(hung_at(true, false, Some(minutes(5)), Some(minutes(30)), 20, NOW).is_none());
    }

    #[test]
    fn the_last_sign_of_life_is_the_latest_of_the_event_and_the_activity() {
        let activity = Activity {
            last_tool_at: Some(minutes(3)),
            last_text_at: Some(minutes(1)),
            ..Activity::default()
        };
        assert_eq!(
            last_alive(Some(minutes(9)), Some(&activity)),
            Some(minutes(1))
        );
        assert_eq!(last_alive(Some(minutes(9)), None), Some(minutes(9)));
        assert_eq!(last_alive(None, None), None);
        // A tool that runs now is reported as its start, so it is no sign of
        // life by itself.
        let running = Activity {
            last_tool_at: Some(minutes(40)),
            tool_running_since: Some(minutes(40)),
            ..Activity::default()
        };
        assert_eq!(last_alive(None, Some(&running)), Some(minutes(40)));
    }

    #[test]
    fn the_line_names_the_agent_and_offers_the_restart() {
        let agent: crate::AgentId = "w1".parse().unwrap();
        let quiet = hung_at(true, false, Some(minutes(25)), None, 20, NOW).unwrap();
        assert_eq!(
            quiet.line(&agent),
            "w1 looks hung: no tool call or text for 25 minutes; run `borant restart w1`"
        );
        let slept = hung_at(true, false, Some(minutes(60)), Some(minutes(30)), 20, NOW).unwrap();
        assert_eq!(
            slept.line(&agent),
            "w1 looks hung: its host slept and the turn made no progress since; run `borant restart w1`"
        );
    }
}
