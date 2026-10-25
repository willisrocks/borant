//! Port traits. Adapters implement them. The traits use only domain types,
//! bytes, and strings, so a new backend needs no change in the core.

mod bus;
mod launch;
mod notify;
pub mod secrets;
mod session;
mod state;

pub use bus::{Acker, Delivery, Incoming, MessageBus, Responder, subject_matches};
pub use launch::{LaunchHandle, LaunchSpec, Launcher};
pub use notify::Notifier;
pub use secrets::{Secret, SecretRef, SecretStore};
pub use session::SessionStore;
pub use state::{Change, Entry, StateStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortErrorKind {
    NotFound,
    /// A create of an existing key, or an update with a wrong revision.
    Conflict,
    /// The backend cannot be reached.
    Unavailable,
    Timeout,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct PortError {
    pub kind: PortErrorKind,
    pub message: String,
}

impl PortError {
    pub fn new(kind: PortErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(PortErrorKind::Conflict, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(PortErrorKind::NotFound, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(PortErrorKind::Unavailable, message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(PortErrorKind::Other, message)
    }

    pub fn is(&self, kind: PortErrorKind) -> bool {
        self.kind == kind
    }
}

pub type PortResult<T> = Result<T, PortError>;
