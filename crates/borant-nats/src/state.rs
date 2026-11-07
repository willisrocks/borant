use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::{AckPolicy, Consumer, DeliverPolicy, pull};
use async_nats::jetstream::kv::{self, CreateErrorKind, Operation, UpdateErrorKind};
use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{Mutex, OnceCell};

use crate::{KV_BUCKET, unavailable};

/// The description of the consumer that feeds the cache, so it is
/// recognizable on the server.
pub const STATE_CACHE_CONSUMER: &str = "borant state cache";
/// How many messages one fetch asks for while the cache catches up.
const DRAIN_BATCH: usize = 256;
/// How long the server keeps the consumer after the last pull. A live daemon
/// pulls it on every list; a process that died leaves one consumer that the
/// server removes after this. The normal end deletes it in `Drop`.
const INACTIVE: Duration = Duration::from_secs(3600);

/// The last message seen for one key: the stream sequence and the value, or
/// `None` when the last message was a delete marker.
type Seen = (u64, Option<Bytes>);

/// The last message seen for every key of the bucket, and the one consumer
/// that keeps it current.
///
/// `list` must not create a consumer per call. The hub lists leases, agents
/// and hosts on a one-second tick, and an ephemeral consumer per call made
/// the server create and delete consumers every second and move thousands of
/// messages a second. Instead this cache is primed once with the last message
/// per key and then follows the stream, and `list` reads the map.
///
/// Read-after-write holds: a write through this store records its own
/// revision in the map before it returns. A write through another process
/// arrives on the next list, because every list drains the consumer before
/// it reads the map. Each entry keeps the stream sequence it was seen at, so
/// a message that is older than what the map holds (a local write raced the
/// delivery) never replaces it.
#[derive(Debug)]
struct Cache {
    js: jetstream::Context,
    entries: HashMap<String, Seen>,
    consumer: Option<Consumer<pull::Config>>,
}

impl Drop for Cache {
    fn drop(&mut self) {
        // Delete the consumer, so a restarted daemon does not leave one
        // behind for the whole inactivity threshold.
        let Some(consumer) = self.consumer.take() else {
            return;
        };
        let info = consumer.cached_info();
        let (name, stream) = (info.name.clone(), info.stream_name.clone());
        let js = self.js.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            if let Err(e) = js.delete_consumer_from_stream(name, stream).await {
                tracing::debug!(error = %e, "could not delete the state cache consumer");
            }
        });
    }
}

#[derive(Debug, Clone)]
pub struct NatsState {
    js: jetstream::Context,
    store: Arc<OnceCell<kv::Store>>,
    cache: Arc<Mutex<Cache>>,
}

impl NatsState {
    pub(crate) fn new(js: jetstream::Context) -> Self {
        let cache = Cache {
            js: js.clone(),
            entries: HashMap::new(),
            consumer: None,
        };
        Self {
            js,
            store: Default::default(),
            cache: Arc::new(Mutex::new(cache)),
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

    /// Records a write of this process before the consumer delivers it, so
    /// the next `list` of this process sees it.
    async fn remember(&self, key: &str, revision: u64, value: Option<Bytes>) {
        let mut cache = self.cache.lock().await;
        cache.entries.insert(key.to_owned(), (revision, value));
    }
}

fn is_wrong_revision(e: &impl std::fmt::Display) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("wrong last sequence") || s.contains("10071")
}

/// The one consumer of the cache: the last message per key, then every new
/// message, over the whole bucket. `DeliverPolicy::LastPerSubject` primes the
/// map in one pass, and the consumer stays for the next list.
async fn create_consumer(store: &kv::Store) -> PortResult<Consumer<pull::Config>> {
    store
        .stream
        .create_consumer(pull::Config {
            description: Some(STATE_CACHE_CONSUMER.to_owned()),
            filter_subject: format!("{}>", store.prefix),
            deliver_policy: DeliverPolicy::LastPerSubject,
            // The consumer keeps its position by acking. A message that is
            // not acked comes back after the ack wait, which the revision
            // check makes harmless.
            ack_policy: AckPolicy::Explicit,
            inactive_threshold: INACTIVE,
            ..Default::default()
        })
        .await
        .map_err(|e| unavailable("create state cache consumer", e))
}

/// True when an error says the consumer no longer exists on the server, which
/// happens when an ephemeral consumer stayed idle past its threshold.
fn consumer_gone(e: &PortError) -> bool {
    let s = e.message.to_lowercase();
    s.contains("consumer not found")
        || s.contains("consumer deleted")
        || s.contains("does not exist")
}

/// Reads every message the consumer has ready into the map, and returns when
/// nothing is left. A fetch of a pull consumer returns at once when the
/// consumer has nothing for it, so the first empty batch ends the drain.
async fn drain(
    consumer: &Consumer<pull::Config>,
    prefix: &str,
    entries: &mut HashMap<String, Seen>,
) -> PortResult<()> {
    loop {
        let mut batch = consumer
            .fetch()
            .max_messages(DRAIN_BATCH)
            .messages()
            .await
            .map_err(|e| unavailable("sync state cache", e))?;
        // The count of pending messages of the last message of the batch: 0
        // means the consumer has nothing left to deliver.
        let mut pending = None;
        while let Some(message) = batch.next().await {
            let message = message.map_err(|e| unavailable("sync state cache", e))?;
            pending = Some(apply(prefix, &message, entries)?);
            message
                .ack()
                .await
                .map_err(|e| unavailable("ack state cache message", e))?;
        }
        match pending {
            // The batch was empty.
            None => return Ok(()),
            Some(0) => return Ok(()),
            Some(_) => {}
        }
    }
}

/// Applies one message of the KV stream to the map, and returns the number of
/// messages the consumer still has to deliver. A message older than the one
/// the map holds (a local write raced its delivery) is skipped. The last
/// message of a key can be a delete or purge marker, which holds no value.
fn apply(
    prefix: &str,
    message: &jetstream::Message,
    entries: &mut HashMap<String, Seen>,
) -> PortResult<u64> {
    let info = message
        .info()
        .map_err(|e| PortError::other(format!("KV list metadata: {e}")))?;
    let revision = info.stream_sequence;
    let pending = info.pending;
    let Some(key) = message.subject.strip_prefix(prefix) else {
        return Ok(pending);
    };
    let removed = message.headers.as_ref().is_some_and(|headers| {
        headers
            .get("KV-Operation")
            .is_some_and(|op| matches!(op.as_str(), "DEL" | "PURGE"))
            || headers.get("NATS-Marker-Reason").is_some()
    });
    if entries.get(key).is_some_and(|(seen, _)| *seen >= revision) {
        return Ok(pending);
    }
    entries.insert(
        key.to_owned(),
        (revision, (!removed).then(|| message.payload.clone())),
    );
    Ok(pending)
}

/// Brings the cache to the stream's latest message, and returns when a
/// consumer the server removed has been replaced: the new one replays the
/// last message per key, which rebuilds the map from scratch.
async fn sync(store: &kv::Store, cache: &mut Cache) -> PortResult<()> {
    let mut remade = false;
    loop {
        let consumer = match cache.consumer.take() {
            Some(consumer) => consumer,
            None => {
                cache.entries.clear();
                create_consumer(store).await?
            }
        };
        match drain(&consumer, &store.prefix, &mut cache.entries).await {
            Ok(()) => {
                cache.consumer = Some(consumer);
                return Ok(());
            }
            Err(e) if !remade && consumer_gone(&e) => {
                // Keep the handle out of the cache: it is gone, and the next
                // pass primes a new consumer.
                remade = true;
            }
            Err(e) => {
                cache.consumer = Some(consumer);
                return Err(e);
            }
        }
    }
}

#[async_trait::async_trait]
impl StateStore for NatsState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        // A key the KV refuses can never be there; asking the server for it
        // would fail the request instead of reporting it absent.
        if !borant_core::keys::valid_key(key) {
            return Ok(None);
        }
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
        let revision = store
            .create(key, value.clone())
            .await
            .map_err(|e| match e.kind() {
                CreateErrorKind::AlreadyExists => PortError::conflict(format!("key {key} exists")),
                CreateErrorKind::InvalidKey => PortError::other(format!("invalid key {key}")),
                _ => unavailable(&format!("KV create {key}"), e),
            })?;
        self.remember(key, revision, Some(value)).await;
        Ok(revision)
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        let store = self.store().await?;
        let revision = store
            .update(key, value.clone(), expected)
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
            })?;
        self.remember(key, revision, Some(value)).await;
        Ok(revision)
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
            })?;
        // A delete marker with a newer sequence follows in the stream; until
        // it arrives, an older put that the consumer had not delivered yet
        // must not put the key back. The drain before the next read applies
        // the marker, and the delete itself removes the key now.
        let mut cache = self.cache.lock().await;
        cache.entries.remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        let store = self.store().await?;
        let mut cache = self.cache.lock().await;
        sync(store, &mut cache).await?;
        // The map holds the whole bucket, so the filter is an exact prefix
        // match here; the old subject filter had to deal with whole tokens.
        let mut out: Vec<Entry> = cache
            .entries
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .filter_map(|(key, (revision, value))| {
                value.as_ref().map(|value| Entry {
                    key: key.clone(),
                    value: value.clone(),
                    revision: *revision,
                })
            })
            .collect();
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
