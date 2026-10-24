use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borant_core::model::Envelope;
use borant_core::ports::{
    Acker, Delivery, Incoming, MessageBus, PortError, PortErrorKind, PortResult, Responder,
    subject_matches,
};
use borant_core::{AgentId, MessageId};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio_stream::wrappers::{BroadcastStream, UnboundedReceiverStream};

use crate::Availability;

/// An unacked message comes back in a pull after this time.
const REDELIVER_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct MemoryBus {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    log: Mutex<Log>,
    notify: Notify,
    subs: broadcast::Sender<(String, Envelope)>,
    servers: Mutex<HashMap<String, Vec<mpsc::UnboundedSender<Incoming>>>>,
    available: Availability,
}

#[derive(Debug, Default)]
struct Log {
    seq: u64,
    msgs: Vec<(u64, String, Envelope)>,
    ids: HashSet<MessageId>,
    inboxes: HashMap<AgentId, Inbox>,
}

#[derive(Debug)]
struct Inbox {
    subjects: Vec<String>,
    /// Messages up to this sequence number were handed out once.
    cursor: u64,
    /// The inbox gets only messages after this sequence number.
    start: u64,
    inflight: BTreeMap<u64, Instant>,
    acked: BTreeSet<u64>,
}

impl Inbox {
    fn wants(&self, subject: &str) -> bool {
        self.subjects.iter().any(|s| subject_matches(s, subject))
    }
}

impl Default for MemoryBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                log: Mutex::new(Log::default()),
                notify: Notify::new(),
                subs: broadcast::channel(1024).0,
                servers: Mutex::new(HashMap::new()),
                available: Availability::default(),
            }),
        }
    }

    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.inner.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn take(&self, agent: &AgentId, max: usize) -> PortResult<Vec<Delivery>> {
        let mut log = self.log();
        let Log { msgs, inboxes, .. } = &mut *log;
        let inbox = inboxes
            .get_mut(agent)
            .ok_or_else(|| PortError::not_found(format!("no inbox for agent {agent}")))?;
        let now = Instant::now();
        let mut out = Vec::new();
        let mut seqs = Vec::new();
        for (seq, at) in inbox.inflight.iter_mut() {
            if out.len() >= max {
                break;
            }
            if now.duration_since(*at) >= REDELIVER_AFTER {
                *at = now;
                seqs.push(*seq);
            }
        }
        for (seq, subject, _) in msgs.iter() {
            if seqs.len() >= max {
                break;
            }
            if *seq > inbox.cursor && inbox.wants(subject) {
                inbox.cursor = *seq;
                inbox.inflight.insert(*seq, now);
                seqs.push(*seq);
            }
        }
        for seq in seqs {
            if let Some((_, _, env)) = msgs.iter().find(|(s, _, _)| *s == seq) {
                out.push(Delivery {
                    envelope: env.clone(),
                    acker: Box::new(MemoryAck {
                        bus: self.clone(),
                        agent: agent.clone(),
                        seq,
                    }),
                });
            }
        }
        Ok(out)
    }
}

struct MemoryAck {
    bus: MemoryBus,
    agent: AgentId,
    seq: u64,
}

#[async_trait::async_trait]
impl Acker for MemoryAck {
    async fn ack(self: Box<Self>) -> PortResult<()> {
        self.bus.inner.available.check()?;
        let mut log = self.bus.log();
        if let Some(inbox) = log.inboxes.get_mut(&self.agent) {
            inbox.inflight.remove(&self.seq);
            inbox.acked.insert(self.seq);
        }
        Ok(())
    }
}

struct OneshotResponder(oneshot::Sender<Bytes>);

#[async_trait::async_trait]
impl Responder for OneshotResponder {
    async fn respond(self: Box<Self>, payload: Bytes) -> PortResult<()> {
        self.0
            .send(payload)
            .map_err(|_| PortError::other("the requester went away"))
    }
}

struct NoReply;

#[async_trait::async_trait]
impl Responder for NoReply {
    async fn respond(self: Box<Self>, _payload: Bytes) -> PortResult<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl MessageBus for MemoryBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        self.inner.available.check()?;
        {
            let mut log = self.log();
            if !log.ids.insert(env.id) {
                return Ok(());
            }
            log.seq += 1;
            let seq = log.seq;
            log.msgs.push((seq, subject.to_owned(), env.clone()));
        }
        let _ = self.inner.subs.send((subject.to_owned(), env.clone()));
        self.inner.notify.notify_waiters();
        Ok(())
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.available.check()?;
        let mut log = self.log();
        let seq = log.seq;
        log.inboxes
            .entry(agent.clone())
            .and_modify(|i| i.subjects = subjects.to_vec())
            .or_insert_with(|| Inbox {
                subjects: subjects.to_vec(),
                cursor: seq,
                start: seq,
                inflight: BTreeMap::new(),
                acked: BTreeSet::new(),
            });
        Ok(())
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            self.inner.available.check()?;
            let notified = self.inner.notify.notified();
            let got = self.take(agent, max)?;
            if !got.is_empty() {
                return Ok(got);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(Vec::new());
            }
        }
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.available.check()?;
        let log = self.log();
        let inbox = log
            .inboxes
            .get(agent)
            .ok_or_else(|| PortError::not_found(format!("no inbox for agent {agent}")))?;
        Ok(log
            .msgs
            .iter()
            .filter(|(s, subj, _)| *s > inbox.cursor && inbox.wants(subj))
            .count() as u64)
    }

    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>> {
        self.inner.available.check()?;
        let log = self.log();
        let Some(inbox) = log.inboxes.get(agent) else {
            return Ok(Vec::new());
        };
        Ok(log
            .msgs
            .iter()
            .filter(|(s, subj, _)| {
                *s > inbox.start && !inbox.acked.contains(s) && inbox.wants(subj)
            })
            .map(|(_, _, e)| e.clone())
            .collect())
    }

    async fn delete_inbox(&self, agent: &AgentId) -> PortResult<()> {
        self.inner.available.check()?;
        self.log().inboxes.remove(agent);
        Ok(())
    }

    async fn subscribe(&self, pattern: &str) -> PortResult<BoxStream<'static, Envelope>> {
        self.inner.available.check()?;
        let pattern = pattern.to_owned();
        Ok(BroadcastStream::new(self.inner.subs.subscribe())
            .filter_map(move |r| {
                futures::future::ready(match r {
                    Ok((subject, env)) if subject_matches(&pattern, &subject) => Some(env),
                    _ => None,
                })
            })
            .boxed())
    }

    async fn request(&self, subject: &str, payload: Bytes, timeout: Duration) -> PortResult<Bytes> {
        self.inner.available.check()?;
        let (tx, rx) = oneshot::channel();
        let incoming = Incoming {
            payload,
            responder: Box::new(OneshotResponder(tx)),
        };
        {
            let mut servers = self.inner.servers.lock().unwrap_or_else(|e| e.into_inner());
            let list = servers.entry(subject.to_owned()).or_default();
            list.retain(|s| !s.is_closed());
            let server = list
                .first()
                .ok_or_else(|| PortError::unavailable(format!("no server for {subject}")))?;
            server
                .send(incoming)
                .map_err(|_| PortError::unavailable(format!("no server for {subject}")))?;
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(b)) => Ok(b),
            Ok(Err(_)) => Err(PortError::other("the server dropped the request")),
            Err(_) => Err(PortError::new(PortErrorKind::Timeout, "request timed out")),
        }
    }

    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>> {
        self.inner.available.check()?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner
            .servers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(subject.to_owned())
            .or_default()
            .push(tx);
        Ok(UnboundedReceiverStream::new(rx).boxed())
    }

    async fn send(&self, subject: &str, payload: Bytes) -> PortResult<()> {
        self.inner.available.check()?;
        let mut servers = self.inner.servers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = servers.get_mut(subject) {
            list.retain(|s| !s.is_closed());
            for s in list.iter() {
                let _ = s.send(Incoming {
                    payload: payload.clone(),
                    responder: Box::new(NoReply),
                });
            }
        }
        Ok(())
    }
}
