//! The hub alert pass: a stalled agent fires once, its claim goes `stuck`, and
//! the alert resolves when the agent moves again.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::events::{EventFilter, SwarmEventBody};
use borant_core::model::{
    ChannelKind, Harness, HostInfo, HostStats, RepoInfo, SessionStats, Task, TaskStatus,
};
use borant_core::ports::{EventLog, Notice, Notifier, PortResult, StateStore};
use borant_core::proto::{
    Actor, ApiError, CtlRequest, RegisterSession, Registered, SessionLeaseRef, ToolCall,
};
use borant_core::{AgentId, Clock, HostName, ManualClock, SessionId, Timestamp};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use futures::StreamExt;
use serde_json::Value;

#[derive(Debug, Default)]
struct Recorder {
    notices: Mutex<Vec<Notice>>,
}

impl Recorder {
    fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Notifier for Recorder {
    async fn notify(&self, notice: &Notice) -> PortResult<()> {
        self.notices.lock().unwrap().push(notice.clone());
        Ok(())
    }
}

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    log: MemoryEventLog,
    clock: Arc<ManualClock>,
    recorder: Arc<Recorder>,
}

fn name() -> HostName {
    "h1".parse().unwrap()
}

fn agent() -> AgentId {
    "w1".parse().unwrap()
}

fn fx() -> Fx {
    let state = MemoryState::new();
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let log = MemoryEventLog::new();
    let recorder = Arc::new(Recorder::default());
    let hub = Arc::new(
        Hub::new(
            Arc::new(state.clone()),
            Arc::new(bus),
            Arc::new(sessions),
            clock.clone(),
            HubConfig::default(),
        )
        .with_notifier(recorder.clone())
        .with_events(Arc::new(log.clone())),
    );
    Fx {
        hub,
        state,
        log,
        clock,
        recorder,
    }
}

impl Fx {
    async fn req(&self, r: CtlRequest) -> Result<Value, ApiError> {
        self.hub.handle(r).await.into_result()
    }

    async fn host(&self) {
        self.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name(),
                harnesses: vec![Harness::Fake],
                launcher: "process".into(),
                max_agents: 4,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }

    async fn register(&self) -> Registered {
        let v = self
            .req(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: name(),
                harness: Harness::Fake,
                harness_session_id: None,
                pid: Some(1),
                cwd: "/work".into(),
                repo: Some(RepoInfo {
                    remote_url: "git://git/demo.git".into(),
                    branch: "main".into(),
                    subdir: String::new(),
                }),
                agent: Some(agent()),
                channel: None,
                shared_dir: false,
                role: None,
                collection: None,
            }))
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    /// A heartbeat that says the session is busy and last wrote at `last`.
    async fn heartbeat(&self, reg: &Registered, last: Timestamp) {
        self.req(CtlRequest::HostHeartbeat {
            host: name(),
            sessions: vec![SessionLeaseRef {
                session: reg.session,
                agent: reg.agent.clone(),
                token: reg.token,
                idle_since: None,
                last_event_at: Some(last),
                pinned: false,
            }],
            launching: Vec::new(),
            stats: Some(HostStats {
                version: "test".into(),
                sessions: vec![SessionStats {
                    agent: reg.agent.clone(),
                    busy: true,
                    paused: false,
                    turn: 1,
                    last_event_at: Some(last),
                    wakes: 0,
                    wake_limited: 0,
                    ack_failed: 0,
                    append_failed: 0,
                    redelivered: 0,
                    bad_envelope: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: 0.0,
                }],
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    }

    async fn call(&self, reg: &Registered, call: ToolCall) -> Value {
        let actor = Actor {
            agent: reg.agent.clone(),
            session: reg.session,
            token: reg.token,
        };
        self.req(CtlRequest::Call {
            actor: Some(actor),
            key: None,
            call,
        })
        .await
        .unwrap()
    }
}

async fn task(fx: &Fx, reg: &Registered) -> Task {
    fx.call(
        reg,
        ToolCall::ChannelCreate {
            name: "api".parse().unwrap(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
        },
    )
    .await;
    let task: Task = serde_json::from_value(
        fx.call(
            reg,
            ToolCall::TaskCreate {
                channel: "api".parse().unwrap(),
                task: borant_core::model::NewTask {
                    title: "the work".into(),
                    ..Default::default()
                },
            },
        )
        .await,
    )
    .unwrap();
    serde_json::from_value(
        fx.call(
            reg,
            ToolCall::TaskClaim {
                channel: "api".parse().unwrap(),
                id: task.id,
            },
        )
        .await,
    )
    .unwrap()
}

async fn stored_task(fx: &Fx, id: borant_core::TaskId) -> Task {
    let e = fx
        .state
        .get(&borant_core::keys::task(&"api".parse().unwrap(), &id))
        .await
        .unwrap()
        .expect("the task");
    serde_json::from_slice(&e.value).unwrap()
}

async fn alert_keys(fx: &Fx) -> Vec<String> {
    fx.state
        .list("alert.")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect()
}

async fn event_kinds(fx: &Fx) -> Vec<&'static str> {
    let mut stream = fx
        .log
        .read(
            &EventFilter {
                kinds: vec!["alert.*".into()],
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    let mut kinds = Vec::new();
    while let Some(event) = stream.next().await {
        kinds.push(match event.body {
            SwarmEventBody::AlertFired { .. } => "alert.fired",
            SwarmEventBody::AlertResolved { .. } => "alert.resolved",
            _ => "other",
        });
    }
    kinds
}

#[tokio::test]
async fn alerts_a_stalled_agent_fires_once_sets_stuck_and_resolves() {
    let fx = fx();
    fx.host().await;
    let reg = fx.register().await;
    let task = task(&fx, &reg).await;
    assert_eq!(task.status, TaskStatus::InProgress);

    // A busy session whose last event is older than the 30 minute stall.
    let old = fx.clock.now().minus(Duration::from_secs(31 * 60));
    fx.heartbeat(&reg, old).await;
    fx.hub.tick().await.unwrap();

    let keys = alert_keys(&fx).await;
    assert_eq!(keys, vec!["alert.agent_stalled.w1".to_owned()]);
    let notices = fx.recorder.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].title, "An agent is stalled");
    assert_eq!(event_kinds(&fx).await, vec!["alert.fired"]);

    // The hub set the claimed task to stuck in the same pass.
    assert_eq!(stored_task(&fx, task.id).await.status, TaskStatus::Stuck);

    // The same state does not fire again, inside the cadence or after it.
    fx.hub.tick().await.unwrap();
    fx.clock.advance(Duration::from_secs(31));
    fx.hub.tick().await.unwrap();
    assert_eq!(fx.recorder.notices().len(), 1);
    assert_eq!(alert_keys(&fx).await.len(), 1);

    // The agent moves again: one resolve, and no second notice.
    fx.heartbeat(&reg, fx.clock.now()).await;
    fx.clock.advance(Duration::from_secs(31));
    fx.hub.tick().await.unwrap();
    assert!(alert_keys(&fx).await.is_empty(), "the alert resolves");
    assert_eq!(fx.recorder.notices().len(), 1);
    assert_eq!(
        event_kinds(&fx).await,
        vec!["alert.fired", "alert.resolved"]
    );
    assert_eq!(stored_task(&fx, task.id).await.status, TaskStatus::Stuck);
}
