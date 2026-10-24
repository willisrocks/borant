use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use borant_core::AgentId;
use borant_core::model::SessionEvent;
use borant_core::ports::{PortResult, SessionStore};

use crate::Availability;

type Logs = HashMap<AgentId, Vec<(u64, SessionEvent)>>;

#[derive(Debug, Clone, Default)]
pub struct MemorySession {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    logs: Mutex<(u64, Logs)>,
    available: Availability,
}

impl MemorySession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    fn with<R>(
        &self,
        f: impl FnOnce(&mut u64, &mut HashMap<AgentId, Vec<(u64, SessionEvent)>>) -> R,
    ) -> R {
        let mut g = self.inner.logs.lock().unwrap_or_else(|e| e.into_inner());
        let (seq, map) = &mut *g;
        f(seq, map)
    }
}

#[async_trait::async_trait]
impl SessionStore for MemorySession {
    async fn append(&self, agent: &AgentId, ev: &SessionEvent) -> PortResult<u64> {
        self.inner.available.check()?;
        Ok(self.with(|seq, map| {
            *seq += 1;
            map.entry(agent.clone())
                .or_default()
                .push((*seq, ev.clone()));
            *seq
        }))
    }

    async fn read(&self, agent: &AgentId) -> PortResult<Vec<(u64, SessionEvent)>> {
        self.inner.available.check()?;
        Ok(self.with(|_, map| map.get(agent).cloned().unwrap_or_default()))
    }

    async fn trim_before(&self, agent: &AgentId, seq: u64) -> PortResult<()> {
        self.inner.available.check()?;
        self.with(|_, map| {
            if let Some(v) = map.get_mut(agent) {
                v.retain(|(s, _)| *s >= seq);
            }
        });
        Ok(())
    }

    async fn delete(&self, agent: &AgentId) -> PortResult<()> {
        self.inner.available.check()?;
        self.with(|_, map| map.remove(agent));
        Ok(())
    }

    async fn agents(&self) -> PortResult<Vec<AgentId>> {
        self.inner.available.check()?;
        Ok(self.with(|_, map| {
            let mut v: Vec<_> = map
                .iter()
                .filter(|(_, e)| !e.is_empty())
                .map(|(a, _)| a.clone())
                .collect();
            v.sort();
            v
        }))
    }
}
