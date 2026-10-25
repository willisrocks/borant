use async_nats::jetstream;
use async_nats::jetstream::consumer::{DeliverPolicy, ReplayPolicy, push};
use async_nats::jetstream::kv::{self, CreateErrorKind, Operation, UpdateErrorKind};
use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::OnceCell;

use crate::{KV_BUCKET, unavailable};

#[derive(Debug, Clone)]
pub struct NatsState {
    client: async_nats::Client,
    js: jetstream::Context,
    store: std::sync::Arc<OnceCell<kv::Store>>,
}

impl NatsState {
    pub(crate) fn new(client: async_nats::Client, js: jetstream::Context) -> Self {
        Self {
            client,
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

/// Turns a delivered KV message into an entry, and the number of messages the
/// consumer still has pending. The last message of a key can be a delete or
/// purge marker, which holds no value.
fn entry_from_message(
    prefix: &str,
    message: jetstream::Message,
) -> PortResult<(Option<Entry>, u64)> {
    let info = message
        .info()
        .map_err(|e| PortError::other(format!("KV list metadata: {e}")))?;
    let pending = info.pending;
    let mut out = None;
    if let Some(headers) = message.headers.as_ref() {
        let removed = headers
            .get("KV-Operation")
            .is_some_and(|op| matches!(op.as_str(), "DEL" | "PURGE"))
            || headers.get("NATS-Marker-Reason").is_some();
        if removed {
            return Ok((None, pending));
        }
    }
    if let Some(key) = message.subject.strip_prefix(prefix) {
        out = Some(Entry {
            key: key.to_owned(),
            value: message.payload.clone(),
            revision: info.stream_sequence,
        });
    }
    Ok((out, pending))
}

#[async_trait::async_trait]
impl StateStore for NatsState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        let store = self.store().await?;
        let entry = store
            .entry(key)
            .await
            .map_err(|e| unavailable(&format!("KV get {key}"), e))?;
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
            _ => unavailable(&format!("KV create {key}"), e),
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
                _ => unavailable(&format!("KV update {key}"), e),
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
                    unavailable(&format!("KV delete {key}"), e)
                }
            })
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        let store = self.store().await?;
        // One consumer filtered to the prefix delivers the last message of each
        // key under it, values included, so the cost is the keys under the
        // prefix rather than every key in the bucket plus one get per key.
        // A NATS subject filter matches whole tokens, so a prefix that ends
        // inside a token is filtered on its last whole token here and finished
        // on the client below.
        let whole = prefix.rfind('.').map(|i| i + 1).unwrap_or(0);
        let scope = &prefix[..whole];
        let filter = format!("{}{scope}>", store.prefix);
        let consumer = store
            .stream
            .create_consumer(push::OrderedConfig {
                deliver_subject: self.client.new_inbox(),
                description: Some(format!("kv list {prefix}")),
                filter_subject: filter,
                replay_policy: ReplayPolicy::Instant,
                deliver_policy: DeliverPolicy::LastPerSubject,
                ..Default::default()
            })
            .await
            .map_err(|e| unavailable(&format!("KV list {prefix}"), e))?;
        // The consumer can deliver before any info round trip, so the count of
        // pending messages is not the number of entries. Read until the last
        // message says nothing is pending, like the KV `keys` stream does.
        if consumer.cached_info().num_pending == 0 {
            return Ok(Vec::new());
        }
        let mut messages = consumer
            .messages()
            .await
            .map_err(|e| unavailable(&format!("KV list {prefix}"), e))?;
        let mut out = Vec::new();
        loop {
            let Some(message) = messages.next().await else {
                break;
            };
            let message = message.map_err(|e| unavailable(&format!("KV list {prefix}"), e))?;
            let (entry, pending) = entry_from_message(&store.prefix, message)?;
            if let Some(entry) = entry.filter(|e| e.key.starts_with(prefix)) {
                out.push(entry);
            }
            if pending == 0 {
                break;
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        let store = self.store().await?;
        let w = if prefix.is_empty() {
            store.watch_all().await
        } else {
            store.watch(format!("{prefix}>")).await
        }
        .map_err(|e| unavailable(&format!("KV watch {prefix}"), e))?;
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
