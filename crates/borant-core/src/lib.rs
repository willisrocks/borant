//! Borant domain model, rules, and ports. This crate does no I/O.

pub mod error;
pub mod ids;
pub mod keys;
pub mod model;
pub mod ports;
pub mod proto;
pub mod rules;
#[cfg(feature = "testing")]
pub mod testing;
pub mod time;

pub use error::CoreError;
pub use ids::{
    AgentId, BundleName, ChannelName, EventId, FencingToken, HostName, MemberName, MessageId,
    PoolName, ProjectName, SessionId, Tag, TaskId,
};
pub use keys::subjects;
pub use time::{Clock, ManualClock, SystemClock, Timestamp};
