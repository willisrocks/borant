//! An in-memory change-request source, for tests. A test seeds the status of
//! a URL, and the adapter serves it or reports `NotFound`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use borant_core::ports::changes::{ChangeRequests, ChangeStatus};
use borant_core::ports::{PortError, PortResult};

use crate::Availability;

#[derive(Debug, Clone)]
pub struct MemoryChanges {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    statuses: Mutex<BTreeMap<String, ChangeStatus>>,
    available: Availability,
}

impl Default for MemoryChanges {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryChanges {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                statuses: Mutex::new(BTreeMap::new()),
                available: Availability::default(),
            }),
        }
    }

    /// Serves `status` for `url`, as a forge would.
    pub fn set(&self, url: impl Into<String>, status: ChangeStatus) {
        self.inner
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.into(), status);
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }
}

#[async_trait::async_trait]
impl ChangeRequests for MemoryChanges {
    async fn status(&self, url: &str) -> PortResult<ChangeStatus> {
        self.inner.available.check()?;
        self.inner
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .cloned()
            .ok_or_else(|| PortError::not_found(format!("change {url}")))
    }
}
