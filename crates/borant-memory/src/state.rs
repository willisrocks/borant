use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::Availability;

type Map = BTreeMap<String, (Bytes, u64)>;

#[derive(Debug, Clone)]
pub struct MemoryState {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    map: Mutex<(u64, Map)>,
    tx: broadcast::Sender<Change>,
    available: Availability,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                map: Mutex::new((0, BTreeMap::new())),
                tx: broadcast::channel(1024).0,
                available: Availability::default(),
            }),
        }
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    fn with<R>(&self, f: impl FnOnce(&mut u64, &mut BTreeMap<String, (Bytes, u64)>) -> R) -> R {
        let mut g = self.inner.map.lock().unwrap_or_else(|e| e.into_inner());
        let (rev, map) = &mut *g;
        f(rev, map)
    }
}

#[async_trait::async_trait]
impl StateStore for MemoryState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        self.inner.available.check()?;
        Ok(self.with(|_, map| {
            map.get(key).map(|(v, r)| Entry {
                key: key.to_owned(),
                value: v.clone(),
                revision: *r,
            })
        }))
    }

    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64> {
        self.inner.available.check()?;
        let entry = self.with(|rev, map| {
            if map.contains_key(key) {
                return Err(PortError::conflict(format!("key {key} exists")));
            }
            *rev += 1;
            map.insert(key.to_owned(), (value.clone(), *rev));
            Ok(Entry {
                key: key.to_owned(),
                value,
                revision: *rev,
            })
        })?;
        let r = entry.revision;
        let _ = self.inner.tx.send(Change::Put(entry));
        Ok(r)
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        self.inner.available.check()?;
        let entry = self.with(|rev, map| {
            let current = map.get(key).map(|(_, r)| *r);
            if current != Some(expected) {
                return Err(PortError::conflict(format!(
                    "key {key} has revision {current:?}, not {expected}"
                )));
            }
            *rev += 1;
            map.insert(key.to_owned(), (value.clone(), *rev));
            Ok(Entry {
                key: key.to_owned(),
                value,
                revision: *rev,
            })
        })?;
        let r = entry.revision;
        let _ = self.inner.tx.send(Change::Put(entry));
        Ok(r)
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        self.inner.available.check()?;
        let removed = self.with(|_, map| {
            let current = map.get(key).map(|(_, r)| *r);
            if let Some(e) = expected
                && current != Some(e)
            {
                return Err(PortError::conflict(format!(
                    "key {key} has revision {current:?}, not {e}"
                )));
            }
            Ok(map.remove(key).is_some())
        })?;
        if removed {
            let _ = self.inner.tx.send(Change::Delete {
                key: key.to_owned(),
            });
        }
        Ok(())
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        self.inner.available.check()?;
        Ok(self.with(|_, map| {
            map.range(prefix.to_owned()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, (v, r))| Entry {
                    key: k.clone(),
                    value: v.clone(),
                    revision: *r,
                })
                .collect()
        }))
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        self.inner.available.check()?;
        let prefix = prefix.to_owned();
        let rx = self.inner.tx.subscribe();
        Ok(BroadcastStream::new(rx)
            .filter_map(move |c| {
                let keep = match &c {
                    Ok(Change::Put(e)) => e.key.starts_with(&prefix),
                    Ok(Change::Delete { key }) => key.starts_with(&prefix),
                    Err(_) => false,
                };
                futures::future::ready(if keep { c.ok() } else { None })
            })
            .boxed())
    }
}
