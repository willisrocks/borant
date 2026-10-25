//! The swarm event stream on JetStream (spec "Swarm event stream" in
//! docs/superpowers/specs/2026-10-24-observability-design.md). One stream for
//! every kind, `borant.events.{kind}`, kept for 14 days or 512 MiB, with the
//! event ID in `Nats-Msg-Id`, so a retried publish stores one event.
//!
//! A reader gets its own ephemeral consumer, so it has its own cursor and two
//! readers never take events from each other. An event that does not decode,
//! for example a kind of a newer release, is skipped: it neither ends the
//! reader nor fails it.

use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream;
use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, pull};
use async_nats::jetstream::stream;
use borant_core::events::{EventFilter, SwarmEvent, kind_of};
use borant_core::keys::subjects;
use borant_core::ports::{EventLog, PortError, PortResult};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{OnceCell, mpsc};

use crate::{EVENT_STREAM, unavailable};

/// Every event of every kind.
const EVENT_SUBJECTS: &str = "borant.events.>";

/// The retention of the stream: 14 days or 512 MiB, whichever comes first.
const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);
const MAX_BYTES: i64 = 512 * 1024 * 1024;

/// How long the stream remembers an event ID for dedupe. A host retries its
/// outbox until the hub acks, and an hour of retries is well inside it.
const DUPLICATE_WINDOW: Duration = Duration::from_secs(3600);

/// How long an abandoned reader keeps its consumer. A reader that follows
/// reconnects after a network drop, so it must not expire while it reads.
const READER_IDLE: Duration = Duration::from_secs(120);

/// The events of one pull request.
const BATCH: usize = 256;

/// How long one pull request waits for an event. An empty request means that
/// the reader has caught up.
const FETCH_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct NatsEventLog {
    js: jetstream::Context,
    stream: std::sync::Arc<OnceCell<stream::Stream>>,
}

impl NatsEventLog {
    pub(crate) fn new(js: jetstream::Context) -> Self {
        Self {
            js,
            stream: Default::default(),
        }
    }

    async fn stream(&self) -> PortResult<&stream::Stream> {
        self.stream
            .get_or_try_init(|| async {
                self.js
                    .get_or_create_stream(stream::Config {
                        name: EVENT_STREAM.to_owned(),
                        subjects: vec![EVENT_SUBJECTS.to_owned()],
                        max_age: MAX_AGE,
                        max_bytes: MAX_BYTES,
                        discard: stream::DiscardPolicy::Old,
                        duplicate_window: DUPLICATE_WINDOW,
                        allow_direct: true,
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| unavailable("create event stream", e))
            })
            .await
    }
}

/// The subjects of a filter. No kinds means every event, and an entry that
/// ends in `.*` means every kind below it.
///
/// A kind comes from a reader, so a kind that is not one is `Invalid`.
fn subjects_of(kinds: &[String]) -> PortResult<Vec<String>> {
    if kinds.is_empty() {
        return Ok(vec![EVENT_SUBJECTS.to_owned()]);
    }
    kinds
        .iter()
        .map(|kind| {
            let (name, below) = match kind.strip_suffix(".*") {
                Some(prefix) => (prefix, true),
                None => (kind.as_str(), false),
            };
            if !is_kind(name) {
                return Err(PortError::invalid(format!("{kind} is not an event kind")));
            }
            let subject = if below {
                subjects::events(&format!("{name}.>"))
            } else {
                subjects::events(name)
            };
            Ok(subject)
        })
        .collect()
}

/// True for the characters of an event kind or of a kind prefix of a filter:
/// lowercase letters, digits, `.`, `_`, and `-`, and no leading or trailing
/// dot.
fn is_kind(kind: &str) -> bool {
    !kind.is_empty()
        && !kind.starts_with('.')
        && !kind.ends_with('.')
        && kind.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

#[async_trait::async_trait]
impl EventLog for NatsEventLog {
    async fn publish(&self, event: &SwarmEvent) -> PortResult<()> {
        self.stream().await?;
        let subject = subjects::events(kind_of(event));
        let payload = serde_json::to_vec(event)
            .map_err(|e| PortError::other(format!("encode event: {e}")))?;
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", event.id.to_string().as_str());
        self.js
            .publish_with_headers(subject, headers, payload.into())
            .await
            .map_err(|e| unavailable("publish event", e))?
            .await
            .map_err(|e| unavailable("publish event ack", e))?;
        Ok(())
    }

    async fn read(
        &self,
        filter: &EventFilter,
        follow: bool,
    ) -> PortResult<BoxStream<'static, SwarmEvent>> {
        let stream = self.stream().await?;
        let consumer = stream
            .create_consumer(pull::Config {
                deliver_policy: DeliverPolicy::All,
                ack_policy: AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                filter_subjects: subjects_of(&filter.kinds)?,
                inactive_threshold: READER_IDLE,
                ..Default::default()
            })
            .await
            .map_err(|e| unavailable("create event reader", e))?;
        let filter = filter.clone();
        let (tx, rx) = mpsc::channel(BATCH);
        tokio::spawn(async move {
            read_into(consumer, filter, follow, tx).await;
        });
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })))
    }
}

/// Fills `tx` from the consumer, oldest first. With `follow`, it waits for
/// new events after the ones that are there and ends when the reader drops
/// the stream; without it, it ends when the backlog is read.
async fn read_into(
    consumer: jetstream::consumer::Consumer<pull::Config>,
    filter: EventFilter,
    follow: bool,
    tx: mpsc::Sender<SwarmEvent>,
) {
    loop {
        let mut batch = match consumer
            .fetch()
            .max_messages(BATCH)
            .expires(FETCH_WAIT)
            .messages()
            .await
        {
            Ok(batch) => batch,
            Err(e) => {
                tracing::warn!("read the event stream: {e}");
                return;
            }
        };
        let mut received = 0;
        while let Some(message) = batch.next().await {
            let message = match message {
                Ok(message) => message,
                Err(e) => {
                    tracing::warn!("read the event stream: {e}");
                    return;
                }
            };
            received += 1;
            // The reader is gone: the events of a reader that nobody reads
            // are not an error, and the consumer expires on its own.
            if tx.is_closed() {
                return;
            }
            match serde_json::from_slice::<SwarmEvent>(&message.payload) {
                Ok(event) => {
                    // A filter that keeps the event out is no reason to end
                    // or fail, and neither is the event of a newer release.
                    if filter.matches(&event) && tx.send(event).await.is_err() {
                        return;
                    }
                }
                Err(e) => tracing::debug!("skip an event that does not decode: {e}"),
            }
            // Every message is acked, the skipped ones too, or the server
            // sends them again and again.
            if let Err(e) = message.ack().await {
                tracing::warn!("ack an event: {e}");
            }
        }
        if received == 0 && !follow {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_subjects_read_the_kinds_of_a_filter() {
        let every = subjects_of(&[]).expect("every kind");
        assert_eq!(every, vec!["borant.events.>".to_owned()]);
        let lease = subjects_of(&["lease.*".to_owned(), "hub.started".to_owned()])
            .expect("a kind and a prefix");
        assert_eq!(
            lease,
            vec![
                "borant.events.lease.>".to_owned(),
                "borant.events.hub.started".to_owned()
            ]
        );
        for bad in ["", ".", "lease.", ".lease", "lease expired", "Lease"] {
            assert!(
                subjects_of(&[bad.to_owned()]).is_err(),
                "{bad} is not a kind"
            );
        }
    }
}
