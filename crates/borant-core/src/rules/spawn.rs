//! The idempotency of a spawn (phase 3 spec, "Idempotency"). A second spawn
//! with one key and the same arguments returns the first agent; a different
//! argument is a conflict.

use std::fmt;

use crate::ids::AgentId;
use crate::model::SpawnSpec;

/// A repeated key that names a different spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict;

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the key names a spawn with different arguments")
    }
}

impl std::error::Error for Conflict {}

/// `Ok` when a spawn with `existing` and one with `new` may share a key: the
/// second returns the first agent. Any difference is a conflict.
pub fn spawn_key_matches(existing: &SpawnSpec, new: &SpawnSpec) -> Result<(), Conflict> {
    if existing == new {
        Ok(())
    } else {
        Err(Conflict)
    }
}

/// The name of the agent of a spawn with a key: a short, stable hash of the
/// key, so a retry that arrives before the agent is stored finds the same
/// name, and so a name that the caller did not choose cannot collide by
/// accident.
pub fn agent_name_from_key(key: &str) -> AgentId {
    // FNV-1a, 64 bits: the same value on every run and every host.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("sp-{hash:012x}")
        .parse()
        .expect("a hex name is a valid agent ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(prompt: &str) -> SpawnSpec {
        SpawnSpec {
            channel: "api".parse().unwrap(),
            prompt: prompt.to_owned(),
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
        }
    }

    #[test]
    fn spawn_key_matches_only_the_same_arguments() {
        assert_eq!(spawn_key_matches(&spec("do"), &spec("do")), Ok(()));

        let mut other = spec("do");
        other.priority = Some(2);
        assert_eq!(spawn_key_matches(&spec("do"), &other), Err(Conflict));

        let mut model = spec("do");
        model.model = Some("m".to_owned());
        assert_eq!(spawn_key_matches(&spec("do"), &model), Err(Conflict));

        // Another continue policy is another spawn.
        let mut continues = spec("do");
        continues.continues = Some(crate::model::ContinuePolicy {
            max: 2,
            max_hours: None,
        });
        assert_eq!(
            spawn_key_matches(&spec("do"), &continues),
            Err(Conflict)
        );
        assert_eq!(
            spawn_key_matches(&spec("do"), &spec("another")),
            Err(Conflict)
        );
    }

    #[test]
    fn agent_name_from_key_is_short_stable_and_distinct() {
        let one = agent_name_from_key("orchestrate-1-worker-a");
        assert_eq!(one, agent_name_from_key("orchestrate-1-worker-a"));
        assert_ne!(one, agent_name_from_key("orchestrate-1-worker-b"));
        assert!(one.as_str().starts_with("sp-"), "{one}");
        assert!(one.as_str().len() <= 63, "{one}");
        // A name that is already an agent ID is left alone by the rule.
        assert_eq!(
            agent_name_from_key(""),
            "sp-cbf29ce484222325".parse().unwrap()
        );
    }
}

#[cfg(test)]
mod compat_tests {
    use crate::model::{Agent, Sender, Task, TaskStatus};
    use crate::proto::ToolCall;
    use serde_json::json;

    /// JSON written before phase 3 still parses: an agent, a task, a spawn
    /// call, and a sender that names a bare agent id.
    #[test]
    fn old_json_of_an_agent_a_task_and_a_spawn_parses() {
        let agent: Agent = serde_json::from_value(json!({
            "id": "w1",
            "preferred_harness": "claude-code",
            "repo": null,
            "movable": false,
            "channels": [],
            "session": null,
            "host": null,
            "harness_session_id": null,
            "created": 1
        }))
        .expect("an agent from before spawns");
        assert_eq!(agent.spawn_key, None);
        assert_eq!(agent.task, None);
        assert!(!agent.stop_on_close);
        assert!(!agent.retired);

        let task: Task = serde_json::from_value(json!({
            "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "title": "t",
            "status": "open",
            "close_reason": null,
            "close_message": null,
            "attention_msg": null,
            "claimed_by": null,
            "parent": null,
            "created": 1,
            "updated": 1
        }))
        .expect("a task from before the create key");
        assert_eq!(task.create_key, None);
        assert_eq!(task.status, TaskStatus::Open);

        let call: ToolCall = serde_json::from_value(json!({
            "tool": "spawn",
            "channel": "api",
            "prompt": "work"
        }))
        .expect("a spawn call from before the key and the task");
        match call {
            ToolCall::Spawn {
                key,
                task,
                stop_on_close,
                priority,
                ..
            } => {
                assert_eq!(key, None);
                assert_eq!(task, None);
                assert_eq!(stop_on_close, None);
                assert_eq!(priority, None);
            }
            other => panic!("not a spawn: {other:?}"),
        }

        // A sender was a bare agent id.
        assert_eq!(
            serde_json::from_value::<Sender>(json!("w1")).unwrap(),
            Sender::Agent("w1".parse().unwrap())
        );
        assert_eq!(
            serde_json::from_value::<Sender>(json!({"type": "agent", "id": "w1"})).unwrap(),
            Sender::Agent("w1".parse().unwrap())
        );
        assert_eq!(
            serde_json::from_value::<Sender>(json!("human")).unwrap(),
            Sender::Human
        );
    }
}
