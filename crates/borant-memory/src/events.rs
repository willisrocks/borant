//! An in-memory swarm event log, for tests and for the fixture of the
//! daemons.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use borant_core::events::{EventFilter, SwarmEvent};
use borant_core::ports::{EventLog, PortResult};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::Availability;

/// A log that keeps its events in memory, oldest first, and fans out new ones
/// to the readers that follow.
#[derive(Debug, Clone)]
pub struct MemoryEventLog {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    events: Mutex<Vec<SwarmEvent>>,
    tx: broadcast::Sender<SwarmEvent>,
    available: Availability,
}

impl Default for MemoryEventLog {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryEventLog {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                events: Mutex::new(Vec::new()),
                tx: broadcast::channel(1024).0,
                available: Availability::default(),
            }),
        }
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    fn events(&self) -> std::sync::MutexGuard<'_, Vec<SwarmEvent>> {
        self.inner.events.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait::async_trait]
impl EventLog for MemoryEventLog {
    async fn publish(&self, event: &SwarmEvent) -> PortResult<()> {
        self.inner.available.check()?;
        {
            let mut events = self.events();
            if events.iter().any(|e| e.id == event.id) {
                return Ok(());
            }
            events.push(event.clone());
            // Oldest first. A stable sort keeps the order of equal times.
            events.sort_by_key(|e| e.at);
        }
        // A reader that is gone is not an error: the event is stored.
        let _ = self.inner.tx.send(event.clone());
        Ok(())
    }

    async fn read(
        &self,
        filter: &EventFilter,
        follow: bool,
    ) -> PortResult<BoxStream<'static, SwarmEvent>> {
        self.inner.available.check()?;
        // Subscribe before the snapshot, so an event published in between is
        // not lost; the ids of the snapshot keep it from arriving twice.
        let subscription = self.inner.tx.subscribe();
        let seen: HashSet<_> = self
            .events()
            .iter()
            .filter(|e| filter.matches(e))
            .map(|e| e.id)
            .collect();
        let snapshot: Vec<SwarmEvent> = self
            .events()
            .iter()
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        let past = futures::stream::iter(snapshot);
        if !follow {
            return Ok(past.boxed());
        }
        let filter = filter.clone();
        let seen = Arc::new(Mutex::new(seen));
        let live = BroadcastStream::new(subscription)
            .filter_map(|item| futures::future::ready(item.ok()))
            .filter(move |event| {
                let keep = {
                    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
                    filter.matches(event) && seen.insert(event.id)
                };
                futures::future::ready(keep)
            });
        Ok(past.chain(live).boxed())
    }
}
