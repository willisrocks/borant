//! Stop on close: a worker bound to a closed task retires, its claims go back
//! to open, its session stops at the turn end, and nothing places it again.

use std::sync::Arc;
use std::time::Duration;

use borant_core::events::{EventFilter, kind_of};
use borant_core::model::{
    Agent, CloseReason, Harness, HostInfo, HostStats, Lease, LeaseState, NewTask, RepoInfo, Task,
    TaskRef, TaskStatus,
};
use borant_core::ports::{EventLog, MessageBus, StateStore};
use borant_core::proto::{
    CtlRequest, HostCommand, RegisterSession, Registered, SessionLeaseRef, ToolCall,
};
use borant_core::{AgentId, ChannelName, Clock, HostName, ManualClock, SessionId, Timestamp, keys};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use futures::StreamExt;

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    bus: MemoryBus,
    events: MemoryEventLog,
    clock: Arc<ManualClock>,
}

impl Fx {
    fn new() -> Self {
        let state = MemoryState::new();
        let bus = MemoryBus::new();
        let sessions = MemorySession::new();
        let events = MemoryEventLog::new();
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let hub = Arc::new(
            Hub::new(
                Arc::new(state.clone()),
                Arc::new(bus.clone()),
                Arc::new(sessions.clone()),
                clock.clone(),
                HubConfig::default(),
            )
            .with_events(Arc::new(events.clone())),
        );
        Self {
            hub,
            state,
            bus,
            events,
            clock,
        }
    }

    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    async fn req(&self, r: CtlRequest) -> Result<serde_json::Value, borant_core::proto::ApiError> {
        self.hub.handle(r).await.into_result()
    }

    async fn call(
        &self,
        who: Option<&Registered>,
        call: ToolCall,
    ) -> Result<serde_json::Value, borant_core::proto::ApiError> {
        let actor = who.map(|r| borant_core::proto::Actor {
            agent: r.agent.clone(),
            session: r.session,
            token: r.token,
        });
        self.req(CtlRequest::Call {
            actor,
            key: None,
            call,
        })
        .await
    }

    async fn register(&self, host: &str, agent: &str, channel: Option<&str>) -> Registered {
        let v = self
            .req(CtlRequest::RegisterSession(RegisterSession {
                session: SessionId::new(),
                host: host.parse().unwrap(),
                harness: Harness::Fake,
                harness_session_id: None,
                pid: Some(1),
                cwd: "/work".into(),
                repo: Some(RepoInfo {
                    remote_url: "git://git/demo.git".into(),
                    branch: "main".into(),
                    subdir: String::new(),
                }),
                agent: Some(agent.parse().unwrap()),
                channel: channel.map(|c| c.parse().unwrap()),
                shared_dir: false,
                role: None,
                collection: None,
            }))
            .await
            .unwrap();
        serde_json::from_value(v).unwrap()
    }

    async fn heartbeat(&self, host: &str, w: &Registered, idle: Option<Timestamp>) {
        self.heartbeat_with(host, w, idle, None).await;
    }

    /// A heartbeat that carries the `SessionStats` of the session, whose
    /// `busy` is the turn state that `TurnStart` and `TurnEnd` set on the
    /// host. The session reports no turn end, as an omp session that waits
    /// for its first prompt does.
    async fn heartbeat_busy(&self, host: &str, w: &Registered, busy: bool) {
        let stats = HostStats {
            sessions: vec![
                serde_json::from_value(serde_json::json!({
                    "agent": w.agent,
                    "busy": busy,
                }))
                .unwrap(),
            ],
            ..Default::default()
        };
        self.heartbeat_with(host, w, None, Some(stats)).await;
    }

    async fn heartbeat_with(
        &self,
        host: &str,
        w: &Registered,
        idle: Option<Timestamp>,
        stats: Option<HostStats>,
    ) {
        self.req(CtlRequest::HostHeartbeat {
            host: host.parse().unwrap(),
            sessions: vec![SessionLeaseRef {
                session: w.session,
                agent: w.agent.clone(),
                token: w.token,
                idle_since: idle,
                last_event_at: None,
                pinned: false,
            }],
            stats,
        })
        .await
        .unwrap();
    }

    /// A heartbeat after the session ended: the host reports no session.
    async fn heartbeat_empty(&self, host: &str) {
        self.req(CtlRequest::HostHeartbeat {
            host: host.parse().unwrap(),
            sessions: Vec::new(),
            stats: None,
        })
        .await
        .unwrap();
    }

    async fn commands(&self, host: &str) -> futures::stream::BoxStream<'static, HostCommand> {
        let host: HostName = host.parse().unwrap();
        self.bus
            .serve(&borant_core::subjects::host_cmd(&host))
            .await
            .unwrap()
            .filter_map(|i: borant_core::ports::Incoming| async move {
                serde_json::from_slice(&i.payload).ok()
            })
            .boxed()
    }

    async fn agent_of(&self, id: &AgentId) -> Agent {
        let doc = self.state.get(&keys::agent(id)).await.unwrap().unwrap();
        serde_json::from_slice(&doc.value).unwrap()
    }

    async fn task_of(&self, channel: &ChannelName, id: &borant_core::TaskId) -> Task {
        let doc = self
            .state
            .get(&keys::task(channel, id))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_slice(&doc.value).unwrap()
    }

    async fn kinds(&self) -> Vec<&'static str> {
        let stream = self
            .events
            .read(&EventFilter::default(), false)
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        events.iter().map(kind_of).collect()
    }
}

async fn next_cmd(s: &mut futures::stream::BoxStream<'static, HostCommand>) -> Option<HostCommand> {
    tokio::time::timeout(Duration::from_millis(200), s.next())
        .await
        .ok()
        .flatten()
}

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// A worker bound to a task with `stop_on_close`, holding a claim on another
/// task, running in a session on h1.
async fn fixture() -> (Fx, Registered, Registered, Task, Task) {
    let fx = Fx::new();
    fx.req(CtlRequest::RegisterHost {
        info: HostInfo {
            name: "h1".parse().unwrap(),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 4,
            always_on: false,
            draining: false,
            ssh: None,
            members: Default::default(),
            pool: None,
            tags: vec![],
            bundles: vec![],
            reserve_mem_mb: None,
            sandboxes: vec![],
        },
    })
    .await
    .unwrap();
    let owner = fx.register("h1", "boss", None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    let create = async |title: &str| -> Task {
        serde_json::from_value(
            fx.call(
                Some(&owner),
                ToolCall::TaskCreate {
                    channel: api(),
                    task: NewTask {
                        title: title.into(),
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap(),
        )
        .unwrap()
    };
    let bound = create("ship it").await;
    let other = create("other work").await;
    let w = fx.register("h1", "w1", Some("api")).await;
    // The worker is bound to the task and stops when it closes.
    let mut agent = fx.agent_of(&w.agent).await;
    agent.task = Some(TaskRef {
        channel: api(),
        id: bound.id,
    });
    agent.stop_on_close = true;
    let doc = fx.state.get(&keys::agent(&w.agent)).await.unwrap().unwrap();
    fx.state
        .update(
            &keys::agent(&w.agent),
            serde_json::to_vec(&agent).unwrap().into(),
            doc.revision,
        )
        .await
        .unwrap();
    fx.call(
        Some(&w),
        ToolCall::TaskClaim {
            channel: api(),
            id: other.id,
        },
    )
    .await
    .unwrap();
    (fx, owner, w, bound, other)
}

async fn close(fx: &Fx, owner: &Registered, task: &Task) {
    fx.call(
        Some(owner),
        ToolCall::TaskClose {
            channel: api(),
            id: task.id,
            reason: CloseReason::Done,
            message: "shipped".into(),
        },
    )
    .await
    .expect("close");
}

/// Closing the bound task retires the worker: `agent.retired`, its other claim
/// back to open, and a stop order at the turn end, once.
#[tokio::test]
async fn retire_close_stops_the_bound_worker_and_frees_its_claims() {
    let (fx, owner, w, bound, other) = fixture().await;
    let mut cmds = fx.commands("h1").await;

    close(&fx, &owner, &bound).await;

    let agent = fx.agent_of(&w.agent).await;
    assert!(agent.retired, "the agent is retired");
    assert!(
        fx.kinds().await.contains(&"agent.retired"),
        "the hub published agent.retired"
    );
    // The claim on the other task is back to open.
    let other = fx.task_of(&api(), &other.id).await;
    assert_eq!(other.claimed_by, None);
    assert_eq!(other.status, TaskStatus::Open);
    // The session is in a turn, so no stop yet.
    assert!(next_cmd(&mut cmds).await.is_none());

    // The turn ends: the stop goes out, once.
    let now = fx.now();
    fx.heartbeat("h1", &w, Some(now)).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop, got {other:?}"),
    }
    fx.heartbeat("h1", &w, Some(now)).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the hub sends one stop for the session"
    );
}

/// A session that is already idle stops at once, without waiting for a
/// heartbeat to report a turn end.
#[tokio::test]
async fn retire_an_idle_session_stops_at_once() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    let now = fx.now();
    fx.heartbeat("h1", &w, Some(now)).await;

    close(&fx, &owner, &bound).await;

    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop right away, got {other:?}"),
    }
    // A later turn end does not send a second stop.
    fx.heartbeat("h1", &w, Some(now)).await;
    assert!(next_cmd(&mut cmds).await.is_none());
}

/// A session that the host reports no stats for gives the hub no turn state at
/// all: the retire holds the stop for the safety grace, then sends it anyway,
/// so the session cannot keep its lease and its clone for ever.
#[tokio::test]
async fn retire_stops_a_session_with_no_turn_state_at_the_safety_grace() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    // No heartbeat at all before the close: the hub knows nothing about the
    // turn of the session, only the session that the agent row names.
    close(&fx, &owner, &bound).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the turn may still run"
    );

    // The host reports the session, but no stats: the safety grace runs.
    fx.heartbeat("h1", &w, None).await;
    assert!(next_cmd(&mut cmds).await.is_none(), "the grace runs");
    fx.clock.advance(Duration::from_secs(599));
    fx.heartbeat("h1", &w, None).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "no stop before the safety grace"
    );

    // Past the grace, the stop goes out even though no state ever arrives.
    fx.clock.advance(Duration::from_secs(2));
    fx.heartbeat("h1", &w, None).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop after the grace, got {other:?}"),
    }
    fx.heartbeat("h1", &w, None).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the hub sends one stop for the session"
    );
}

/// A session whose stats say it is not busy (an omp session that waits for its
/// first prompt reports `busy: false` and no `idle_since`) stops at the next
/// heartbeat, so a cancelled idle worker never runs on.
#[tokio::test]
async fn retire_stops_a_not_busy_session_at_the_next_heartbeat() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    // No stats before the close, so the retire cannot know the turn state.
    close(&fx, &owner, &bound).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the turn may still run"
    );

    fx.heartbeat_busy("h1", &w, false).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop at the next heartbeat, got {other:?}"),
    }
    fx.heartbeat_busy("h1", &w, false).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "the hub sends one stop for the session"
    );
}

/// A session whose stats say it is busy keeps its turn: no stop at the safety
/// grace, and the stop at the first heartbeat that reports the turn ended, so
/// a worker that writes its last message is not cut off.
#[tokio::test]
async fn retire_does_not_stop_a_busy_session_at_the_grace() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    // The session is in a turn when the bound task closes.
    fx.heartbeat_busy("h1", &w, true).await;
    close(&fx, &owner, &bound).await;
    assert!(next_cmd(&mut cmds).await.is_none(), "no stop in a turn");

    fx.clock.advance(Duration::from_secs(31));
    fx.heartbeat_busy("h1", &w, true).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "a busy session is not stopped at the grace"
    );
    fx.clock.advance(Duration::from_secs(599));
    fx.heartbeat_busy("h1", &w, true).await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "a busy session is not stopped at the safety grace"
    );

    // The turn ends: the stop goes out.
    fx.heartbeat_busy("h1", &w, false).await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop at the turn end, got {other:?}"),
    }
}

/// A cancel of a session that has not ended its turn says `stopped: false`
/// and when the stop comes, so a reply never claims a stop that did not go out.
#[tokio::test]
async fn retire_cancel_says_stopping_when_the_turn_has_not_ended() {
    let (fx, owner, w, _bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    fx.heartbeat_busy("h1", &w, true).await;

    let reply = fx
        .call(
            Some(&owner),
            ToolCall::Cancel {
                agent: w.agent.clone(),
                close_task: None,
            },
        )
        .await
        .expect("cancel");

    assert_eq!(reply["cancelled"], true);
    assert_eq!(reply["stopped"], false, "no stop went out yet");
    assert_eq!(reply["stopping"], "at the turn end");
    assert!(fx.agent_of(&w.agent).await.retired);
    assert!(next_cmd(&mut cmds).await.is_none(), "no stop in a turn");
}

/// The work dir of a retired agent goes also when a person stopped its session
/// from outside: the host reports no session, so the clone goes without any
/// stop of the hub.
#[tokio::test]
async fn retire_deletes_the_work_dir_of_a_session_stopped_from_outside() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    fx.heartbeat_busy("h1", &w, true).await;
    close(&fx, &owner, &bound).await;
    assert!(next_cmd(&mut cmds).await.is_none(), "the session runs");

    // The owner killed the session: the host no longer reports it.
    fx.heartbeat_empty("h1").await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::DeleteWork { agent }) => assert_eq!(agent, w.agent),
        other => panic!("expected a delete-work order, got {other:?}"),
    }
    fx.heartbeat_empty("h1").await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "one delete for the agent"
    );
}

/// The work dir of a retired agent goes only after its session ended: the
/// stop first, and the delete from a heartbeat that no longer reports it.
#[tokio::test]
async fn retire_deletes_the_work_dir_only_after_the_session_ended() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    let now = fx.now();
    fx.heartbeat("h1", &w, Some(now)).await;
    close(&fx, &owner, &bound).await;

    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, w.session),
        other => panic!("expected a stop first, got {other:?}"),
    }
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "no delete before the session ends"
    );
    // The host still reports the session, so the clone stays.
    fx.heartbeat("h1", &w, Some(now)).await;
    assert!(next_cmd(&mut cmds).await.is_none(), "the session is there");

    // The session ended: one delete for the agent, and only one.
    fx.heartbeat_empty("h1").await;
    match next_cmd(&mut cmds).await {
        Some(HostCommand::DeleteWork { agent }) => assert_eq!(agent, w.agent),
        other => panic!("expected a delete-work order, got {other:?}"),
    }
    fx.heartbeat_empty("h1").await;
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "one delete for the agent"
    );
}

/// A retired worker is never placed again: no takeover, no wake, no move.
#[tokio::test]
async fn retire_never_comes_back() {
    let (fx, owner, w, bound, _other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    close(&fx, &owner, &bound).await;
    let _ = next_cmd(&mut cmds).await;

    // The session ends and its lease expires.
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "stopped".into(),
    })
    .await
    .unwrap();
    fx.clock.advance(Duration::from_secs(600));
    fx.hub.tick().await.unwrap();
    assert!(next_cmd(&mut cmds).await.is_none(), "no takeover");

    // A wake message does not bring it back either.
    let env = borant_core::testing::envelope(
        borant_core::model::Target::Agent(w.agent.clone()),
        "please continue",
    );
    let subject = borant_core::subjects::agent(&w.agent);
    fx.bus.publish(&subject, &env).await.unwrap();
    fx.hub.tick().await.unwrap();
    assert!(next_cmd(&mut cmds).await.is_none(), "no wake");

    // A move does not take it.
    let moved = fx
        .call(
            None,
            ToolCall::Move {
                selector: borant_core::proto::MoveSelector::Agent(w.agent.clone()),
                to: borant_core::proto::MoveTarget {
                    host: Some("h1".parse().unwrap()),
                    harness: None,
                },
                member: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(moved["moving"].as_array().map(Vec::len), Some(0));
}

/// A parked lease of a retired agent never wakes.
#[tokio::test]
async fn retire_a_parked_lease_never_wakes() {
    let (fx, owner, w, bound, other) = fixture().await;
    let mut cmds = fx.commands("h1").await;
    close(&fx, &owner, &bound).await;
    let _ = next_cmd(&mut cmds).await;

    // The host parked the session before the close.
    let doc = fx
        .state
        .get(&keys::lease_agent(&w.agent))
        .await
        .unwrap()
        .unwrap();
    let lease = Lease {
        agent: w.agent.clone(),
        token: w.token,
        state: LeaseState::Released {
            since: fx.now(),
            parked: true,
        },
    };
    fx.state
        .update(
            &keys::lease_agent(&w.agent),
            serde_json::to_vec(&lease).unwrap().into(),
            doc.revision,
        )
        .await
        .unwrap();
    // A claim and a message would wake a parked agent.
    fx.call(
        Some(&w),
        ToolCall::TaskClaim {
            channel: api(),
            id: other.id,
        },
    )
    .await
    .ok();
    let env = borant_core::testing::envelope(
        borant_core::model::Target::Agent(w.agent.clone()),
        "wake up",
    );
    fx.bus
        .publish(&borant_core::subjects::agent(&w.agent), &env)
        .await
        .unwrap();
    fx.clock.advance(Duration::from_secs(600));
    fx.hub.tick().await.unwrap();
    assert!(
        next_cmd(&mut cmds).await.is_none(),
        "a retired agent never wakes"
    );
}
