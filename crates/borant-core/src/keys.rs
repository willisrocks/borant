//! The state-store key layout (spec section 6) and the bus subjects (spec
//! section 10).

use crate::ids::{AgentId, ChannelName, HostName, ProjectName, SessionId, TaskId};

/// True when a state store accepts this key. The KV of the real adapter
/// refuses an empty key, one that starts or ends with `.`, one with an empty
/// part (`..`), and one with whitespace, `*`, or `>`. A caller that builds a
/// key from a stored record checks it first, so one odd record cannot fail a
/// whole pass.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('.')
        && !key.ends_with('.')
        && !key.contains("..")
        && !key.contains(|c: char| c.is_whitespace() || c == '*' || c == '>')
}

pub fn agent(id: &AgentId) -> String {
    format!("agent.{id}")
}

pub const AGENT_PREFIX: &str = "agent.";

pub fn session(id: &SessionId) -> String {
    format!("session.{id}")
}

pub fn lease_agent(id: &AgentId) -> String {
    format!("lease.agent.{id}")
}

pub const LEASE_AGENT_PREFIX: &str = "lease.agent.";

pub fn lease_hub() -> String {
    "lease.hub".to_owned()
}

/// The hub's view of one pool host: pool, provider id, phase, and times.
pub fn elastic(host: &HostName) -> String {
    format!("elastic.{host}")
}

/// The queued spawn of one agent.
pub fn spawn(agent: &AgentId) -> String {
    format!("spawn.{agent}")
}

pub const ELASTIC_PREFIX: &str = "elastic.";

pub const SPAWN_PREFIX: &str = "spawn.";

/// What the month `<yyyy-mm>` has cost, all pools together.
pub fn spend(month: &str) -> String {
    format!("spend.{month}")
}

pub const SPEND_PREFIX: &str = "spend.";

pub fn host(name: &HostName) -> String {
    format!("host.{name}")
}

pub const HOST_PREFIX: &str = "host.";

pub fn channel(name: &ChannelName) -> String {
    format!("chan.{name}")
}

pub const CHANNEL_PREFIX: &str = "chan.";

pub fn project(name: &ProjectName) -> String {
    format!("project.{name}")
}

pub const PROJECT_PREFIX: &str = "project.";

pub fn member(channel: &ChannelName, agent: &AgentId) -> String {
    format!("chan.{channel}.member.{agent}")
}

pub fn members_prefix(channel: &ChannelName) -> String {
    format!("chan.{channel}.member.")
}

pub fn task(channel: &ChannelName, id: &TaskId) -> String {
    format!("chan.{channel}.task.{id}")
}

pub fn tasks_prefix(channel: &ChannelName) -> String {
    format!("chan.{channel}.task.")
}

/// One stored wait of an agent on a task state, by the id that the agent
/// got back. The hub deletes it when it wakes the agent.
pub fn wait(agent: &AgentId, id: &str) -> String {
    format!("wait.{agent}.{id}")
}

pub const WAIT_PREFIX: &str = "wait.";

/// Bus subjects.
pub mod subjects {
    use super::{AgentId, ChannelName, HostName};

    /// Requests from host daemons and the CLI to the hub daemon.
    pub const CTL: &str = "borant.ctl";

    pub fn agent(id: &AgentId) -> String {
        format!("borant.agent.{id}")
    }

    pub fn broadcast(channel: &ChannelName) -> String {
        format!("borant.chan.{channel}.all")
    }

    pub fn tasks(channel: &ChannelName) -> String {
        format!("borant.chan.{channel}.tasks")
    }

    /// All task events, for host notifiers.
    pub const ALL_TASKS: &str = "borant.chan.*.tasks";

    /// The swarm event stream, one subject for each kind, for example
    /// `borant.events.lease.expired`.
    pub fn events(kind: &str) -> String {
        format!("borant.events.{kind}")
    }

    /// Commands from the hub to one host daemon. Not durable.
    pub fn host_cmd(host: &HostName) -> String {
        format!("borant.host.{host}.cmd")
    }

    /// The inbox subjects of an agent: its direct subject plus the broadcast
    /// and task subjects of each channel.
    pub fn inbox(agent: &AgentId, channels: &[ChannelName]) -> Vec<String> {
        let mut out = vec![self::agent(agent)];
        for c in channels {
            out.push(broadcast(c));
            out.push(tasks(c));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let api: ChannelName = "api".parse().unwrap();
        let id = TaskId::new();
        assert_eq!(task(&api, &id), format!("chan.api.task.{id}"));
        assert_eq!(subjects::broadcast(&api), "borant.chan.api.all");
        let a: AgentId = "w1".parse().unwrap();
        assert_eq!(member(&api, &a), "chan.api.member.w1");
        assert_eq!(
            subjects::inbox(&a, &[api]),
            vec![
                "borant.agent.w1",
                "borant.chan.api.all",
                "borant.chan.api.tasks"
            ]
        );
    }

    /// The keys that the KV refuses, which a caller must not build.
    #[test]
    fn valid_key_refuses_the_shapes_the_store_refuses() {
        assert!(valid_key("alert.host_offline:vps-1"));
        for key in ["", ".", "alert.", ".alert", "alert..x", "a b", "a*", "a>"] {
            assert!(!valid_key(key), "{key:?} is refused");
        }
    }
}
