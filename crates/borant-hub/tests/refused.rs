//! The refuse reasons of a placement (phase 5 spec, "Resources and reject
//! reasons"): one reason per host in the `placement.refused` event and in the
//! spawn record that `borant ps --queued` prints.

use std::sync::Arc;

use borant_core::events::{EventFilter, SwarmEvent, SwarmEventBody, kind_of};
use borant_core::model::{
    ChannelKind, Harness, HostInfo, HostStats, RepoInfo, Sender, SpawnRecord, SpawnSpec, SpawnState,
};
use borant_core::ports::{EventLog, StateStore};
use borant_core::proto::{Actor, ApiError, CtlRequest, Registered, SessionLeaseRef, ToolCall};
use borant_core::rules::roles::Resolved;
use borant_core::{AgentId, ChannelName, HostName, ManualClock, SessionId, Timestamp, keys};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use futures::StreamExt;
use serde_json::Value;

const NOW: Timestamp = Timestamp(1_000_000_000);

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    log: Arc<MemoryEventLog>,
    clock: Arc<ManualClock>,
}

fn name(host: &str) -> HostName {
    host.parse().unwrap()
}

fn repo() -> RepoInfo {
    RepoInfo {
        remote_url: "git://git/demo.git".into(),
        branch: "main".into(),
        subdir: String::new(),
    }
}

impl Fx {
    fn new() -> Self {
        let state = MemoryState::new();
        let clock = Arc::new(ManualClock::new(NOW));
        let log = Arc::new(MemoryEventLog::new());
        let hub = Hub::new(
            Arc::new(state.clone()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock.clone(),
            HubConfig::default(),
        )
        .with_events(log.clone());
        Self {
            hub: Arc::new(hub),
            state,
            log,
            clock,
        }
    }

    async fn req(&self, req: CtlRequest) -> Result<Value, ApiError> {
        self.hub.handle(req).await.into_result()
    }

    async fn host(&self, host: &str, max_agents: u32) {
        self.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name(host),
                harnesses: vec![Harness::Fake],
                launcher: "process".into(),
                max_agents,
                ..Default::default()
            },
        })
        .await
        .expect("register the host");
    }

    async fn register(&self, host: &str, agent: &str) -> Registered {
        let v = self
            .req(CtlRequest::RegisterSession(
                borant_core::proto::RegisterSession {
                    session: SessionId::new(),
                    host: name(host),
                    harness: Harness::Fake,
                    agent: Some(agent.parse().unwrap()),
                    repo: Some(repo()),
                    ..Default::default()
                },
            ))
            .await
            .expect("register the session");
        serde_json::from_value(v).expect("a registration")
    }

    async fn call(&self, who: &Registered, call: ToolCall) -> Result<Value, ApiError> {
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
    }

    /// A heartbeat, with `who` when the host runs a session, and its stats.
    async fn heartbeat(&self, host: &str, who: Option<&Registered>, stats: Option<HostStats>) {
        let sessions: Vec<SessionLeaseRef> = who
            .map(|r| {
                vec![SessionLeaseRef {
                    session: r.session,
                    agent: r.agent.clone(),
                    token: r.token,
                    idle_since: None,
                    last_event_at: None,
                    pinned: false,
                }]
            })
            .unwrap_or_default();
        self.req(CtlRequest::HostHeartbeat {
            host: name(host),
            sessions,
            stats,
            launching: Vec::new(),
        })
        .await
        .expect("heartbeat");
    }

    async fn events(&self) -> Vec<SwarmEvent> {
        self.log
            .read(&EventFilter::default(), false)
            .await
            .expect("read the log")
            .collect()
            .await
    }

    /// A queued spawn record for `agent`, as the spawn queue writes it.
    async fn queue(&self, agent: &AgentId, channel: &ChannelName) {
        let record = SpawnRecord {
            agent: agent.clone(),
            spec: SpawnSpec {
                channel: channel.clone(),
                prompt: "work".into(),
                harness: None,
                host: None,
                name: Some(agent.clone()),
                model: None,
                role: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                bundles: Vec::new(),
            },
            resolved: Resolved::default(),
            by: Sender::Agent("boss".parse().unwrap()),
            queued_at: NOW,
            priority: 0,
            state: SpawnState::Queued,
            attempts: 0,
            last_reject: Vec::new(),
            queue_reason: None,
            launch_failures: Vec::new(),
        };
        let bytes = serde_json::to_vec(&record).unwrap();
        self.state
            .create(&keys::spawn(agent), bytes.into())
            .await
            .expect("queue the spawn");
    }

    async fn record(&self, agent: &AgentId) -> SpawnRecord {
        let e = self
            .state
            .get(&keys::spawn(agent))
            .await
            .expect("read the queue")
            .expect("a queued spawn");
        serde_json::from_slice(&e.value).expect("a spawn record")
    }

    fn stats(mem_avail_mb: u64) -> HostStats {
        HostStats {
            version: "test".into(),
            mem_avail_mb: Some(mem_avail_mb),
            ..Default::default()
        }
    }
}

/// A channel that `owner` created, where a spawn can happen.
async fn channel(fx: &Fx, owner: &Registered) -> ChannelName {
    let channel: ChannelName = "api".parse().unwrap();
    fx.call(
        owner,
        ToolCall::ChannelCreate {
            name: channel.clone(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
        },
    )
    .await
    .expect("create the channel");
    channel
}

async fn spawn(fx: &Fx, owner: &Registered, channel: &ChannelName, agent: &AgentId) -> Value {
    fx.call(
        owner,
        ToolCall::Spawn {
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            bundles: Vec::new(),
            channel: channel.clone(),
            prompt: "work".into(),
            harness: None,
            host: None,
            name: Some(agent.clone()),
            model: None,
            role: None,
        },
    )
    .await
    .expect("spawn")
}

#[tokio::test]
async fn refused_a_spawn_that_fits_nowhere_names_every_host() {
    let fx = Fx::new();
    // `a` is up and full: its one slot holds the owner.
    fx.host("a", 1).await;
    let owner = fx.register("a", "boss").await;
    let channel = channel(&fx, &owner).await;
    fx.heartbeat("a", Some(&owner), None).await;
    // `b` is up, has room, and reports too little free memory for its reserve
    // of 1024 MB.
    fx.host("b", 4).await;
    fx.heartbeat("b", None, Some(Fx::stats(700))).await;

    let helper: AgentId = "helper".parse().unwrap();
    fx.queue(&helper, &channel).await;
    let spawned = spawn(&fx, &owner, &channel, &helper).await;
    assert_eq!(spawned["agent"]["placed"], false, "{spawned}");

    // The queue record carries one reason per host, in the wording of
    // `borant ps --queued`.
    let record = fx.record(&helper).await;
    assert_eq!(
        record.last_reject,
        vec![
            (name("a"), "full (1/1)".to_owned()),
            (
                name("b"),
                "low memory (700 MB free, 1024 reserved)".to_owned()
            ),
        ]
    );

    // The event carries the same list.
    let events = fx.events().await;
    let refused: Vec<&SwarmEvent> = events
        .iter()
        .filter(|e| kind_of(e) == "placement.refused")
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    let SwarmEventBody::PlacementRefused { rejected, .. } = &refused[0].body else {
        panic!("not a refusal: {:?}", refused[0].body);
    };
    let got: Vec<(String, String)> = rejected
        .iter()
        .map(|r| (r.host.to_string(), r.why.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("a".to_owned(), "full (1/1)".to_owned()),
            (
                "b".to_owned(),
                "low memory (700 MB free, 1024 reserved)".to_owned()
            ),
        ]
    );
    assert_eq!(refused[0].agent, Some(helper));
}

#[tokio::test]
async fn refused_a_host_without_stats_still_takes_the_agent() {
    let fx = Fx::new();
    // The host is up but reports no stats: the memory rule cannot judge it, so
    // it must not refuse the agent.
    fx.host("c", 4).await;
    let owner = fx.register("c", "boss").await;
    let channel = channel(&fx, &owner).await;
    fx.heartbeat("c", Some(&owner), None).await;

    let helper: AgentId = "solo".parse().unwrap();
    fx.queue(&helper, &channel).await;
    let spawned = spawn(&fx, &owner, &channel, &helper).await;
    assert_eq!(spawned["agent"]["placed"], true, "{spawned}");
    assert!(
        fx.record(&helper).await.last_reject.is_empty(),
        "a placed spawn has no reject"
    );
    assert!(
        !fx.events()
            .await
            .iter()
            .any(|e| kind_of(e) == "placement.refused"),
        "no refusal"
    );
    let _ = fx.clock;
}
