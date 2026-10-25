//! The event log: one durable stream of swarm events, read by filters.

use futures::stream::BoxStream;

use crate::events::{EventFilter, SwarmEvent};
use crate::ports::PortResult;

/// The swarm event stream: the hub and the hosts write, readers follow.
#[async_trait::async_trait]
pub trait EventLog: Send + Sync + std::fmt::Debug {
    /// Stores one event. Publishing the same event ID twice stores it once.
    async fn publish(&self, event: &SwarmEvent) -> PortResult<()>;

    /// The events that match `filter`, oldest first. With `follow`, the
    /// stream then waits for events published from now on, and ends when the
    /// reader drops it.
    async fn read(
        &self,
        filter: &EventFilter,
        follow: bool,
    ) -> PortResult<BoxStream<'static, SwarmEvent>>;
}
