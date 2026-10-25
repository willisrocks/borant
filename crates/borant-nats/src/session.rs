use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::{self, pull};
use async_nats::jetstream::stream;
use borant_core::AgentId;
use borant_core::model::SessionEvent;
use borant_core::ports::{PortError, PortResult, SessionStore};
use futures::StreamExt;
use tokio::sync::OnceCell;

use crate::{SESSION_STREAM, unavailable};

/// How many session events one fetch asks for.
const BATCH: usize = 1000;
/// The cap on a fetch that finds fewer events than it counted, which only
/// happens when a trim runs between the count and the fetch.
const FETCH_WAIT: Duration = Duration::from_secs(2);
/// How long the read consumer of an agent stays after its last read.
const READER_IDLE: Duration = Duration::from_secs(60);

fn subject(agent: &AgentId) -> String {
    format!("borant.session.{agent}")
}

#[derive(Debug, Clone)]
pub struct NatsSession {
    js: jetstream::Context,
    stream: std::sync::Arc<OnceCell<stream::Stream>>,
}

impl NatsSession {
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
                        name: SESSION_STREAM.to_owned(),
                        subjects: vec!["borant.session.>".to_owned()],
                        allow_direct: true,
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| unavailable("create session stream", e))
            })
            .await
    }
}

#[async_trait::async_trait]
impl SessionStore for NatsSession {
    async fn append(&self, agent: &AgentId, ev: &SessionEvent) -> PortResult<u64> {
        self.stream().await?;
        let payload =
            serde_json::to_vec(ev).map_err(|e| PortError::other(format!("encode event: {e}")))?;
        // The event ID removes a duplicate when an append is retried.
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", ev.id.to_string().as_str());
        let ack = self
            .js
            .publish_with_headers(subject(agent), headers, payload.into())
            .await
            .map_err(|e| unavailable("append session event", e))?
            .await
            .map_err(|e| unavailable("append session event ack", e))?;
        Ok(ack.sequence)
    }

    async fn read(&self, agent: &AgentId) -> PortResult<Vec<(u64, SessionEvent)>> {
        let stream = self.stream().await?;
        let subject = subject(agent);
        // A pull consumer reads the whole log in batches: one direct get per
        // event costs a round trip each, which stalls a restore on a slow link.
        let mut consumer = stream
            .create_consumer(pull::Config {
                filter_subject: subject.clone(),
                deliver_policy: consumer::DeliverPolicy::All,
                ack_policy: consumer::AckPolicy::None,
                inactive_threshold: READER_IDLE,
                ..Default::default()
            })
            .await
            .map_err(|e| unavailable("create the session reader", e))?;
        let mut out = Vec::new();
        loop {
            // The pending count makes every fetch a whole batch, so the read
            // does not wait for a timeout once the log is drained.
            let pending = consumer
                .info()
                .await
                .map_err(|e| unavailable("count the session log", e))?
                .num_pending;
            if pending == 0 {
                break;
            }
            let want = usize::try_from(pending).unwrap_or(BATCH).min(BATCH);
            let mut batch = consumer
                .fetch()
                .max_messages(want)
                .expires(FETCH_WAIT)
                .messages()
                .await
                .map_err(|e| unavailable("read the session log", e))?;
            let mut received = 0;
            while let Some(message) = batch.next().await {
                let message = message.map_err(|e| unavailable("read the session log", e))?;
                received += 1;
                let seq = message.info().map(|info| info.stream_sequence).unwrap_or(0);
                match serde_json::from_slice::<SessionEvent>(&message.payload) {
                    Ok(ev) => out.push((seq, ev)),
                    Err(e) => tracing::warn!(%agent, seq, "skip bad session event: {e}"),
                }
            }
            // A trim between the count and the fetch ends the batch early.
            if received == 0 {
                break;
            }
        }
        Ok(out)
    }

    async fn trim_before(&self, agent: &AgentId, seq: u64) -> PortResult<()> {
        let stream = self.stream().await?;
        stream
            .purge()
            .filter(subject(agent))
            .sequence(seq)
            .await
            .map_err(|e| unavailable("trim session log", e))?;
        Ok(())
    }

    async fn delete(&self, agent: &AgentId) -> PortResult<()> {
        let stream = self.stream().await?;
        stream
            .purge()
            .filter(subject(agent))
            .await
            .map_err(|e| unavailable("delete session log", e))?;
        Ok(())
    }

    async fn agents(&self) -> PortResult<Vec<AgentId>> {
        let stream = self.stream().await?;
        let mut info = stream
            .info_with_subjects("borant.session.>")
            .await
            .map_err(|e| unavailable("list session subjects", e))?;
        let mut out = Vec::new();
        use futures::StreamExt;
        while let Some(item) = info.next().await {
            let (subj, count) = item.map_err(|e| unavailable("list session subjects", e))?;
            if count == 0 {
                continue;
            }
            if let Some(Ok(a)) = subj
                .strip_prefix("borant.session.")
                .map(str::parse::<AgentId>)
            {
                out.push(a);
            }
        }
        out.sort();
        Ok(out)
    }
}
