use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;

use serde::{Deserialize, Serialize};

use super::{PortError, PortErrorKind, PortResult};
use crate::ids::AgentId;
use crate::model::Envelope;

/// What the inbox of one agent holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxStats {
    /// Messages that the inbox has not delivered yet.
    pub pending: u64,
    /// Messages that it delivered and that the agent did not ack.
    pub ack_pending: u64,
    /// Messages that it delivered more than once.
    pub redelivered: u64,
}

#[async_trait::async_trait]
pub trait Acker: Send + Sync {
    async fn ack(self: Box<Self>) -> PortResult<()>;
}

pub struct Delivery {
    pub envelope: Envelope,
    pub acker: Box<dyn Acker>,
}

impl std::fmt::Debug for Delivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Delivery")
            .field("envelope", &self.envelope)
            .finish()
    }
}

#[async_trait::async_trait]
pub trait Responder: Send + Sync {
    async fn respond(self: Box<Self>, payload: Bytes) -> PortResult<()>;
}

pub struct Incoming {
    pub payload: Bytes,
    pub responder: Box<dyn Responder>,
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Incoming")
            .field("payload", &self.payload)
            .finish()
    }
}

/// Messages, events, and requests between daemons.
///
/// Durable messages go to subjects such as `borant.agent.{id}` and
/// `borant.chan.{channel}.all`. Each agent has one durable inbox with a list
/// of subjects. A message stays in the inbox until the agent acks it.
#[async_trait::async_trait]
pub trait MessageBus: Send + Sync + std::fmt::Debug {
    /// Stores the envelope durably on the subject. Publishing the same
    /// envelope ID twice stores it once.
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()>;
    /// Creates the inbox of the agent, or replaces its subject list. A new
    /// inbox gets only messages published after its creation.
    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()>;
    /// Creates the inbox of an agent that lost it, keeping the messages that
    /// arrived while it was gone. An inbox that never existed uses
    /// `ensure_inbox`, which starts from now; the host drops the messages it
    /// logged already by envelope ID.
    async fn recover_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.ensure_inbox(agent, subjects).await
    }
    /// Up to `max` messages, waiting up to `wait` for the first one. A
    /// message that is not acked comes back in a later pull.
    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>>;
    /// The number of messages that the inbox did not deliver yet.
    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64>;
    /// The counts of the inbox of one agent. A bus that cannot report them
    /// fails with `Invalid`, so a caller shows no numbers instead of zeros.
    async fn inbox_stats(&self, agent: &AgentId) -> PortResult<InboxStats> {
        let _ = agent;
        Err(PortError::new(
            PortErrorKind::Invalid,
            "this bus does not report inbox stats",
        ))
    }
    /// All messages in the inbox that are not acked, for an export.
    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>>;
    async fn delete_inbox(&self, agent: &AgentId) -> PortResult<()>;
    /// Envelopes published from now on to subjects that match `pattern`.
    /// `*` matches one token and `>` matches the rest. Not durable.
    async fn subscribe(&self, pattern: &str) -> PortResult<BoxStream<'static, Envelope>>;
    /// Sends a request and waits for one reply.
    async fn request(&self, subject: &str, payload: Bytes, timeout: Duration) -> PortResult<Bytes>;
    /// Requests and sends to `subject`, from now on.
    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>>;
    /// Sends a payload with no reply and no durability.
    async fn send(&self, subject: &str, payload: Bytes) -> PortResult<()>;
}

/// True if a subject matches a pattern with `*` and `>` wildcards.
pub fn subject_matches(pattern: &str, subject: &str) -> bool {
    let mut p = pattern.split('.');
    let mut s = subject.split('.');
    loop {
        match (p.next(), s.next()) {
            (Some(">"), Some(_)) => return true,
            (Some("*"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::subject_matches;

    #[test]
    fn wildcards() {
        assert!(subject_matches(
            "borant.chan.*.tasks",
            "borant.chan.api.tasks"
        ));
        assert!(!subject_matches(
            "borant.chan.*.tasks",
            "borant.chan.api.all"
        ));
        assert!(subject_matches("borant.>", "borant.chan.api.all"));
        assert!(!subject_matches("borant.>", "borant"));
        assert!(subject_matches("a.b", "a.b"));
        assert!(!subject_matches("a.b", "a.b.c"));
    }
}
