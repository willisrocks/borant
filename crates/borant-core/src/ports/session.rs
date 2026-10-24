use super::PortResult;
use crate::ids::AgentId;
use crate::model::SessionEvent;

/// The neutral session log of each agent.
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    /// Appends an event and returns its sequence number. Sequence numbers
    /// rise for each agent.
    async fn append(&self, agent: &AgentId, ev: &SessionEvent) -> PortResult<u64>;
    /// All events of the agent, oldest first.
    async fn read(&self, agent: &AgentId) -> PortResult<Vec<(u64, SessionEvent)>>;
    /// Removes the events of the agent with a sequence number below `seq`.
    async fn trim_before(&self, agent: &AgentId, seq: u64) -> PortResult<()>;
    async fn delete(&self, agent: &AgentId) -> PortResult<()>;
    /// Agents that have at least one event.
    async fn agents(&self) -> PortResult<Vec<AgentId>>;
}
