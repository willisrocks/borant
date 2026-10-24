//! Borant domain model, rules, and ports. This crate does no I/O.

pub mod error;
pub mod ids;
pub mod keys;
pub mod model;
pub mod rules;
pub mod time;

pub use error::CoreError;
pub use ids::{
    AgentId, ChannelName, EventId, FencingToken, HostName, MessageId, SessionId, TaskId,
};
pub use keys::subjects;
pub use time::{Clock, ManualClock, SystemClock, Timestamp};
