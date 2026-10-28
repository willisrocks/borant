//! The hub side of the launch watchdog: a failed launch is reported with the
//! redacted end of its screen, the try counts against the spawn, and a spawn
//! that failed three times is not placed again.

use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{SwarmEvent, SwarmEventBody};
use borant_core::model::{
    ChannelKind, Harness, HostInfo, NewTask, RepoInfo, Sender, SpawnRecord, SpawnSpec, SpawnState,
    Task,
};
use borant_core::ports::{EventLog, MessageBus, StateStore};
use borant_core::proto::{
    Actor, CtlRequest, HostCommand, RegisterSession, Registered, SessionLeaseRef, ToolCall,
};
use borant_core::rules::roles::Resolved;
use borant_core::{AgentId, ChannelName, ManualClock, SessionId, Timestamp, keys, subjects};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use futures::StreamExt;

use futures::stream::BoxStream;

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    bus: MemoryBus,
    log: Arc<MemoryEventLog>,
    clock: Arc<ManualClock>,
}

impl Fx {
    fn new() -> Self {
        let state = MemoryState::new();
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000_000)));
        let log = Arc::new(MemoryEventLog::new());
        let hub = Arc::new(
            Hub::new(
                Arc::new(state.clone()),
                Arc::new(bus.clone()),
                Arc::new(sessions),
                clock.clone(),
                HubConfig::default(),
            )
            .with_events(log.clone()),
        );
        Self {
            hub,
            state,
            bus,
            log,
            clock,
        }
    }

    async fn req(
        &self,
        req: CtlRequest,
    ) -> Result<serde_json::Value, borant_core::proto::ApiError> {
        self.hub.handle(req).await.into_result()
    }

    async fn host(&self, host: &str) {
        self.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: host.parse().unwrap(),
                harnesses: vec![Harness::Fake],
                launcher: "process".into(),
                max_agents: 4,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    }

    async fn register(&self, host: &str, agent: Option<&str>, channel: Option<&str>) -> Registered {
        let v = self
            .req(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: host.parse().unwrap(),
                harness: Harness::Fake,
                pid: Some(1),
                cwd: "/work".into(),
                repo: Some(RepoInfo {
                    remote_url: "git://git/demo.git".into(),
                    branch: "main".into(),
                    subdir: String::new(),
                }),
                agent: agent.map(|a| a.parse().unwrap()),
                channel: channel.map(|c| c.parse().unwrap()),
                ..Default::default()
            }))
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    async fn call(&self, who: &Registered, call: ToolCall) -> serde_json::Value {
        self.req(CtlRequest::Call {
            actor: Some(Actor {
                agent: who.agent.clone(),
                session: who.session,
                token: who.token,
            }),
            key: None,
            call,
        })
        .await
        .unwrap()
    }

    async fn heartbeat(&self, host: &str, regs: &[&Registered]) {
        let sessions: Vec<SessionLeaseRef> = regs
            .iter()
            .map(|r| SessionLeaseRef {
                session: r.session,
                agent: r.agent.clone(),
                token: r.token,
                idle_since: None,
                last_event_at: None,
                pinned: false,
            })
            .collect();
        self.req(CtlRequest::HostHeartbeat {
            host: host.parse().unwrap(),
            sessions,
            stats: None,
            launching: Vec::new(),
        })
        .await
        .unwrap();
    }

    async fn commands(&self, host: &str) -> BoxStream<'static, HostCommand> {
        self.bus
            .serve(&subjects::host_cmd(&host.parse().unwrap()))
            .await
            .unwrap()
            .filter_map(|i| async move { serde_json::from_slice(&i.payload).ok() })
            .boxed()
    }

    fn advance(&self, secs: u64) {
        self.clock.advance(Duration::from_secs(secs));
    }

    async fn events(&self) -> Vec<SwarmEvent> {
        let mut stream = self
            .log
            .read(&Default::default(), false)
            .await
            .expect("read the event log");
        let mut out = Vec::new();
        while let Some(e) = stream.next().await {
            out.push(e);
        }
        out
    }

    /// The spawn record of `agent`, read back as the hub wrote it.
    async fn spawn_record(&self, agent: &AgentId) -> Option<SpawnRecord> {
        let entry = self.state.get(&keys::spawn(agent)).await.unwrap()?;
        Some(serde_json::from_slice(&entry.value).unwrap())
    }

    /// Writes the spawn record of `agent` with `state`, as the spawn queue
    /// will once it exists.
    async fn set_spawn(&self, agent: &AgentId, state: SpawnState, attempts: u32) {
        let record = SpawnRecord {
            agent: agent.clone(),
            spec: SpawnSpec {
                channel: "api".parse().unwrap(),
                prompt: String::new(),
                harness: None,
                host: None,
                name: None,
                model: None,
                role: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                bundles: Vec::new(),
            },
            resolved: Resolved::default(),
            by: Sender::Human,
            queued_at: Timestamp(1_000_000_000),
            priority: 0,
            state,
            attempts,
            last_reject: Vec::new(),
            queue_reason: None,
            launch_failures: Vec::new(),
        };
        let value = serde_json::to_vec(&record).unwrap();
        match self.state.get(&keys::spawn(agent)).await.unwrap() {
            Some(entry) => {
                self.state
                    .update(&entry.key, value.into(), entry.revision)
                    .await
                    .unwrap();
            }
            None => {
                self.state
                    .create(&keys::spawn(agent), value.into())
                    .await
                    .unwrap();
            }
        }
    }

    async fn tick(&self) {
        self.hub.tick().await.unwrap();
    }
}

async fn next_cmd(s: &mut BoxStream<'static, HostCommand>) -> Option<HostCommand> {
    tokio::time::timeout(Duration::from_millis(200), s.next())
        .await
        .ok()
        .flatten()
}

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// Two hosts, an owner with a channel on h1, and a worker on h1.
async fn setup(fx: &Fx) -> (Registered, Registered) {
    fx.host("h1").await;
    fx.host("h2").await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        &owner,
        ToolCall::ChannelCreate {
            name: api(),
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
    let w = fx.register("h1", Some("w"), Some("api")).await;
    (owner, w)
}

/// A task of the channel that the worker holds.
async fn claim(fx: &Fx, owner: &Registered, w: &Registered) {
    let t: Task = serde_json::from_value(
        fx.call(
            owner,
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "t".into(),
                    ..Default::default()
                },
            },
        )
        .await,
    )
    .unwrap();
    fx.call(
        w,
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await;
}

/// Lets h1 go silent while h2 keeps sending heartbeats.
async fn h1_sleeps(fx: &Fx, secs: u64) {
    let mut left = secs;
    while left > 0 {
        let step = left.min(10);
        fx.advance(step);
        fx.heartbeat("h2", &[]).await;
        fx.tick().await;
        left -= step;
    }
}

#[tokio::test]
async fn watchdog_failure_reports_the_redacted_screen_and_counts_tries() {
    let fx = Fx::new();
    let agent: AgentId = "a1".parse().unwrap();
    fx.set_spawn(&agent, SpawnState::Queued, 0).await;
    let screen = "Do you trust this folder?\n  token=abc123";

    for _ in 0..3 {
        fx.req(CtlRequest::LaunchFailed {
            agent: agent.clone(),
            host: "h1".parse().unwrap(),
            error: screen.into(),
            timeout_secs: Some(90.0),
        })
        .await
        .unwrap();
    }

    let record = fx.spawn_record(&agent).await.expect("the spawn record");
    assert_eq!(record.attempts, 3, "every failed try counts");
    assert_eq!(record.state, SpawnState::Failed, "three tries give up");

    let stuck: Vec<(String, u32)> = fx
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e.body {
            SwarmEventBody::LaunchStuck { screen, attempts } => Some((screen, attempts)),
            _ => None,
        })
        .collect();
    assert_eq!(stuck.len(), 3, "{stuck:?}");
    assert_eq!(
        stuck.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (screen, _) in &stuck {
        assert!(screen.contains("Do you trust this folder?"), "{screen}");
        assert!(
            !screen.contains("abc123"),
            "the screen is redacted: {screen}"
        );
    }
}

#[tokio::test]
async fn watchdog_failed_spawn_is_not_placed_again() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    claim(&fx, &owner, &w).await;
    fx.set_spawn(&w.agent, SpawnState::Failed, 3).await;
    let mut h2 = fx.commands("h2").await;

    h1_sleeps(&fx, 160).await;
    assert!(
        next_cmd(&mut h2).await.is_none(),
        "a spawn that failed three launches waits for a person"
    );

    // The same lease is placed once the record allows it, so the wait above
    // is the record and not a lease that never expired.
    fx.set_spawn(&w.agent, SpawnState::Queued, 0).await;
    fx.advance(10);
    fx.heartbeat("h2", &[]).await;
    fx.tick().await;
    match next_cmd(&mut h2).await {
        Some(HostCommand::Launch(o)) => assert_eq!(o.agent, w.agent),
        other => panic!("expected a launch, got {other:?}"),
    }
}
