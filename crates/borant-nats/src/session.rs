use async_nats::jetstream;
use async_nats::jetstream::stream::{self, DirectGetErrorKind};
use borant_core::AgentId;
use borant_core::model::SessionEvent;
use borant_core::ports::{PortError, PortResult, SessionStore};
use tokio::sync::OnceCell;

use crate::{SESSION_STREAM, unavailable};

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
        let mut out = Vec::new();
        let mut seq = 1;
        loop {
            match stream
                .direct_get_next_for_subject(subject.clone(), Some(seq))
                .await
            {
                Ok(msg) => {
                    seq = msg.sequence + 1;
                    match serde_json::from_slice::<SessionEvent>(&msg.payload) {
                        Ok(ev) => out.push((msg.sequence, ev)),
                        Err(e) => {
                            tracing::warn!(%agent, seq = msg.sequence, "skip bad session event: {e}")
                        }
                    }
                }
                Err(e) if matches!(e.kind(), DirectGetErrorKind::NotFound) => break,
                Err(e) => return Err(unavailable("read session log", e)),
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
