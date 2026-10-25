//! Member rules (spec "Member rules"). `pick` decides which account of a
//! bundle a launch uses; the hub assignment calls it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ids::MemberName;
use crate::model::{MemberState, UsageWindow};
use crate::time::Timestamp;

/// How long a usage report stays fresh. The caller passes only fresh reports to
/// [`pick`].
pub const REPORT_FRESH_FOR: Duration = Duration::from_secs(15 * 60);

/// The rules for bundle members, from `[secrets.members]` in `hub.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRules {
    /// The reset time to assume when a limit report gives none.
    pub unknown_reset_mins: u64,
    /// How often one agent may restart on a new member in an hour, then it
    /// waits for the member to reset.
    pub max_restarts_per_hour: u32,
}

impl Default for MemberRules {
    fn default() -> Self {
        Self {
            unknown_reset_mins: 60,
            max_restarts_per_hour: 3,
        }
    }
}

/// The result of one pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// The member for this launch.
    Member(MemberName),
    /// Every member is limited now. The agent waits until this time.
    WaitUntil(Timestamp),
}

/// Rules 2 to 6 of the spec, for one bundle at one launch.
///
/// `members` are the members that the target host defines, `states` holds what
/// is known about them, and every entry of `reports` must be fresh.
pub fn pick(
    members: &[MemberName],
    current: Option<&MemberName>,
    states: &[MemberState],
    reports: &[(MemberName, Vec<UsageWindow>)],
    now: Timestamp,
) -> Pick {
    let limited_until = |m: &MemberName| -> Option<Timestamp> {
        states
            .iter()
            .find(|s| &s.member == m)
            .and_then(|s| s.limited_until)
    };
    // Rule 2: keep only the members that are not limited now.
    let free: Vec<&MemberName> = members
        .iter()
        .filter(|m| limited_until(m).is_none_or(|until| until <= now))
        .collect();
    // Rule 3: the member of the agent keeps the bundle when it is free.
    if let Some(current) = current
        && free.contains(&current)
    {
        return Pick::Member(current.clone());
    }
    // Rule 4: among the free members with a fresh report, the one with the
    // earliest reset over its windows. Members without a report come after.
    let reset = |m: &MemberName| -> Option<i64> {
        reports
            .iter()
            .find(|(r, _)| r == m)
            .and_then(|(_, windows)| windows.iter().map(|w| w.resets_at.0).min())
    };
    if let Some((_, member)) = free
        .iter()
        .filter_map(|m| reset(m).map(|at| (at, *m)))
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)))
    {
        return Pick::Member(member.clone());
    }
    // Rule 5: the first free member in name order.
    if let Some(member) = free.iter().min() {
        return Pick::Member((*member).clone());
    }
    // Rule 6: every member is limited; the earliest reset wakes the agent.
    let until = members
        .iter()
        .filter_map(limited_until)
        .min()
        .unwrap_or(now);
    Pick::WaitUntil(until)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<MemberName> {
        list.iter().map(|n| n.parse().unwrap()).collect()
    }

    fn state(member: &str, limited_until: Option<Timestamp>) -> MemberState {
        MemberState {
            bundle: "claude_auth".parse().unwrap(),
            member: member.parse().unwrap(),
            limited_until,
            waiting: vec![],
        }
    }

    fn window(resets_at: i64) -> Vec<UsageWindow> {
        vec![UsageWindow {
            name: "5h".into(),
            used: 0.4,
            resets_at: Timestamp(resets_at),
        }]
    }

    fn name(picked: Pick) -> MemberName {
        match picked {
            Pick::Member(m) => m,
            other => panic!("expected a member, got {other:?}"),
        }
    }

    fn member(name: &str) -> MemberName {
        name.parse().unwrap()
    }

    /// Rule 2: a limited member is not picked.
    #[test]
    fn members_rule_2_skips_a_limited_member() {
        let members = names(&["a", "b"]);
        let states = [state("a", Some(Timestamp(2_000)))];
        let picked = pick(&members, None, &states, &[], Timestamp(1_000));
        assert_eq!(name(picked), member("b"));
        // A limit that is over no longer holds the member back.
        let picked = pick(&members, None, &states, &[], Timestamp(2_000));
        assert_eq!(name(picked), member("a"));
    }

    /// Rule 3: the current member is kept while it is free.
    #[test]
    fn members_rule_3_keeps_the_current_member() {
        let members = names(&["a", "b"]);
        let current = member("b");
        let picked = pick(&members, Some(&current), &[], &[], Timestamp(1_000));
        assert_eq!(name(picked), current);
        // A limited current member is not kept.
        let states = [state("b", Some(Timestamp(2_000)))];
        let picked = pick(&members, Some(&current), &states, &[], Timestamp(1_000));
        assert_eq!(name(picked), member("a"));
    }

    /// Rule 4: the free member with the earliest reset wins, and a member
    /// without a report comes after one with a report.
    #[test]
    fn members_rule_4_picks_the_earliest_reset() {
        let members = names(&["a", "b", "c"]);
        let reports = [(member("a"), window(3_000)), (member("b"), window(1_500))];
        let picked = pick(&members, None, &[], &reports, Timestamp(1_000));
        assert_eq!(name(picked), member("b"));
        // Only "c" has a report, so "c" comes before the two without one.
        let reports = [(member("c"), window(9_000))];
        let picked = pick(&members, None, &[], &reports, Timestamp(1_000));
        assert_eq!(name(picked), member("c"));
    }

    /// Rule 5: with no reports, the first free member in name order.
    #[test]
    fn members_rule_5_picks_the_first_free_name() {
        let members = names(&["c", "b", "a"]);
        let states = [state("a", Some(Timestamp(2_000)))];
        let picked = pick(&members, None, &states, &[], Timestamp(1_000));
        assert_eq!(name(picked), member("b"));
    }

    /// Rule 6: every member limited gives the earliest reset to wait for.
    #[test]
    fn members_rule_6_waits_for_the_earliest_reset() {
        let members = names(&["a", "b", "c"]);
        let states = [
            state("a", Some(Timestamp(5_000))),
            state("b", Some(Timestamp(3_000))),
            state("c", Some(Timestamp(9_000))),
        ];
        let picked = pick(&members, None, &states, &[], Timestamp(1_000));
        assert_eq!(picked, Pick::WaitUntil(Timestamp(3_000)));
        // A member with no state at all is free, so nobody waits.
        let states = [state("a", Some(Timestamp(5_000)))];
        let picked = pick(&members, None, &states, &[], Timestamp(1_000));
        assert_eq!(name(picked), member("b"));
    }
}
