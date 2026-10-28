//! In-memory adapters for the Borant ports. Tests use them, and each one has
//! `set_available(false)` to act as an unreachable backend.

mod bus;
mod changes;
mod events;
mod issues;
mod provision;
mod session;
mod state;

pub use bus::MemoryBus;
pub use changes::MemoryChanges;
pub use events::MemoryEventLog;
pub use issues::{IssueCall, MemoryIssues};
pub use provision::{Call, MemoryProvisioner};
pub use session::MemorySession;
pub use state::MemoryState;

use std::sync::atomic::{AtomicBool, Ordering};

use borant_core::ports::{PortError, PortResult};

#[derive(Debug)]
struct Availability(AtomicBool);

impl Default for Availability {
    fn default() -> Self {
        Self(AtomicBool::new(true))
    }
}

impl Availability {
    fn set(&self, on: bool) {
        self.0.store(on, Ordering::SeqCst);
    }

    fn check(&self) -> PortResult<()> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(PortError::unavailable(
                "memory backend is set to unavailable",
            ))
        }
    }
}
