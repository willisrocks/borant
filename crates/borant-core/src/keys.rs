//! The state-store key layout (spec section 6) and the bus subjects (spec
//! section 10).

use crate::ids::{AgentId, ChannelName, HostName, ProjectName, SessionId, TaskId};

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
}
