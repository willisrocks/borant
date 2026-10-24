use async_nats::jetstream;
use async_nats::jetstream::kv::{self, CreateErrorKind, Operation, UpdateErrorKind};
use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::OnceCell;

use crate::{KV_BUCKET, unavailable};

#[derive(Debug, Clone)]
pub struct NatsState {
    js: jetstream::Context,
    store: std::sync::Arc<OnceCell<kv::Store>>,
}

impl NatsState {
    pub(crate) fn new(js: jetstream::Context) -> Self {
        Self {
            js,
            store: Default::default(),
        }
    }

    async fn store(&self) -> PortResult<&kv::Store> {
        self.store
            .get_or_try_init(|| async {
                match self.js.get_key_value(KV_BUCKET).await {
                    Ok(s) => Ok(s),
                    Err(_) => self
                        .js
                        .create_key_value(kv::Config {
                            bucket: KV_BUCKET.to_owned(),
                            history: 1,
                            ..Default::default()
                        })
                        .await
                        .map_err(|e| unavailable("create KV bucket", e)),
                }
            })
            .await
    }
}

fn is_wrong_revision(e: &impl std::fmt::Display) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("wrong last sequence") || s.contains("10071")
}

#[async_trait::async_trait]
impl StateStore for NatsState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        let store = self.store().await?;
        let entry = store
            .entry(key)
            .await
            .map_err(|e| unavailable("KV get", e))?;
        Ok(entry
            .filter(|e| e.operation == Operation::Put)
            .map(|e| Entry {
                key: e.key,
                value: e.value,
                revision: e.revision,
            }))
    }

    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64> {
        let store = self.store().await?;
        store.create(key, value).await.map_err(|e| match e.kind() {
            CreateErrorKind::AlreadyExists => PortError::conflict(format!("key {key} exists")),
            CreateErrorKind::InvalidKey => PortError::other(format!("invalid key {key}")),
            _ => unavailable("KV create", e),
        })
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        let store = self.store().await?;
        store
            .update(key, value, expected)
            .await
            .map_err(|e| match e.kind() {
                UpdateErrorKind::WrongLastRevision => {
                    PortError::conflict(format!("key {key} is not at revision {expected}"))
                }
                _ if is_wrong_revision(&e) => {
                    PortError::conflict(format!("key {key} is not at revision {expected}"))
                }
                UpdateErrorKind::InvalidKey => PortError::other(format!("invalid key {key}")),
                _ => unavailable("KV update", e),
            })
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        let store = self.store().await?;
        if expected.is_none() && self.get(key).await?.is_none() {
            return Ok(());
        }
        store
            .delete_expect_revision(key, expected)
            .await
            .map_err(|e| {
                if matches!(e.kind(), UpdateErrorKind::WrongLastRevision) || is_wrong_revision(&e) {
                    PortError::conflict(format!("key {key} is not at revision {expected:?}"))
                } else {
                    unavailable("KV delete", e)
                }
            })
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        let store = self.store().await?;
        let mut keys: Vec<String> = match store.keys().await {
            Ok(k) => {
                k.filter_map(|k| futures::future::ready(k.ok()))
                    .filter(|k| futures::future::ready(k.starts_with(prefix)))
                    .collect()
                    .await
            }
            Err(e) if e.to_string().to_lowercase().contains("no keys") => Vec::new(),
            Err(e) => return Err(unavailable("KV keys", e)),
        };
        keys.sort();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(e) = self.get(&k).await? {
                out.push(e);
            }
        }
        Ok(out)
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        let store = self.store().await?;
        let w = if prefix.is_empty() {
            store.watch_all().await
        } else {
            store.watch(format!("{prefix}>")).await
        }
        .map_err(|e| unavailable("KV watch", e))?;
        Ok(w.filter_map(|e| {
            futures::future::ready(e.ok().map(|e| match e.operation {
                Operation::Put => Change::Put(Entry {
                    key: e.key,
                    value: e.value,
                    revision: e.revision,
                }),
                Operation::Delete | Operation::Purge => Change::Delete { key: e.key },
            }))
        })
        .boxed())
    }
}
