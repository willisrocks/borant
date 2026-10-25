//! A hub restart on a large store must keep answering requests while the
//! catch-up tick runs.
//!
//! The store has the shape that a hub restart met on borant-1 (about 1900 keys,
//! most of them under `chan.*`), with 200 agents whose leases expired an hour
//! ago and still hold a claim: so the first tick has a long pass over many
//! leases, while the work for one lease stays a short step.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borant_core::model::{
    Agent, Channel, Harness, Lease, LeaseState, Member, RepoInfo, Session, Task, TaskStatus,
};
use borant_core::ports::{Change, Entry, PortResult, StateStore};
use borant_core::proto::{CtlRequest, RegisterSession, ToolCall};
use borant_core::{
    AgentId, ChannelName, Clock, FencingToken, ManualClock, SessionId, TaskId, Timestamp, keys,
};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use bytes::Bytes;
use futures::stream::BoxStream;

const AGENTS: usize = 40;
const CHANNELS: usize = 4;
const TASKS_PER_CHANNEL: usize = 320;
const MEMBERS_PER_CHANNEL: usize = 4;
/// One store call costs about this.
const CALL: Duration = Duration::from_millis(1);

/// A store call costs `CALL`, and the first call the tick makes after its list
/// rings a bell: the tick makes it while it holds the memory lock. Seeding
/// writes to the inner `MemoryState` and pays neither.
#[derive(Debug)]
struct SlowState {
    inner: MemoryState,
    calls: AtomicU64,
    in_step: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl SlowState {
    fn new(inner: MemoryState) -> Self {
        Self {
            inner,
            calls: AtomicU64::new(0),
            in_step: Mutex::new(None),
        }
    }

    fn watch_step(&self) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.in_step.lock().unwrap() = Some(tx);
        rx
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    async fn slow(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(CALL).await;
    }

    fn step_started(&self) {
        if let Some(tx) = self.in_step.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

#[async_trait::async_trait]
impl StateStore for SlowState {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        self.step_started();
        self.slow().await;
        self.inner.get(key).await
    }

    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64> {
        self.step_started();
        self.slow().await;
        self.inner.create(key, value).await
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        self.step_started();
        self.slow().await;
        self.inner.update(key, value, expected).await
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        self.step_started();
        self.slow().await;
        self.inner.delete(key, expected).await
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        // The tick lists the leases before its first step, so a list is not
        // yet a step.
        self.slow().await;
        self.inner.list(prefix).await
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        self.inner.watch(prefix).await
    }
}

async fn put(state: &MemoryState, key: &str, value: &impl serde::Serialize) {
    let bytes = Bytes::from(serde_json::to_vec(value).unwrap());
    match state.get(key).await.unwrap() {
        Some(e) => {
            state.update(key, bytes, e.revision).await.unwrap();
        }
        None => {
            state.create(key, bytes).await.unwrap();
        }
    }
}

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    slow: Arc<SlowState>,
    clock: Arc<ManualClock>,
}

fn channel(i: usize) -> ChannelName {
    format!("c{i}").parse().unwrap()
}

fn agent(i: usize) -> AgentId {
    format!("w{i:03}").parse().unwrap()
}

impl Fx {
    fn new() -> Self {
        let state = MemoryState::new();
        let slow = Arc::new(SlowState::new(state.clone()));
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let hub = Arc::new(Hub::new(
            slow.clone(),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock.clone(),
            HubConfig::default(),
        ));
        Self {
            hub,
            state,
            slow,
            clock,
        }
    }

    /// Seeds the store that a hub restart met: channels with members and many
    /// tasks, and agents whose leases expired an hour ago.
    async fn seed(&self) {
        let now = self.clock.now();
        let since = now.minus(Duration::from_secs(3600));
        for c in 0..CHANNELS {
            let name = channel(c);
            let chan = Channel {
                name: name.clone(),
                owner: agent(c),
                schema: serde_json::json!({}),
                created: now,
                closed: None,
                project: None,
                branch: None,
            };
            put(&self.state, &keys::channel(&name), &chan).await;
            for m in 0..MEMBERS_PER_CHANNEL {
                let member = Member {
                    agent: agent(c + m),
                    joined: now,
                };
                put(&self.state, &keys::member(&name, &member.agent), &member).await;
            }
            for t in 0..TASKS_PER_CHANNEL {
                let task = Task {
                    id: TaskId::new(),
                    title: format!("task {t}"),
                    body: String::new(),
                    status: if t == 0 {
                        TaskStatus::InProgress
                    } else {
                        TaskStatus::Open
                    },
                    close_reason: None,
                    close_message: None,
                    attention_msg: None,
                    // The first task is still claimed by an expired agent.
                    claimed_by: (t == 0).then(|| agent(c)),
                    blocked_by: vec![],
                    parent: None,
                    properties: Default::default(),
                    created: now,
                    updated: now,
                };
                put(&self.state, &keys::task(&name, &task.id), &task).await;
            }
        }
        for i in 0..AGENTS {
            let id = agent(i);
            let a = Agent {
                id: id.clone(),
                preferred_harness: Harness::Fake,
                repo: Some(RepoInfo {
                    remote_url: "git://git/demo.git".into(),
                    branch: "main".into(),
                    subdir: String::new(),
                }),
                movable: true,
                channels: vec![channel(i % CHANNELS)],
                session: None,
                host: None,
                harness_session_id: None,
                created: now,
                model: None,
                model_harness: None,
                project: None,
                members: Default::default(),
                pinned: false,
            };
            put(&self.state, &keys::agent(&id), &a).await;
            let lease = Lease {
                agent: id.clone(),
                token: FencingToken(1),
                state: LeaseState::Expired {
                    since,
                    last_session: None,
                },
            };
            put(&self.state, &keys::lease_agent(&id), &lease).await;
            let session = Session {
                id: SessionId::new(),
                agent: id,
                harness: Harness::Fake,
                harness_session_id: None,
                host: "h1".parse().unwrap(),
                cwd: "/work".into(),
                pid: Some(1),
                started: now,
                ended: Some(since),
            };
            put(&self.state, &keys::session(&session.id), &session).await;
        }
    }
}

/// A `borant.ctl` request must be answered while the first tick catches up: the
/// hub serves it between two steps of the pass, not after the pass. The tick
/// still does all of its work.
#[tokio::test]
async fn answers_a_request_while_the_first_tick_catches_up() {
    let fx = Fx::new();
    fx.seed().await;
    let step = fx.slow.watch_step();
    let tick = {
        let hub = fx.hub.clone();
        tokio::spawn(async move { hub.tick().await })
    };
    // The tick is inside a step with the memory lock held.
    tokio::time::timeout(Duration::from_secs(10), step)
        .await
        .expect("the tick never reached a step")
        .expect("the tick dropped the bell");

    let start = Instant::now();
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        fx.hub.handle(CtlRequest::Call {
            actor: None,
            call: ToolCall::ChannelList,
        }),
    )
    .await
    .expect("the hub did not answer a request while the tick ran");
    let answered_at = fx.slow.calls();
    let waited = start.elapsed();
    assert!(reply.into_result().is_ok(), "the request failed");

    tick.await.unwrap().expect("the tick failed");
    let total = fx.slow.calls();
    assert!(
        answered_at * 2 < total,
        "the request waited {waited:?} for the tick, which was still at store call \
         {answered_at} of {total}"
    );
    // The pass did its work: the claims of the expired agents are open again.
    for c in 0..CHANNELS {
        for e in fx
            .state
            .list(&keys::tasks_prefix(&channel(c)))
            .await
            .unwrap()
        {
            let t: Task = serde_json::from_slice(&e.value).unwrap();
            assert_eq!(t.claimed_by, None, "task {} is still claimed", t.id);
        }
    }
}

/// A session that registers while the tick runs keeps its claims. The tick acts
/// on a lease only while the lease is still the one it listed.
#[tokio::test]
async fn a_session_that_registers_during_the_tick_keeps_its_claims() {
    let fx = Fx::new();
    fx.seed().await;
    let step = fx.slow.watch_step();
    let tick = {
        let hub = fx.hub.clone();
        tokio::spawn(async move { hub.tick().await })
    };
    tokio::time::timeout(Duration::from_secs(10), step)
        .await
        .expect("the tick never reached a step")
        .expect("the tick dropped the bell");

    // Every agent but the first registers a session while the tick is inside
    // its first step, so its lease changes before the tick reaches it.
    let mut regs = Vec::new();
    for i in 1..AGENTS {
        let hub = fx.hub.clone();
        regs.push(tokio::spawn(async move {
            hub.handle(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: "h1".parse().unwrap(),
                harness: Harness::Fake,
                harness_session_id: None,
                pid: Some(1),
                cwd: "/work".into(),
                repo: Some(RepoInfo {
                    remote_url: "git://git/demo.git".into(),
                    branch: "main".into(),
                    subdir: String::new(),
                }),
                agent: Some(agent(i)),
                channel: None,
                shared_dir: false,
            }))
            .await
        }));
    }
    for r in regs {
        let reply = r.await.unwrap();
        assert!(reply.into_result().is_ok(), "the registration failed");
    }
    tick.await.unwrap().expect("the tick failed");

    // The first agent registered nothing, so its claim went back to the open.
    // The agents that came back keep theirs.
    for c in 0..CHANNELS {
        let tasks: Vec<Task> = fx
            .state
            .list(&keys::tasks_prefix(&channel(c)))
            .await
            .unwrap()
            .iter()
            .map(|e| serde_json::from_slice(&e.value).unwrap())
            .collect();
        let claimed = tasks
            .iter()
            .find(|t| t.claimed_by.is_some())
            .or_else(|| tasks.iter().find(|t| t.title == "task 0"));
        let claimed = claimed.unwrap();
        if c == 0 {
            assert_eq!(claimed.claimed_by, None, "agent 0 never came back");
        } else {
            assert_eq!(
                claimed.claimed_by,
                Some(agent(c)),
                "channel {c} lost the claim of the agent that came back"
            );
        }
    }
}
