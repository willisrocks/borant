//! Bounded task continuation (phase 11 spec): whether an idle worker bound
//! to a task gets a "continue" message, and whether the limit ran out.
//!
//! Pure: the hub reads the agent, the task, its own idle report, and its
//! stored waits, then calls [`decide`] and writes what it says. The spec's
//! signature names one thing less than the guards do: a move in flight is the
//! hub's memory (`Memory.pending_moves`), so it is passed as `moving`.

use std::fmt;
use std::time::Duration;

use crate::error::CoreError;
use crate::model::{Agent, ContinuePolicy, Task, TaskBinding, TaskStatus};
use crate::time::Timestamp;

/// One hour, the unit of `ContinuePolicy.max_hours`.
const HOUR: Duration = Duration::from_secs(3600);

/// What the hub does with one idle report of a session (phase 11 spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Send "continue", after writing `used + 1` and `last_idle`.
    Continue,
    /// Do nothing.
    Skip(Why),
    /// The limit ran out: the task goes `needs_human` and the agent gets no
    /// continue again.
    Exhaust(Why),
}

/// The reason of a [`Decision`]. The two exhaust reasons are the
/// `needs_human` message of the task, so they read as the spec writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The agent has no `ContinuePolicy`.
    NoPolicy,
    /// A previous exhaust already stopped it.
    Exhausted,
    /// A person or the channel owner stopped it.
    Paused,
    /// The agent is never placed again.
    Retired,
    /// A move is in flight: the session is about to stop.
    Moving,
    /// The session is in a turn, or the host reported nothing.
    NotIdle,
    /// The idle period that already got a continue.
    SameIdlePeriod,
    /// The agent is bound to no task.
    NoTask,
    /// The task passed is not the one that the agent is bound to.
    NotBoundTask,
    /// The bound task is not `in_progress`.
    TaskNotInProgress,
    /// The bound task is claimed by another agent, or by nobody.
    TaskNotClaimedByAgent,
    /// The agent stored a wait on the task: it is done, and waits.
    WaitingOnTask,
    /// `used` reached `max`.
    LimitReached { max: u32 },
    /// The session ran longer than `max_hours`.
    HoursReached { max_hours: u32 },
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPolicy => f.write_str("no continue policy"),
            Self::Exhausted => f.write_str("the continues are exhausted"),
            Self::Paused => f.write_str("the agent is paused"),
            Self::Retired => f.write_str("the agent is retired"),
            Self::Moving => f.write_str("the agent is moving"),
            Self::NotIdle => f.write_str("the agent is not idle"),
            Self::SameIdlePeriod => f.write_str("the idle period already got a continue"),
            Self::NoTask => f.write_str("the agent is bound to no task"),
            Self::NotBoundTask => f.write_str("the task is not the bound task"),
            Self::TaskNotInProgress => f.write_str("the task is not in progress"),
            Self::TaskNotClaimedByAgent => f.write_str("the agent holds no claim on the task"),
            Self::WaitingOnTask => f.write_str("the agent waits on the task"),
            Self::LimitReached { max } => write!(f, "continue limit reached ({max})"),
            Self::HoursReached { max_hours } => write!(f, "continue hours reached ({max_hours})"),
        }
    }
}

/// Whether an idle worker gets a continue. It continues only when every guard
/// holds; it exhausts when the only thing left is the count or the hours on a
/// task that it still claims and still works on; every other case skips.
///
/// The hours run from `Agent.created`, so a move keeps the same clock.
pub fn decide(
    agent: &Agent,
    task: Option<&Task>,
    idle_since: Option<Timestamp>,
    waiting_on_task: bool,
    moving: bool,
    now: Timestamp,
) -> Decision {
    let Some(state) = &agent.continues else {
        return Decision::Skip(Why::NoPolicy);
    };
    if state.exhausted {
        return Decision::Skip(Why::Exhausted);
    }
    if agent.paused.is_some() {
        return Decision::Skip(Why::Paused);
    }
    if agent.retired {
        return Decision::Skip(Why::Retired);
    }
    if moving {
        return Decision::Skip(Why::Moving);
    }
    let Some(idle_since) = idle_since else {
        return Decision::Skip(Why::NotIdle);
    };
    // One idle period never gets two continues.
    if state.last_idle == Some(idle_since) {
        return Decision::Skip(Why::SameIdlePeriod);
    }
    let Some(task) = task else {
        return Decision::Skip(Why::NoTask);
    };
    if agent.task.as_ref().is_none_or(|b| b.id != task.id) {
        return Decision::Skip(Why::NotBoundTask);
    }
    if task.status != TaskStatus::InProgress {
        return Decision::Skip(Why::TaskNotInProgress);
    }
    if task.claimed_by.as_ref() != Some(&agent.id) {
        return Decision::Skip(Why::TaskNotClaimedByAgent);
    }
    if waiting_on_task {
        return Decision::Skip(Why::WaitingOnTask);
    }
    // The count first, then the hours: a policy that a change shrank under
    // the count exhausts with the count too.
    if state.used >= state.policy.max {
        return Decision::Exhaust(Why::LimitReached {
            max: state.policy.max,
        });
    }
    if let Some(max_hours) = state.policy.max_hours
        && now.since(agent.created) >= HOUR.saturating_mul(max_hours)
    {
        return Decision::Exhaust(Why::HoursReached { max_hours });
    }
    Decision::Continue
}

/// A spawn may ask for continues only when it binds a task, and only with a
/// limit of at least one (phase 11 spec). The hub answers `Invalid`.
pub fn check_spawn(
    continues: Option<&ContinuePolicy>,
    task: Option<&TaskBinding>,
) -> Result<(), CoreError> {
    let Some(policy) = continues else {
        return Ok(());
    };
    if task.is_none() {
        return Err(CoreError::ContinuesWithoutTask);
    }
    if policy.max == 0 {
        return Err(CoreError::ContinueLimit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;
    use crate::model::{ContinueState, Harness, PauseReason, Sender, TaskKind, TaskRef};

    fn agent() -> Agent {
        Agent {
            preferred_harness: Harness::Fake,
            created: Timestamp(0),
            task: Some(TaskRef {
                channel: "api".parse().unwrap(),
                id: task_id(),
            }),
            continues: Some(ContinueState::new(ContinuePolicy {
                max: 3,
                max_hours: None,
            })),
            ..Default::default()
        }
    }

    fn task_id() -> TaskId {
        "01M5S7YF2GSX8GPG3YQ2K5KH4A".parse().unwrap()
    }

    fn task() -> Task {
        Task {
            id: task_id(),
            title: "do the work".into(),
            body: String::new(),
            status: TaskStatus::InProgress,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: Some("test-agent".parse().unwrap()),
            deadline: None,
            attempts: 0,
            max_attempts: None,
            blocked_by: Vec::new(),
            parent: None,
            kind: TaskKind::Task,
            issue: None,
            issue_target: None,
            properties: Default::default(),
            created: Timestamp(0),
            updated: Timestamp(0),
            create_key: None,
        }
    }

    const IDLE: Timestamp = Timestamp(1_000);

    fn idle() -> Option<Timestamp> {
        Some(IDLE)
    }

    fn skip(agent: &Agent, task: Option<&Task>, idle_since: Option<Timestamp>) -> Why {
        match decide(agent, task, idle_since, false, false, Timestamp(2_000)) {
            Decision::Skip(why) => why,
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn a_bound_idle_worker_continues() {
        assert_eq!(
            decide(
                &agent(),
                Some(&task()),
                idle(),
                false,
                false,
                Timestamp(2_000)
            ),
            Decision::Continue
        );
    }

    #[test]
    fn every_other_case_skips_with_its_reason() {
        let no_policy = Agent {
            continues: None,
            ..agent()
        };
        let exhausted = Agent {
            continues: Some(ContinueState {
                exhausted: true,
                ..ContinueState::new(ContinuePolicy {
                    max: 3,
                    max_hours: None,
                })
            }),
            ..agent()
        };
        let paused = Agent {
            paused: Some(PauseReason {
                by: Sender::Human,
                why: "cap".into(),
                at: Timestamp(1),
            }),
            ..agent()
        };
        let retired = Agent {
            retired: true,
            ..agent()
        };
        let unbound = Agent {
            task: None,
            ..agent()
        };
        let other_task = Agent {
            task: Some(TaskRef {
                channel: "api".parse().unwrap(),
                id: "01M5S7YF2GSX8GPG3YQ2K5KH4B".parse().unwrap(),
            }),
            ..agent()
        };
        let open = Task {
            status: TaskStatus::Open,
            ..task()
        };
        let unclaimed = Task {
            claimed_by: None,
            ..task()
        };
        let another: Agent = {
            let mut a = agent();
            a.id = "other".parse().unwrap();
            a
        };
        let mut recorded = agent();
        recorded.continues.as_mut().unwrap().record_continue(IDLE);

        assert_eq!(skip(&no_policy, Some(&task()), idle()), Why::NoPolicy);
        assert_eq!(skip(&exhausted, Some(&task()), idle()), Why::Exhausted);
        assert_eq!(skip(&paused, Some(&task()), idle()), Why::Paused);
        assert_eq!(skip(&retired, Some(&task()), idle()), Why::Retired);
        assert_eq!(
            decide(
                &agent(),
                Some(&task()),
                idle(),
                false,
                true,
                Timestamp(2_000)
            ),
            Decision::Skip(Why::Moving)
        );
        assert_eq!(skip(&agent(), Some(&task()), None), Why::NotIdle);
        assert_eq!(skip(&recorded, Some(&task()), idle()), Why::SameIdlePeriod);
        assert_eq!(skip(&other_task, Some(&task()), idle()), Why::NotBoundTask);
        // A task handed to an agent that is bound to nothing is not its own.
        assert_eq!(skip(&unbound, Some(&task()), idle()), Why::NotBoundTask);
        assert_eq!(skip(&agent(), Some(&open), idle()), Why::TaskNotInProgress);
        assert_eq!(
            skip(&agent(), Some(&unclaimed), idle()),
            Why::TaskNotClaimedByAgent
        );
        assert_eq!(
            decide(
                &another,
                Some(&task()),
                idle(),
                false,
                false,
                Timestamp(2_000)
            ),
            Decision::Skip(Why::TaskNotClaimedByAgent)
        );
        assert_eq!(
            decide(
                &agent(),
                Some(&task()),
                idle(),
                true,
                false,
                Timestamp(2_000)
            ),
            Decision::Skip(Why::WaitingOnTask)
        );
        assert_eq!(
            decide(&agent(), None, idle(), false, false, Timestamp(2_000)),
            Decision::Skip(Why::NoTask)
        );
    }

    #[test]
    fn the_limit_exhausts_the_continues() {
        let mut agent = agent();
        // `max: 3` gives three continues, and the fourth idle period exhausts.
        for period in 1..=3 {
            assert_eq!(
                decide(
                    &agent,
                    Some(&task()),
                    Some(Timestamp(1_000 * period)),
                    false,
                    false,
                    Timestamp(10_000)
                ),
                Decision::Continue
            );
            agent
                .continues
                .as_mut()
                .unwrap()
                .record_continue(Timestamp(1_000 * period));
        }
        assert_eq!(agent.continues.unwrap().used, 3);
        let why = Why::LimitReached { max: 3 };
        assert_eq!(why.to_string(), "continue limit reached (3)");
        assert_eq!(
            decide(
                &agent,
                Some(&task()),
                Some(Timestamp(4_000)),
                false,
                false,
                Timestamp(10_000)
            ),
            Decision::Exhaust(why)
        );
        // A policy that shrank under the count exhausts too, not skips.
        let over = Agent {
            continues: Some(ContinueState {
                policy: ContinuePolicy {
                    max: 2,
                    max_hours: None,
                },
                used: 3,
                last_idle: Some(Timestamp(1_000)),
                exhausted: false,
            }),
            ..agent
        };
        assert_eq!(
            decide(
                &over,
                Some(&task()),
                Some(Timestamp(4_000)),
                false,
                false,
                Timestamp(10_000)
            ),
            Decision::Exhaust(Why::LimitReached { max: 2 })
        );
    }

    #[test]
    fn the_hours_exhaust_the_continues() {
        let agent = Agent {
            continues: Some(ContinueState::new(ContinuePolicy {
                max: 100,
                max_hours: Some(2),
            })),
            ..agent()
        };
        let two_hours = Duration::from_secs(2 * 3600);
        // One millisecond before the bound still continues.
        assert_eq!(
            decide(
                &agent,
                Some(&task()),
                idle(),
                false,
                false,
                Timestamp(0).plus(two_hours).minus(Duration::from_millis(1))
            ),
            Decision::Continue
        );
        let why = Why::HoursReached { max_hours: 2 };
        assert_eq!(why.to_string(), "continue hours reached (2)");
        assert_eq!(
            decide(
                &agent,
                Some(&task()),
                idle(),
                false,
                false,
                Timestamp(0).plus(two_hours)
            ),
            Decision::Exhaust(why)
        );
    }

    #[test]
    fn one_idle_period_gets_one_continue() {
        let mut agent = agent();
        let at = Timestamp(5_000);
        assert_eq!(
            decide(&agent, Some(&task()), idle(), false, false, at),
            Decision::Continue
        );
        agent.continues.as_mut().unwrap().record_continue(IDLE);
        // The next heartbeat of the same idle period gets nothing.
        assert_eq!(
            decide(&agent, Some(&task()), idle(), false, false, at.plus(HOUR)),
            Decision::Skip(Why::SameIdlePeriod)
        );
        // The next turn ends: a new idle period, and the next continue.
        let next = Timestamp(9_000);
        assert_eq!(
            decide(&agent, Some(&task()), Some(next), false, false, at),
            Decision::Continue
        );
        assert_eq!(agent.continues.unwrap().used, 1);
    }

    #[test]
    fn a_spawn_needs_a_task_for_its_continues() {
        let policy = ContinuePolicy {
            max: 2,
            max_hours: None,
        };
        let binding = TaskBinding::Existing {
            channel: "api".parse().unwrap(),
            id: task_id(),
        };
        assert_eq!(check_spawn(None, None), Ok(()));
        assert_eq!(check_spawn(Some(&policy), Some(&binding)), Ok(()));
        assert_eq!(
            check_spawn(Some(&policy), None),
            Err(CoreError::ContinuesWithoutTask)
        );
        // The wire refuses a limit below one, so a stored record cannot.
        let zero: Result<ContinuePolicy, _> = serde_json::from_str(r#"{"max":0}"#);
        assert!(zero.is_err());
        assert_eq!(
            check_spawn(
                Some(&ContinuePolicy {
                    max: 0,
                    max_hours: None
                }),
                Some(&binding)
            ),
            Err(CoreError::ContinueLimit)
        );
    }

    #[test]
    fn old_json_of_an_agent_and_a_spawn_parses() {
        let agent = agent();
        let mut old = serde_json::to_value(&agent).unwrap();
        old.as_object_mut().unwrap().remove("continues");
        let parsed: Agent = serde_json::from_value(old).expect("old agent JSON parses");
        assert_eq!(parsed.continues, None);

        // The new field round-trips with its count and policy.
        let back: Agent = serde_json::from_value(serde_json::to_value(&agent).unwrap()).unwrap();
        assert_eq!(back.continues, agent.continues);

        // A spawn from before the field, and one that names it.
        let old: crate::proto::ToolCall = serde_json::from_value(serde_json::json!({
            "tool": "spawn",
            "channel": "api",
            "prompt": "do the work",
        }))
        .unwrap();
        assert!(matches!(
            old,
            crate::proto::ToolCall::Spawn {
                continues: None,
                ..
            }
        ));
        let new: crate::proto::ToolCall = serde_json::from_value(serde_json::json!({
            "tool": "spawn",
            "channel": "api",
            "prompt": "do the work",
            "continues": {"max": 2, "max_hours": 4},
        }))
        .unwrap();
        assert!(matches!(
            new,
            crate::proto::ToolCall::Spawn {
                continues: Some(ContinuePolicy {
                    max: 2,
                    max_hours: Some(4)
                }),
                ..
            }
        ));
    }
}
