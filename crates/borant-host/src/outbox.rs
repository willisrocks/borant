//! A local queue for writes while the hub is unreachable (spec section 16).
//! It is a SQLite file, so it works with any broker adapter.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use borant_core::events::{EventFilter, Severity, SwarmEvent, SwarmEventBody, kind_of};
use borant_core::ports::PortErrorKind;
use borant_core::proto::ApiError;
use borant_core::{AgentId, Timestamp};
use futures::stream::BoxStream;
use rusqlite::{Connection, params};

use crate::Host;

/// How long one rate-limited event covers, per agent and kind.
const EVENT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxKind {
    /// A message: `target` is the subject, `payload` is the envelope JSON.
    Publish,
    /// A session event: `target` is the agent ID, `payload` is the event JSON.
    Append,
    /// A swarm event: `target` is the kind, `payload` is the event JSON.
    Event,
}

impl OutboxKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Append => "append",
            Self::Event => "event",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "publish" => Some(Self::Publish),
            "append" => Some(Self::Append),
            "event" => Some(Self::Event),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub kind: OutboxKind,
    pub target: String,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct Outbox(Mutex<Connection>);

impl Outbox {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open outbox {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS outbox (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL,
                 target TEXT NOT NULL,
                 payload BLOB NOT NULL
             );",
        )?;
        Ok(Self(Mutex::new(conn)))
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, kind: OutboxKind, target: &str, payload: &[u8]) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO outbox (kind, target, payload) VALUES (?1, ?2, ?3)",
            params![kind.as_str(), target, payload],
        )?;
        Ok(())
    }

    /// The oldest `n` items, in order.
    pub fn peek(&self, n: usize) -> anyhow::Result<Vec<OutboxItem>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, kind, target, payload FROM outbox ORDER BY id LIMIT ?1")?;
        let rows = stmt.query_map(params![n as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, target, payload) = row?;
            if let Some(kind) = OutboxKind::parse(&kind) {
                out.push(OutboxItem {
                    id,
                    kind,
                    target,
                    payload,
                });
            }
        }
        Ok(out)
    }

    pub fn remove(&self, id: i64) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn len(&self) -> anyhow::Result<usize> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }
}

impl Host {
    /// Publishes a swarm event. When the log is unreachable, the event waits
    /// in the outbox, so an event of an outage arrives with the next flush.
    pub(crate) async fn emit_event(&self, event: SwarmEvent) {
        let kind = kind_of(&event);
        let Some(log) = &self.inner.events else {
            tracing::debug!(kind, "no event log, the event is dropped");
            return;
        };
        match log.publish(&event).await {
            Ok(()) => {}
            Err(e) if matches!(e.kind, PortErrorKind::Unavailable | PortErrorKind::Timeout) => {
                match serde_json::to_vec(&event) {
                    Ok(payload) => {
                        if let Err(e) = self.inner.outbox.push(OutboxKind::Event, kind, &payload) {
                            tracing::error!(kind, "outbox write failed: {e}");
                        }
                    }
                    Err(e) => tracing::error!(kind, "encode the event: {e}"),
                }
            }
            Err(e) => tracing::warn!(kind, "cannot publish the event: {e}"),
        }
    }

    /// One event per agent and kind per minute, as the observability doc
    /// asks for `delivery.*`. `body` gets the count of the occurrences since
    /// the last event of that kind for the agent.
    pub(crate) async fn delivery_event(
        &self,
        agent: &AgentId,
        kind: &'static str,
        body: impl FnOnce(u64) -> SwarmEventBody,
    ) {
        let now = self.inner.clock.now();
        let Some(count) = self.event_due(agent, kind, now) else {
            return;
        };
        let event = SwarmEvent::host(now, &self.inner.cfg.name, Severity::Warn, body(count))
            .about_agent(agent.clone());
        self.emit_event(event).await;
    }

    /// The count to report, when this occurrence of `kind` for `agent` is the
    /// one that its minute allows. A suppressed occurrence is counted for the
    /// next event.
    pub(crate) fn event_due(
        &self,
        agent: &AgentId,
        kind: &'static str,
        now: Timestamp,
    ) -> Option<u64> {
        let mut st = self.state();
        match st.event_limited.get_mut(&(agent.clone(), kind)) {
            Some((at, pending)) if now.since(*at) < EVENT_WINDOW => {
                *pending += 1;
                None
            }
            Some((at, pending)) => {
                let count = *pending + 1;
                *at = now;
                *pending = 0;
                Some(count)
            }
            None => {
                st.event_limited.insert((agent.clone(), kind), (now, 0));
                Some(1)
            }
        }
    }

    /// The swarm events that match `filter`, oldest first. With `follow` the
    /// stream stays open for the events published from now on.
    pub(crate) async fn event_stream(
        &self,
        filter: &EventFilter,
        follow: bool,
    ) -> Result<BoxStream<'static, SwarmEvent>, ApiError> {
        let Some(log) = &self.inner.events else {
            return Err(ApiError::unavailable("this host daemon has no event log"));
        };
        log.read(filter, follow).await.map_err(Into::into)
    }
}

/// A host with an event log on `clock`, for the event tests of this crate.
#[cfg(test)]
pub(crate) fn test_host(
    log: std::sync::Arc<borant_memory::MemoryEventLog>,
    clock: std::sync::Arc<borant_core::ManualClock>,
) -> Host {
    use std::sync::Arc;

    use borant_core::model::Harness;
    use borant_launch::ProcessLauncher;
    use borant_memory::{MemoryBus, MemorySession};

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = crate::HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    Host::new(
        cfg,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        clock,
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        None,
        Some(log),
        Outbox::in_memory().unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use borant_core::ports::EventLog;
    use borant_core::{Clock, ManualClock, MessageId};
    use borant_memory::MemoryEventLog;
    use futures::StreamExt;

    /// The events of the log, oldest first.
    async fn published(log: &MemoryEventLog) -> Vec<SwarmEvent> {
        log.read(&EventFilter::default(), false)
            .await
            .unwrap()
            .collect()
            .await
    }

    fn ack_failed(count: u64) -> SwarmEventBody {
        SwarmEventBody::DeliveryAckFailed {
            message: MessageId::new(),
            error: "the bus is down".into(),
            count,
        }
    }

    fn agent(name: &str) -> AgentId {
        name.parse().unwrap()
    }

    #[tokio::test]
    async fn delivery_events_are_one_a_minute_for_an_agent_and_kind() {
        let log = Arc::new(MemoryEventLog::new());
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let h = test_host(log.clone(), clock.clone());
        let w = agent("w1");
        for _ in 0..3 {
            h.delivery_event(&w, "delivery.ack_failed", ack_failed)
                .await;
        }
        assert_eq!(published(&log).await.len(), 1, "one event in the minute");
        clock.advance(Duration::from_secs(10));
        h.delivery_event(&w, "delivery.ack_failed", ack_failed)
            .await;
        assert_eq!(
            published(&log).await.len(),
            1,
            "the same minute stays quiet"
        );
        // Another agent, and another kind, are separate minutes.
        h.delivery_event(&agent("w2"), "delivery.ack_failed", ack_failed)
            .await;
        h.delivery_event(&w, "delivery.wake_limited", |count| {
            SwarmEventBody::DeliveryWakeLimited {
                message: MessageId::new(),
                count,
            }
        })
        .await;
        assert_eq!(published(&log).await.len(), 3);
        // The next minute allows one more event, and it counts the four
        // occurrences of that kind for the agent.
        clock.advance(Duration::from_secs(60));
        h.delivery_event(&w, "delivery.ack_failed", ack_failed)
            .await;
        let events = published(&log).await;
        assert_eq!(events.len(), 4);
        let counts: Vec<u64> = events
            .iter()
            .filter_map(|e| match &e.body {
                SwarmEventBody::DeliveryAckFailed { count, .. } => Some(*count),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![1, 1, 4]);
        assert_eq!(events[3].agent, Some(w));
    }

    #[tokio::test]
    async fn events_survive_a_bus_outage_in_the_outbox() {
        let log = Arc::new(MemoryEventLog::new());
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let h = test_host(log.clone(), clock.clone());
        log.set_available(false);
        let at = clock.now();
        let event = SwarmEvent::host(
            at,
            &"h1".parse().unwrap(),
            Severity::Warn,
            SwarmEventBody::SessionPaused {
                why: "the hub is down".into(),
            },
        )
        .about_agent(agent("w1"));
        h.emit_event(event.clone()).await;
        assert_eq!(h.inner.outbox.len().unwrap(), 1, "the event waits");
        log.set_available(true);
        h.flush_outbox().await;
        let events = published(&log).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, event.id);
        assert_eq!(h.inner.outbox.len().unwrap(), 0);
    }

    #[test]
    fn keeps_order_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.db");
        {
            let o = Outbox::open(&path).unwrap();
            o.push(OutboxKind::Publish, "borant.agent.a", b"1").unwrap();
            o.push(OutboxKind::Append, "a", b"2").unwrap();
        }
        let o = Outbox::open(&path).unwrap();
        let items = o.peek(10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, OutboxKind::Publish);
        assert_eq!(items[1].payload, b"2");
        o.remove(items[0].id).unwrap();
        assert_eq!(o.len().unwrap(), 1);
    }
}
