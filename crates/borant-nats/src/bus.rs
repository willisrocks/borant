use std::time::Duration;

use async_nats::jetstream::consumer::{DeliverPolicy, pull};
use async_nats::jetstream::stream::{self, DirectGetErrorKind};
use async_nats::jetstream::{self, AckKind};
use async_nats::{HeaderMap, RequestErrorKind};
use borant_core::AgentId;
use borant_core::model::Envelope;
use borant_core::ports::{
    Acker, Delivery, Incoming, MessageBus, PortError, PortErrorKind, PortResult, Responder,
};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::OnceCell;

use crate::{MSG_STREAM, unavailable};

const ACK_WAIT: Duration = Duration::from_secs(60);

fn consumer_name(agent: &AgentId) -> String {
    format!("inbox-{agent}")
}

#[derive(Debug, Clone)]
pub struct NatsBus {
    client: async_nats::Client,
    js: jetstream::Context,
    max_age: Duration,
    stream: std::sync::Arc<OnceCell<stream::Stream>>,
}

impl NatsBus {
    pub(crate) fn new(
        client: async_nats::Client,
        js: jetstream::Context,
        max_age: Duration,
    ) -> Self {
        Self {
            client,
            js,
            max_age,
            stream: Default::default(),
        }
    }

    async fn stream(&self) -> PortResult<&stream::Stream> {
        self.stream
            .get_or_try_init(|| async {
                self.js
                    .get_or_create_stream(stream::Config {
                        name: MSG_STREAM.to_owned(),
                        subjects: vec!["borant.agent.>".to_owned(), "borant.chan.>".to_owned()],
                        max_age: self.max_age,
                        duplicate_window: Duration::from_secs(120),
                        allow_direct: true,
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| unavailable("create message stream", e))
            })
            .await
    }

    async fn consumer(
        &self,
        agent: &AgentId,
    ) -> PortResult<jetstream::consumer::Consumer<pull::Config>> {
        let stream = self.stream().await?;
        stream
            .get_consumer(&consumer_name(agent))
            .await
            .map_err(|e| {
                if e.to_string().to_lowercase().contains("not found") {
                    PortError::not_found(format!("no inbox for agent {agent}"))
                } else {
                    unavailable("get inbox", e)
                }
            })
    }
}

struct NatsAck(jetstream::Message);

#[async_trait::async_trait]
impl Acker for NatsAck {
    async fn ack(self: Box<Self>) -> PortResult<()> {
        self.0
            .ack_with(AckKind::Ack)
            .await
            .map_err(|e| unavailable("ack", e))
    }
}

struct ReplyTo {
    client: async_nats::Client,
    reply: Option<async_nats::Subject>,
}

#[async_trait::async_trait]
impl Responder for ReplyTo {
    async fn respond(self: Box<Self>, payload: Bytes) -> PortResult<()> {
        let Some(reply) = self.reply else {
            return Ok(());
        };
        self.client
            .publish(reply, payload)
            .await
            .map_err(|e| unavailable("reply", e))?;
        self.client
            .flush()
            .await
            .map_err(|e| unavailable("flush reply", e))
    }
}

#[async_trait::async_trait]
impl MessageBus for NatsBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.stream().await?;
        let payload = serde_json::to_vec(env)
            .map_err(|e| PortError::other(format!("encode envelope: {e}")))?;
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", env.id.to_string().as_str());
        self.js
            .publish_with_headers(subject.to_owned(), headers, payload.into())
            .await
            .map_err(|e| unavailable("publish", e))?
            .await
            .map_err(|e| unavailable("publish ack", e))?;
        Ok(())
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        let stream = self.stream().await?;
        match self.consumer(agent).await {
            Ok(mut c) => {
                let info = c.info().await.map_err(|e| unavailable("inbox info", e))?;
                let mut have = info.config.filter_subjects.clone();
                if have.is_empty() && !info.config.filter_subject.is_empty() {
                    have.push(info.config.filter_subject.clone());
                }
                if have == subjects {
                    return Ok(());
                }
                let config = pull::Config {
                    durable_name: Some(consumer_name(agent)),
                    deliver_policy: info.config.deliver_policy,
                    ack_wait: info.config.ack_wait,
                    filter_subjects: subjects.to_vec(),
                    ..Default::default()
                };
                stream
                    .update_consumer(config)
                    .await
                    .map_err(|e| unavailable("update inbox", e))?;
                Ok(())
            }
            Err(e) if e.is(PortErrorKind::NotFound) => {
                stream
                    .create_consumer(pull::Config {
                        durable_name: Some(consumer_name(agent)),
                        deliver_policy: DeliverPolicy::New,
                        ack_wait: ACK_WAIT,
                        filter_subjects: subjects.to_vec(),
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| unavailable("create inbox", e))?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        let consumer = self.consumer(agent).await?;
        let mut batch = consumer
            .batch()
            .max_messages(max.max(1))
            .expires(wait.max(Duration::from_millis(100)))
            .messages()
            .await
            .map_err(|e| unavailable("pull", e))?;
        let mut out = Vec::new();
        loop {
            // Wait for the first message up to `wait`. After it, take only
            // what arrives at once, so a single message is not delayed.
            let next = if out.is_empty() {
                batch.next().await
            } else {
                match tokio::time::timeout(Duration::from_millis(20), batch.next()).await {
                    Ok(n) => n,
                    Err(_) => break,
                }
            };
            let Some(m) = next else { break };
            let m = match m {
                Ok(m) => m,
                Err(e) => {
                    if out.is_empty() {
                        return Err(unavailable("pull message", e));
                    }
                    break;
                }
            };
            match serde_json::from_slice::<Envelope>(&m.payload) {
                Ok(envelope) => out.push(Delivery {
                    envelope,
                    acker: Box::new(NatsAck(m)),
                }),
                Err(e) => {
                    tracing::warn!(%agent, "drop bad envelope: {e}");
                    let _ = m.ack().await;
                }
            }
        }
        Ok(out)
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        let mut c = self.consumer(agent).await?;
        let info = c.info().await.map_err(|e| unavailable("inbox info", e))?;
        Ok(info.num_pending)
    }

    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>> {
        let stream = self.stream().await?;
        let mut c = match self.consumer(agent).await {
            Ok(c) => c,
            Err(e) if e.is(PortErrorKind::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let info = c
            .info()
            .await
            .map_err(|e| unavailable("inbox info", e))?
            .clone();
        let mut subjects = info.config.filter_subjects.clone();
        if subjects.is_empty() && !info.config.filter_subject.is_empty() {
            subjects.push(info.config.filter_subject.clone());
        }
        let floor = info.ack_floor.stream_sequence;
        let mut found: Vec<(u64, Envelope)> = Vec::new();
        for subject in subjects {
            let mut seq = floor + 1;
            loop {
                match stream
                    .direct_get_next_for_subject(subject.clone(), Some(seq))
                    .await
                {
                    Ok(m) => {
                        seq = m.sequence + 1;
                        if let Ok(env) = serde_json::from_slice::<Envelope>(&m.payload) {
                            found.push((m.sequence, env));
                        }
                    }
                    Err(e) if matches!(e.kind(), DirectGetErrorKind::NotFound) => break,
                    Err(e) => return Err(unavailable("read unacked", e)),
                }
            }
        }
        found.sort_by_key(|(s, _)| *s);
        Ok(found.into_iter().map(|(_, e)| e).collect())
    }

    async fn delete_inbox(&self, agent: &AgentId) -> PortResult<()> {
        let stream = self.stream().await?;
        match stream.delete_consumer(&consumer_name(agent)).await {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().to_lowercase().contains("not found") => Ok(()),
            Err(e) => Err(unavailable("delete inbox", e)),
        }
    }

    async fn subscribe(&self, pattern: &str) -> PortResult<BoxStream<'static, Envelope>> {
        let sub = self
            .client
            .subscribe(pattern.to_owned())
            .await
            .map_err(|e| unavailable("subscribe", e))?;
        Ok(sub
            .filter_map(|m| {
                futures::future::ready(serde_json::from_slice::<Envelope>(&m.payload).ok())
            })
            .boxed())
    }

    async fn request(&self, subject: &str, payload: Bytes, timeout: Duration) -> PortResult<Bytes> {
        match tokio::time::timeout(timeout, self.client.request(subject.to_owned(), payload)).await
        {
            Ok(Ok(m)) => Ok(m.payload),
            Ok(Err(e)) => Err(match e.kind() {
                RequestErrorKind::NoResponders => {
                    PortError::unavailable(format!("no server for {subject}"))
                }
                RequestErrorKind::TimedOut => PortError::new(
                    PortErrorKind::Timeout,
                    format!("request to {subject} timed out"),
                ),
                _ => unavailable("request", e),
            }),
            Err(_) => Err(PortError::new(
                PortErrorKind::Timeout,
                format!("request to {subject} timed out"),
            )),
        }
    }

    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>> {
        let sub = self
            .client
            .subscribe(subject.to_owned())
            .await
            .map_err(|e| unavailable("serve", e))?;
        let client = self.client.clone();
        Ok(sub
            .map(move |m| Incoming {
                payload: m.payload,
                responder: Box::new(ReplyTo {
                    client: client.clone(),
                    reply: m.reply,
                }),
            })
            .boxed())
    }

    async fn send(&self, subject: &str, payload: Bytes) -> PortResult<()> {
        self.client
            .publish(subject.to_owned(), payload)
            .await
            .map_err(|e| unavailable("send", e))?;
        self.client
            .flush()
            .await
            .map_err(|e| unavailable("flush", e))
    }
}
