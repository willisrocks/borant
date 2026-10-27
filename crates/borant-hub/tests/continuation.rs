//! Bounded task continuation (phase 11 spec): the `continues` field of a
//! spawn, its refusal without a bound task, the policy that the agent record
//! keeps, and the hub's idle path in `crates/borant-hub/src/continuation.rs`:
//! one continue per idle period, the record written before the send, a stored
//! wait that stops it, and the exhaust that sets `needs_human`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::model::{
    Agent, ChannelKind, ContinuePolicy, ContinueState, Envelope, Harness, NewTask, Target, Task,
    TaskBinding, TaskStatus,
};
use borant_core::ports::{Delivery, Incoming, MessageBus, PortResult, StateStore};
use borant_core::proto::{ApiErrorCode, PauseSelector, Registered, ToolCall};
use borant_core::{AgentId, ChannelName, Clock, Timestamp, keys, subjects};
use borant_hub::HubConfig;
use borant_memory::{MemoryBus, MemoryState};
use bytes::Bytes;
use common::Fx;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn policy() -> ContinuePolicy {
    ContinuePolicy {
        max: 2,
        max_hours: Some(4),
    }
}

fn unlimited(max: u32) -> ContinuePolicy {
    ContinuePolicy {
        max,
        max_hours: None,
    }
}

fn spawn_call(continues: Option<ContinuePolicy>, task: Option<TaskBinding>) -> ToolCall {
    ToolCall::Spawn {
        channel: api(),
        prompt: "do the work".into(),
        harness: Some(Harness::Fake),
        host: None,
        name: None,
        model: None,
        role: None,
        key: None,
        task,
        stop_on_close: None,
        continues,
        priority: None,
        bundles: Vec::new(),
    }
}

/// A hub with host `h1`, an owner, a channel `api`, and one task in it.
async fn bound(fx: &Fx) -> (Registered, Task) {
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
        },
    )
    .await
    .expect("create the channel");
    let v = fx
        .call(
            Some(&owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "do the work".into(),
                    ..NewTask::default()
                },
            },
        )
        .await
        .expect("create the task");
    (owner, serde_json::from_value(v).expect("a task"))
}

/// A worker that a spawn bound to a task with `continues`, registered on `h1`,
/// and holding the claim on its task: the state that a continue needs.
async fn worker(fx: &Fx, continues: ContinuePolicy) -> (Registered, Registered, Task) {
    let (owner, task) = bound(fx).await;
    let v = fx
        .call(
            Some(&owner),
            spawn_call(
                Some(continues),
                Some(TaskBinding::Existing {
                    channel: api(),
                    id: task.id,
                }),
            ),
        )
        .await
        .expect("spawn a bound worker");
    let id: AgentId = serde_json::from_value(agent_id(&v)).expect("an agent ID");
    let w = fx.register("h1", Some(id.as_str()), Some("api")).await;
    fx.call(
        Some(&w),
        ToolCall::TaskClaim {
            channel: api(),
            id: task.id,
        },
    )
    .await
    .expect("claim the task");
    (owner, w, task)
}

/// The agent record, from the hub state.
async fn stored_agent(fx: &Fx, id: &AgentId) -> Agent {
    let doc = fx
        .state
        .get(&keys::agent(id))
        .await
        .expect("read the agent")
        .expect("a stored agent");
    serde_json::from_slice(&doc.value).expect("an agent")
}

/// The task record, from the hub state.
async fn stored_task(fx: &Fx, id: &borant_core::TaskId) -> Task {
    let doc = fx
        .state
        .get(&keys::task(&api(), id))
        .await
        .expect("read the task")
        .expect("a stored task");
    serde_json::from_slice(&doc.value).expect("a task")
}

/// The next direct message of an agent subject, if one comes soon.
async fn next_msg(s: &mut BoxStream<'static, Envelope>) -> Option<Envelope> {
    tokio::time::timeout(Duration::from_millis(200), s.next())
        .await
        .ok()
        .flatten()
}

/// Heartbeats for `secs` with the idle signal of `w`, so a long run keeps the
/// lease fresh.
async fn beat(fx: &Fx, w: &Registered, idle: Option<Timestamp>, secs: u64) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(10);
        fx.advance(step);
        fx.heartbeat_idle("h1", &[(w, idle)]).await;
        left -= step;
    }
}

#[tokio::test]
async fn a_spawn_with_a_continuation_and_a_task_keeps_the_policy() {
    let fx = Fx::new();
    let (owner, task) = bound(&fx).await;
    let v = fx
        .call(
            Some(&owner),
            spawn_call(
                Some(policy()),
                Some(TaskBinding::Existing {
                    channel: api(),
                    id: task.id,
                }),
            ),
        )
        .await
        .expect("spawn a bound worker");
    let id: AgentId = serde_json::from_value(agent_id(&v)).expect("an agent ID");
    let agent = stored_agent(&fx, &id).await;
    assert_eq!(agent.continues, Some(ContinueState::new(policy())));
    assert_eq!(agent.task.map(|t| t.id), Some(task.id));
}

#[tokio::test]
async fn a_spawn_that_asks_for_a_continuation_without_a_task_is_invalid() {
    let fx = Fx::new();
    let (owner, _task) = bound(&fx).await;
    let err = fx
        .call(Some(&owner), spawn_call(Some(policy()), None))
        .await
        .expect_err("continues need a bound task");
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
    assert!(err.message.contains("binds no task"), "{err:?}");
}

#[tokio::test]
async fn a_continuation_is_one_per_idle_period() {
    let fx = Fx::new();
    let (_owner, w, task) = worker(&fx, unlimited(3)).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();

    let first = fx.clock.now();
    fx.heartbeat_idle("h1", &[(&w, Some(first))]).await;
    let msg = next_msg(&mut inbox).await.expect("the continue");
    assert_eq!(msg.to, Target::Agent(w.agent.clone()));
    assert_eq!(
        msg.text,
        format!(
            "Continue with task api/{}. When it is done, call the wait tool on it; \
             do not stop otherwise.",
            task.id
        )
    );
    let a = stored_agent(&fx, &w.agent).await;
    assert_eq!(a.continues.unwrap().used, 1);
    assert_eq!(a.continues.unwrap().last_idle, Some(first));

    // The next heartbeat repeats the idle period that already got its
    // continue, so it gets none.
    fx.heartbeat_idle("h1", &[(&w, Some(first))]).await;
    assert!(next_msg(&mut inbox).await.is_none(), "one per idle period");
    assert_eq!(stored_agent(&fx, &w.agent).await.continues.unwrap().used, 1);

    // A new idle period continues again.
    fx.advance(30);
    let second = fx.clock.now();
    fx.heartbeat_idle("h1", &[(&w, Some(second))]).await;
    assert!(next_msg(&mut inbox).await.is_some(), "the next idle period");
    let a = stored_agent(&fx, &w.agent).await;
    assert_eq!(a.continues.unwrap().used, 2);
    assert_eq!(a.continues.unwrap().last_idle, Some(second));
}

#[tokio::test]
async fn a_stored_wait_stops_the_continuation() {
    let fx = Fx::new();
    let (_owner, w, task) = worker(&fx, unlimited(2)).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();
    // The worker is done: it waits for the close instead of stopping.
    fx.call(
        Some(&w),
        ToolCall::Wait {
            channel: api(),
            task: task.id,
            until: vec![TaskStatus::Closed],
            timeout_secs: 3_600,
        },
    )
    .await
    .expect("store the wait");

    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(next_msg(&mut inbox).await.is_none(), "a waiting worker");
    assert_eq!(stored_agent(&fx, &w.agent).await.continues.unwrap().used, 0);
    assert_eq!(
        stored_task(&fx, &task.id).await.status,
        TaskStatus::InProgress
    );
}

#[tokio::test]
async fn the_last_continuation_sets_needs_human_and_stops() {
    let fx = Fx::new();
    let (_owner, w, task) = worker(&fx, unlimited(1)).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();

    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(next_msg(&mut inbox).await.is_some(), "the one continue");

    // The count is spent: the task goes to a person and nothing is sent.
    fx.advance(30);
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(
        next_msg(&mut inbox).await.is_none(),
        "the limit sends nothing"
    );
    let a = stored_agent(&fx, &w.agent).await;
    assert!(a.continues.unwrap().exhausted);
    assert_eq!(a.continues.unwrap().used, 1);
    let t = stored_task(&fx, &task.id).await;
    assert_eq!(t.status, TaskStatus::NeedsHuman);
    assert_eq!(
        t.attention_msg.as_deref(),
        Some("continue limit reached (1)")
    );

    // A later idle period changes nothing.
    fx.advance(30);
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(next_msg(&mut inbox).await.is_none());
    assert!(
        stored_agent(&fx, &w.agent)
            .await
            .continues
            .unwrap()
            .exhausted
    );
}

#[tokio::test]
async fn the_session_hours_end_the_continuation() {
    let fx = Fx::new();
    let (_owner, w, task) = worker(
        &fx,
        ContinuePolicy {
            max: 5,
            max_hours: Some(1),
        },
    )
    .await;
    // An hour and a bit of turns, so the session hours run out.
    beat(&fx, &w, None, 3_700).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();
    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(
        next_msg(&mut inbox).await.is_none(),
        "the hours send nothing"
    );
    let a = stored_agent(&fx, &w.agent).await;
    assert!(a.continues.unwrap().exhausted);
    assert_eq!(a.continues.unwrap().used, 0);
    let t = stored_task(&fx, &task.id).await;
    assert_eq!(t.status, TaskStatus::NeedsHuman);
    assert_eq!(
        t.attention_msg.as_deref(),
        Some("continue hours reached (1)")
    );
}

#[tokio::test]
async fn a_paused_worker_gets_no_continuation() {
    let fx = Fx::new();
    let (owner, w, _task) = worker(&fx, unlimited(2)).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();
    fx.call(
        Some(&owner),
        ToolCall::Stop {
            selector: PauseSelector::Agent(w.agent.clone()),
            grace_secs: None,
        },
    )
    .await
    .expect("stop the worker");

    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(next_msg(&mut inbox).await.is_none(), "a paused worker");
    let a = stored_agent(&fx, &w.agent).await;
    assert!(a.paused.is_some());
    assert_eq!(a.continues.unwrap().used, 0);
}

#[tokio::test]
async fn a_retired_worker_gets_no_continuation() {
    let fx = Fx::new();
    let (owner, w, _task) = worker(&fx, unlimited(2)).await;
    let mut inbox = fx.bus.subscribe(&subjects::agent(&w.agent)).await.unwrap();
    fx.call(
        Some(&owner),
        ToolCall::Cancel {
            agent: w.agent.clone(),
            close_task: None,
        },
    )
    .await
    .expect("cancel the worker");

    fx.heartbeat_idle("h1", &[(&w, Some(fx.clock.now()))]).await;
    assert!(next_msg(&mut inbox).await.is_none(), "a retired worker");
    let a = stored_agent(&fx, &w.agent).await;
    assert!(a.retired);
    assert_eq!(a.continues.unwrap().used, 0);
}

/// What one publish saw of the agent record at that moment.
#[derive(Debug, Clone)]
struct Seen {
    subject: String,
    text: String,
    continues: Option<ContinueState>,
    task: Option<TaskStatus>,
}

/// A bus that reads the agent record when the hub publishes a message, so a
/// test can prove the record was written before the send.
#[derive(Debug, Clone)]
struct WatchBus {
    inner: MemoryBus,
    state: MemoryState,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl WatchBus {
    /// The state of the agent that a message is for, as the publish sees it.
    async fn snapshot(&self, subject: &str, env: &Envelope) -> Seen {
        let mut seen = Seen {
            subject: subject.to_owned(),
            text: env.text.clone(),
            continues: None,
            task: None,
        };
        let Target::Agent(id) = &env.to else {
            return seen;
        };
        let Ok(Some(doc)) = self.state.get(&keys::agent(id)).await else {
            return seen;
        };
        let Ok(agent) = serde_json::from_slice::<Agent>(&doc.value) else {
            return seen;
        };
        seen.continues = agent.continues;
        if let Some(task) = &agent.task
            && let Ok(Some(doc)) = self.state.get(&keys::task(&task.channel, &task.id)).await
            && let Ok(task) = serde_json::from_slice::<Task>(&doc.value)
        {
            seen.task = Some(task.status);
        }
        seen
    }
}

#[async_trait::async_trait]
impl MessageBus for WatchBus {
    async fn publish(&self, subject: &str, env: &Envelope) -> PortResult<()> {
        let seen = self.snapshot(subject, env).await;
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(seen);
        self.inner.publish(subject, env).await
    }

    async fn ensure_inbox(&self, agent: &AgentId, subjects: &[String]) -> PortResult<()> {
        self.inner.ensure_inbox(agent, subjects).await
    }

    async fn pull(&self, agent: &AgentId, max: usize, wait: Duration) -> PortResult<Vec<Delivery>> {
        self.inner.pull(agent, max, wait).await
    }

    async fn inbox_pending(&self, agent: &AgentId) -> PortResult<u64> {
        self.inner.inbox_pending(agent).await
    }

    async fn unacked(&self, agent: &AgentId) -> PortResult<Vec<Envelope>> {
        self.inner.unacked(agent).await
    }

    async fn delete_inbox(&self, agent: &AgentId) -> PortResult<()> {
        self.inner.delete_inbox(agent).await
    }

    async fn subscribe(&self, pattern: &str) -> PortResult<BoxStream<'static, Envelope>> {
        self.inner.subscribe(pattern).await
    }

    async fn request(&self, subject: &str, payload: Bytes, timeout: Duration) -> PortResult<Bytes> {
        self.inner.request(subject, payload, timeout).await
    }

    async fn serve(&self, subject: &str) -> PortResult<BoxStream<'static, Incoming>> {
        self.inner.serve(subject).await
    }

    async fn send(&self, subject: &str, payload: Bytes) -> PortResult<()> {
        self.inner.send(subject, payload).await
    }
}

#[tokio::test]
async fn the_continuation_record_is_written_before_the_send() {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let fx = {
        let seen = seen.clone();
        Fx::with_bus(HubConfig::default(), move |bus, state| {
            Arc::new(WatchBus {
                inner: bus,
                state,
                seen,
            })
        })
    };
    let (_owner, w, task) = worker(&fx, unlimited(2)).await;
    seen.lock().unwrap().clear();

    let idle = fx.clock.now();
    fx.heartbeat_idle("h1", &[(&w, Some(idle))]).await;

    let seen = seen.lock().unwrap();
    let sent = seen
        .iter()
        .find(|s| s.subject == subjects::agent(&w.agent))
        .expect("the continue");
    assert!(
        sent.text
            .starts_with(&format!("Continue with task api/{}", task.id))
    );
    // The publish saw the count that the continue of this idle period wrote,
    // so a hub that crashed after the send would count it once.
    let continues = sent.continues.expect("the policy");
    assert_eq!(continues.used, 1);
    assert_eq!(continues.last_idle, Some(idle));
    assert_eq!(sent.task, Some(TaskStatus::InProgress));
}

/// The agent of a spawn reply, `{"agent": {"agent": "<id>", ...}}`.
fn agent_id(v: &Value) -> Value {
    v["agent"]["agent"].clone()
}
