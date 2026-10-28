//! The hub's swarm events: the one place that publishes, so the rule of the
//! lock is one line to review.
//!
//! Every event is published after the write that it reports, and never while
//! `mem` is held. A handler that holds the guard *queues* its events in
//! `Memory::outbox`; `handle` and the lease tick drain the queue after they
//! drop the guard. A caller that holds no lock publishes at once. A failed
//! publish is a log line, not an error, because the write already happened.

use std::sync::Arc;

use borant_core::events::{Severity, SwarmEvent, SwarmEventBody};
use borant_core::ports::EventLog;
use borant_core::{AgentId, HostName, Timestamp};

use crate::{Hub, Memory};

impl Hub {
    /// Sets the event log. Without one the hub writes no events, as before.
    pub fn with_events(mut self, log: Arc<dyn EventLog>) -> Self {
        self.events = Some(log);
        self
    }

    /// A hub event at `at`.
    pub(crate) fn event(
        &self,
        at: Timestamp,
        severity: Severity,
        body: SwarmEventBody,
    ) -> SwarmEvent {
        SwarmEvent::hub(at, severity, body)
    }

    /// Queues one event for the flush that follows the write. For a caller
    /// that holds `mem`; it must drop the guard before [`Hub::flush`].
    pub(crate) fn queue(mem: &mut Memory, event: SwarmEvent) {
        mem.outbox.push(event);
    }

    /// Publishes the queued events. The caller holds no lock.
    pub(crate) async fn flush(&self) {
        let queued = std::mem::take(&mut self.mem.lock().await.outbox);
        for event in queued {
            self.publish(event).await;
        }
    }

    /// Publishes one event at once. The caller holds no lock.
    pub(crate) async fn publish(&self, event: SwarmEvent) {
        let Some(log) = &self.events else {
            return;
        };
        if let Err(e) = log.publish(&event).await {
            tracing::warn!(event = %event.id, "event publish failed: {e}");
        }
    }

    /// The event of a lease change, about its agent and its host.
    pub(crate) fn lease_event(
        &self,
        at: Timestamp,
        severity: Severity,
        agent: &AgentId,
        host: Option<&HostName>,
        body: SwarmEventBody,
    ) -> SwarmEvent {
        let event = self.event(at, severity, body).about_agent(agent.clone());
        match host {
            Some(host) => event.about_host(host.clone()),
            None => event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use borant_core::events::{EventFilter, kind_of};
    use borant_core::model::{
        Agent, ChannelKind, Harness, HostInfo, MemberBundle, MemberState, MoveReason, NewTask,
        RepoInfo, Task, TaskKind, TaskPatch, TaskStatus,
    };
    use borant_core::ports::{PortError, PortResult};
    use borant_core::proto::{
        Actor, CtlRequest, MoveTarget, RegisterSession, Registered, SessionLeaseRef, ToolCall,
    };
    use borant_core::rules::pool::{ElasticHost, Phase, PoolConfig};
    use borant_core::{ChannelName, Clock, ManualClock, SessionId};
    use borant_memory::{MemoryBus, MemoryEventLog, MemoryProvisioner, MemorySession, MemoryState};
    use futures::StreamExt;

    use crate::HubConfig;
    use crate::PendingMove;
    use crate::elastic::ElasticConfig;

    const NOW: Timestamp = Timestamp(1_000_000_000);

    struct Fx {
        hub: Arc<Hub>,
        log: Arc<MemoryEventLog>,
        clock: Arc<ManualClock>,
    }

    fn fx() -> Fx {
        fx_with(None)
    }

    fn fx_with(elastic: Option<ElasticConfig>) -> Fx {
        let clock = Arc::new(ManualClock::new(NOW));
        let log = Arc::new(MemoryEventLog::new());
        let hub = Hub::new(
            Arc::new(MemoryState::new()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock.clone(),
            HubConfig {
                elastic,
                ..HubConfig::default()
            },
        )
        .with_events(log.clone());
        Fx {
            hub: Arc::new(hub),
            log,
            clock,
        }
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
        async fn req(
            &self,
            req: CtlRequest,
        ) -> Result<serde_json::Value, borant_core::proto::ApiError> {
            self.hub.handle(req).await.into_result()
        }

        async fn events(&self) -> Vec<SwarmEvent> {
            let mut stream = self
                .log
                .read(&EventFilter::default(), false)
                .await
                .expect("read the log");
            let mut out = Vec::new();
            while let Some(event) = stream.next().await {
                out.push(event);
            }
            out
        }

        async fn kinds(&self) -> Vec<&'static str> {
            self.events().await.iter().map(kind_of).collect()
        }

        async fn host(&self, host: &str, always_on: bool) {
            self.host_in(host, always_on, None).await;
        }

        async fn host_in(&self, host: &str, always_on: bool, pool: Option<&str>) {
            self.req(CtlRequest::RegisterHost {
                info: HostInfo {
                    name: name(host),
                    harnesses: vec![Harness::Fake],
                    launcher: "process".into(),
                    max_agents: 4,
                    always_on,
                    pool: pool.map(|p| p.parse().unwrap()),
                    ..Default::default()
                },
            })
            .await
            .expect("register the host");
        }

        async fn register(&self, host: &str, agent: &str) -> Registered {
            self.register_in(host, agent, None).await
        }

        async fn register_in(&self, host: &str, agent: &str, channel: Option<&str>) -> Registered {
            let v = self
                .req(CtlRequest::RegisterSession(RegisterSession {
                    session: SessionId::new(),
                    host: name(host),
                    harness: Harness::Fake,
                    agent: Some(agent.parse().unwrap()),
                    channel: channel.map(|c| c.parse().unwrap()),
                    repo: Some(repo()),
                    ..Default::default()
                }))
                .await
                .expect("register the session");
            serde_json::from_value(v).expect("a registration")
        }

        async fn call(
            &self,
            who: &Registered,
            call: ToolCall,
        ) -> Result<serde_json::Value, borant_core::proto::ApiError> {
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

        async fn heartbeat(&self, host: &str, who: &Registered) {
            self.heartbeat_idle(host, who, None).await;
        }

        async fn heartbeat_idle(&self, host: &str, who: &Registered, idle: Option<Timestamp>) {
            self.req(CtlRequest::HostHeartbeat {
                host: name(host),
                sessions: vec![SessionLeaseRef {
                    session: who.session,
                    agent: who.agent.clone(),
                    token: who.token,
                    idle_since: idle,
                    last_event_at: None,
                    pinned: false,
                }],
                stats: None,
                launching: Vec::new(),
            })
            .await
            .expect("heartbeat");
        }

        fn advance(&self, secs: u64) {
            self.clock.advance(Duration::from_secs(secs));
        }

        /// A channel with one task, owned by `owner`.
        async fn channel_with_task(&self, owner: &Registered) -> (Task, ChannelName) {
            let channel: ChannelName = "api".parse().unwrap();
            self.call(
                owner,
                ToolCall::ChannelCreate {
                    name: channel.clone(),
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
            let v = self
                .call(
                    owner,
                    ToolCall::TaskCreate {
                        channel: channel.clone(),
                        task: NewTask {
                            title: "do the work".into(),
                            body: String::new(),
                            blocked_by: Vec::new(),
                            parent: None,
                            kind: TaskKind::Task,
                            issue: None,
                            issue_target: None,
                            properties: Default::default(),
                            deadline: None,
                            max_attempts: None,
                        },
                    },
                )
                .await
                .expect("create the task");
            (serde_json::from_value(v).unwrap(), channel)
        }
    }

    /// The named kinds of a log, in the order they were published.
    fn kinds_of(events: &[SwarmEvent], wanted: &[&str]) -> Vec<String> {
        events
            .iter()
            .map(kind_of)
            .filter(|k| wanted.contains(k))
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn events_a_takeover_publishes_expiry_choice_and_launch_in_order() {
        let fx = fx();
        fx.host("a", false).await;
        fx.host("b", false).await;
        fx.register("a", "w").await;

        // The host of the agent stops heartbeating and its lease expires.
        fx.advance(31);
        fx.hub.tick().await.expect("the expiry");
        // Only the second host comes back, and the agent waits for a takeover.
        let w: AgentId = "w".parse().unwrap();
        {
            let mut mem = fx.hub.mem.lock().await;
            mem.hosts_seen.insert(name("b"), fx.clock.now());
            mem.pending_moves.insert(
                w.clone(),
                PendingMove {
                    target: MoveTarget::default(),
                    reason: MoveReason::Takeover,
                    prompt: None,
                },
            );
        }
        fx.advance(130);
        fx.hub
            .mem
            .lock()
            .await
            .hosts_seen
            .insert(name("b"), fx.clock.now());
        fx.hub.tick().await.expect("the takeover");

        let events = fx.events().await;
        let all: Vec<&str> = events.iter().map(kind_of).collect();
        assert_eq!(
            kinds_of(
                &events,
                &["lease.expired", "placement.chosen", "move.launched"]
            ),
            vec!["lease.expired", "placement.chosen", "move.launched"],
            "{all:?}"
        );
        let launched = events
            .iter()
            .find(|e| kind_of(e) == "move.launched")
            .expect("a launch");
        assert!(
            matches!(&launched.body, SwarmEventBody::MoveLaunched { to_host, .. } if *to_host == name("b")),
            "{launched:?}"
        );
        assert_eq!(launched.agent.as_ref(), Some(&w));
    }

    #[tokio::test]
    async fn events_a_renewal_publishes_nothing() {
        let fx = fx();
        fx.host("a", false).await;
        let reg = fx.register("a", "w").await;
        let before = fx.events().await.len();
        fx.heartbeat("a", &reg).await;
        assert_eq!(fx.events().await.len(), before, "a renewal makes no event");
    }

    #[tokio::test]
    async fn events_a_claim_publishes_one_task_status() {
        let fx = fx();
        fx.host("a", false).await;
        let owner = fx.register("a", "boss").await;
        let (task, channel) = fx.channel_with_task(&owner).await;
        let w = fx.register("a", "w").await;
        let before = fx.events().await.len();

        fx.call(
            &w,
            ToolCall::TaskClaim {
                channel: channel.clone(),
                id: task.id,
            },
        )
        .await
        .expect("claim");

        let events = fx.events().await;
        let statuses: Vec<&SwarmEvent> = events[before..]
            .iter()
            .filter(|e| kind_of(e) == "task.status")
            .collect();
        assert_eq!(statuses.len(), 1, "one task.status for one claim");
        assert!(
            matches!(
                &statuses[0].body,
                SwarmEventBody::TaskStatus { from: TaskStatus::Open, to: TaskStatus::InProgress, claimed_by: Some(by), .. } if *by == w.agent
            ),
            "{:?}",
            statuses[0]
        );
        assert_eq!(statuses[0].task, Some(task.id));

        // A title-only update is neither a status nor a claim change.
        let before = fx.events().await.len();
        fx.call(
            &w,
            ToolCall::TaskUpdate {
                channel,
                id: task.id,
                patch: TaskPatch {
                    title: Some("a better title".into()),
                    body: None,
                    status: None,
                    attention_msg: None,
                    blocked_by: None,
                    parent: None,
                    issue: None,
                    issue_target: None,
                    properties: None,
                    deadline: None,
                    max_attempts: None,
                },
            },
        )
        .await
        .expect("update");
        let after = fx.events().await;
        assert!(
            !after[before..].iter().any(|e| kind_of(e) == "task.status"),
            "a title update publishes no task.status"
        );
    }

    #[tokio::test]
    async fn events_a_host_registers_and_drains() {
        let fx = fx();
        fx.host("a", false).await;
        assert_eq!(fx.kinds().await, vec!["host.registered"]);
        fx.req(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::Drain { host: name("a") },
        })
        .await
        .expect("drain");
        assert_eq!(
            fx.kinds().await,
            vec!["host.registered", "host.draining"],
            "the drain follows the registration"
        );
        let draining = fx
            .events()
            .await
            .into_iter()
            .find(|e| kind_of(e) == "host.draining")
            .expect("a drain");
        assert_eq!(draining.host, Some(name("a")));
    }

    #[tokio::test]
    async fn events_a_spawn_publishes_agent_spawned() {
        let fx = fx();
        fx.host("a", false).await;
        let owner = fx.register("a", "boss").await;
        let (_task, channel) = fx.channel_with_task(&owner).await;
        let before = fx.events().await.len();
        fx.call(
            &owner,
            ToolCall::Spawn {
                key: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                channel,
                prompt: "work".into(),
                harness: None,
                host: None,
                name: Some("helper".parse().unwrap()),
                model: None,
                role: None,
                bundles: Vec::new(),
            },
        )
        .await
        .expect("spawn");
        let events = fx.events().await;
        let spawned: Vec<&SwarmEvent> = events[before..]
            .iter()
            .filter(|e| kind_of(e) == "agent.spawned")
            .collect();
        assert_eq!(
            spawned.len(),
            1,
            "{:?}",
            events.iter().map(kind_of).collect::<Vec<_>>()
        );
        assert_eq!(spawned[0].agent, Some("helper".parse().unwrap()));
        assert!(
            matches!(&spawned[0].body, SwarmEventBody::AgentSpawned { by, .. } if *by == borant_core::model::Sender::Agent(owner.agent.clone()))
        );
    }

    #[tokio::test]
    async fn events_a_refused_placement_says_why() {
        let fx = fx();
        fx.host("a", false).await;
        let owner = fx.register("a", "boss").await;
        let (_task, channel) = fx.channel_with_task(&owner).await;
        let before = fx.events().await.len();
        // The only host runs another harness.
        fx.call(
            &owner,
            ToolCall::Spawn {
                key: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                channel,
                prompt: "work".into(),
                harness: Some(Harness::ClaudeCode),
                host: None,
                name: Some("helper".parse().unwrap()),
                model: None,
                role: None,
                bundles: Vec::new(),
            },
        )
        .await
        .expect("spawn");
        let events = fx.events().await;
        let refused: Vec<&SwarmEvent> = events[before..]
            .iter()
            .filter(|e| kind_of(e) == "placement.refused")
            .collect();
        assert_eq!(
            refused.len(),
            1,
            "{:?}",
            events.iter().map(kind_of).collect::<Vec<_>>()
        );
        assert!(
            matches!(&refused[0].body, SwarmEventBody::PlacementRefused { rejected, .. } if rejected.iter().any(|r| r.host == name("a") && r.why == "no harness")),
            "{:?}",
            refused[0]
        );
    }

    #[tokio::test]
    async fn events_a_park_order_publishes_agent_parked() {
        let fx = fx();
        fx.host_in("p1", false, Some("workers")).await;
        let owner = fx.register("p1", "boss").await;
        let (_task, channel) = fx.channel_with_task(&owner).await;
        let w = fx.register_in("p1", "w", Some(&channel.to_string())).await;
        // The agent has been idle for ten minutes.
        let idle = fx.clock.now().minus(Duration::from_secs(600));
        fx.heartbeat_idle("p1", &w, Some(idle)).await;
        let before = fx.events().await.len();

        let parked = fx
            .hub
            .park_idle(Some(&"workers".parse().unwrap()), Duration::from_secs(60))
            .await
            .expect("park");
        assert_eq!(parked, vec![w.agent.clone()]);

        let events = fx.events().await;
        let parks: Vec<&SwarmEvent> = events[before..]
            .iter()
            .filter(|e| kind_of(e) == "agent.parked")
            .collect();
        assert_eq!(
            parks.len(),
            1,
            "{:?}",
            events.iter().map(kind_of).collect::<Vec<_>>()
        );
        assert!(
            matches!(&parks[0].body, SwarmEventBody::AgentParked { session, git_ref: None } if *session == w.session)
        );
    }

    fn workers_pool() -> PoolConfig {
        PoolConfig {
            name: "workers".parse().unwrap(),
            provisioner: "cloud".to_owned(),
            size: "small".to_owned(),
            region: "r1".to_owned(),
            image: None,
            name_prefix: "e".to_owned(),
            tags: Vec::new(),
            harnesses: vec![Harness::ClaudeCode],
            agents_per_host: 2,
            max_hosts: 2,
            idle_timeout: Duration::from_secs(20 * 60),
            boot_timeout: Duration::from_secs(10 * 60),
            lost_timeout: Duration::from_secs(10 * 60),
            backoff_until: None,
            monthly_budget_usd: None,
            price_hour_usd: Some(0.1),
        }
    }

    #[tokio::test]
    async fn events_an_elastic_create_and_delete_are_published() {
        let prov = Arc::new(MemoryProvisioner::new());
        let elastic = ElasticConfig {
            tick: Duration::from_secs(5),
            lost_timeout: Duration::from_secs(10 * 60),
            create_backoff: Duration::from_secs(5 * 60),
            monthly_budget_usd: None,
            pools: vec![workers_pool()],
            provisioners: BTreeMap::from([(
                "cloud".to_owned(),
                prov.clone() as Arc<dyn borant_core::ports::Provisioner>,
            )]),
        };
        let fx = fx_with(Some(elastic));
        // An agent that waits for a host orders one.
        let id: AgentId = "w".parse().unwrap();
        fx.hub
            .store
            .upsert(
                &borant_core::keys::agent(&id),
                &Agent {
                    id: id.clone(),
                    spawn_key: None,
                    task: None,
                    stop_on_close: false,
                    continues: None,
                    retired: false,
                    preferred_harness: Harness::ClaudeCode,
                    repo: Some(repo()),
                    movable: true,
                    channels: Vec::new(),
                    session: None,
                    host: None,
                    harness_session_id: None,
                    created: NOW,
                    model: None,
                    model_harness: None,
                    project: None,
                    members: Default::default(),
                    pinned: false,
                    role: None,
                    collection: None,
                    bundles: Vec::new(),
                    parent: None,
                    paused: None,
                },
            )
            .await
            .expect("the agent");
        fx.hub.mem.lock().await.pending_moves.insert(
            id,
            PendingMove {
                target: MoveTarget::default(),
                reason: MoveReason::Spawn,
                prompt: None,
            },
        );

        fx.hub.tick_elastic().await.expect("the first pass");
        let created = fx
            .events()
            .await
            .into_iter()
            .find(|e| kind_of(e) == "elastic.created")
            .expect("a create");
        assert_eq!(created.host, Some(name("e1")));

        // A draining host with no agent is deleted.
        fx.hub
            .store
            .upsert(
                &borant_core::keys::elastic(&name("e1")),
                &ElasticHost {
                    name: name("e1"),
                    pool: "workers".parse().unwrap(),
                    provider_id: borant_core::ports::provision::ProviderHostId(
                        "workers-e1".to_owned(),
                    ),
                    phase: Phase::Draining,
                    created: NOW.minus(Duration::from_secs(3600)),
                    last_busy: NOW.minus(Duration::from_secs(1800)),
                    billing_step_end: NOW.minus(Duration::from_secs(60)),
                    last_seen: NOW,
                },
            )
            .await
            .expect("the key");
        fx.advance(6);
        fx.hub.tick_elastic().await.expect("the second pass");
        let deleted = fx
            .events()
            .await
            .into_iter()
            .find(|e| kind_of(e) == "elastic.deleted")
            .expect("a delete");
        assert_eq!(deleted.host, Some(name("e1")));
    }

    #[tokio::test]
    async fn events_a_claim_timeout_publishes_claims_released() {
        let fx = fx();
        fx.host("a", false).await;
        let owner = fx.register("a", "boss").await;
        let (task, channel) = fx.channel_with_task(&owner).await;
        let w = fx.register("a", "w").await;
        fx.call(
            &w,
            ToolCall::TaskClaim {
                channel: channel.clone(),
                id: task.id,
            },
        )
        .await
        .expect("claim");

        // The session dies, its lease expires, and no host takes the agent.
        fx.advance(31);
        fx.hub.tick().await.expect("the expiry");
        fx.advance(1801);
        fx.hub.tick().await.expect("the claim timeout");

        let events = fx.events().await;
        let released = events
            .iter()
            .find(|e| kind_of(e) == "claims.released")
            .unwrap_or_else(|| panic!("{:?}", events.iter().map(kind_of).collect::<Vec<_>>()));
        assert!(matches!(
            &released.body,
            SwarmEventBody::ClaimsReleased { tasks: 1, .. }
        ));
        assert_eq!(released.agent, Some(w.agent.clone()));
    }

    #[tokio::test]
    async fn events_a_member_limit_is_published() {
        let fx = fx();
        let bundle: borant_core::BundleName = "claude_auth".parse().unwrap();
        let members: Vec<borant_core::MemberName> =
            vec!["a".parse().unwrap(), "b".parse().unwrap()];
        fx.req(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name("a"),
                harnesses: vec![Harness::ClaudeCode],
                launcher: "process".into(),
                max_agents: 4,
                members: BTreeMap::from([(
                    bundle.clone(),
                    MemberBundle {
                        members: members.clone(),
                        harnesses: vec![Harness::ClaudeCode],
                    },
                )]),
                ..Default::default()
            },
        })
        .await
        .expect("the host");
        // Every member is limited for the next hour.
        for member in &members {
            fx.hub
                .store
                .upsert(
                    &crate::members::key(&bundle, member),
                    &MemberState {
                        bundle: bundle.clone(),
                        member: member.clone(),
                        limited_until: Some(NOW.plus(Duration::from_secs(3600))),
                        waiting: Vec::new(),
                        restarts: Vec::new(),
                    },
                )
                .await
                .expect("the state");
        }
        let owner = fx.register("a", "boss").await;
        let (_task, channel) = fx.channel_with_task(&owner).await;
        let before = fx.events().await.len();
        fx.call(
            &owner,
            ToolCall::Spawn {
                key: None,
                task: None,
                stop_on_close: None,
                continues: None,
                priority: None,
                channel,
                prompt: "work".into(),
                harness: Some(Harness::ClaudeCode),
                host: None,
                name: Some("helper".parse().unwrap()),
                model: None,
                role: None,
                bundles: Vec::new(),
            },
        )
        .await
        .expect("spawn");

        let events = fx.events().await;
        let limited: Vec<&SwarmEvent> = events[before..]
            .iter()
            .filter(|e| kind_of(e) == "member.limited")
            .collect();
        assert_eq!(
            limited.len(),
            1,
            "{:?}",
            events.iter().map(kind_of).collect::<Vec<_>>()
        );
        assert!(
            matches!(&limited[0].body, SwarmEventBody::MemberLimited { bundle: b, .. } if *b == bundle)
        );
    }

    /// A log that always fails, for the rule that an event never fails a
    /// request.
    #[derive(Debug)]
    struct FailingLog;

    #[async_trait::async_trait]
    impl EventLog for FailingLog {
        async fn publish(&self, _event: &SwarmEvent) -> PortResult<()> {
            Err(PortError::other("the log is down"))
        }

        async fn read(
            &self,
            _filter: &EventFilter,
            _follow: bool,
        ) -> PortResult<futures::stream::BoxStream<'static, SwarmEvent>> {
            Ok(futures::stream::empty().boxed())
        }
    }

    #[tokio::test]
    async fn events_a_failed_publish_does_not_fail_the_request() {
        let hub = Arc::new(
            Hub::new(
                Arc::new(MemoryState::new()),
                Arc::new(MemoryBus::new()),
                Arc::new(MemorySession::new()),
                Arc::new(ManualClock::new(NOW)),
                HubConfig::default(),
            )
            .with_events(Arc::new(FailingLog)),
        );
        hub.handle(CtlRequest::RegisterHost {
            info: HostInfo {
                name: name("a"),
                harnesses: vec![Harness::Fake],
                launcher: "process".into(),
                max_agents: 4,
                ..Default::default()
            },
        })
        .await
        .into_result()
        .expect("the write stands, the publish is a log line");
        let host = hub
            .store
            .get::<HostInfo>(&borant_core::keys::host(&name("a")))
            .await
            .expect("read the host");
        assert!(host.is_some(), "the host is stored");
    }
}
