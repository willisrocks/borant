//! Every kind of swarm event round-trips, and a kind that this build does not
//! know fails clearly instead of being dropped.

use borant_core::events::{Severity, SwarmEvent, SwarmEventBody, kind_of};
use borant_core::model::{Harness, MoveReason, Sender, TaskStatus};
use borant_core::ports::{ChangeState, CheckState, ReviewState};
use borant_core::{
    AgentId, BundleName, ChannelName, EventId, FencingToken, HostName, MemberName, MessageId,
    PoolName, SessionId, TaskId, Timestamp,
};

fn id<T: std::str::FromStr>(text: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    text.parse().expect("a valid id")
}

fn session() -> SessionId {
    SessionId::new()
}

/// One body of each kind. A new kind must be added here.
fn every_kind() -> Vec<SwarmEventBody> {
    let host: HostName = id("h1");
    let agent: AgentId = id("a1");
    let bundle: BundleName = id("claude_auth");
    let member: MemberName = id("a");
    let pool: PoolName = id("workers");
    let now = Timestamp(1_000);
    vec![
        SwarmEventBody::HubStarted {
            instance: "hub-1".into(),
            token: 3,
            quiet_secs: 30,
        },
        SwarmEventBody::HubSteppedDown {
            instance: "hub-1".into(),
            token: 3,
        },
        SwarmEventBody::HubMaintenance {
            instance: "hub-1".into(),
            on: true,
        },
        SwarmEventBody::HostRegistered {
            last_seen: now,
            always_on: true,
            agents: 2,
        },
        SwarmEventBody::HostOnline {
            last_seen: now,
            always_on: true,
            agents: 2,
        },
        SwarmEventBody::HostOffline {
            last_seen: now,
            always_on: true,
            agents: 0,
        },
        SwarmEventBody::HostDraining { agents: 2 },
        SwarmEventBody::HubUnreachable { since: now },
        SwarmEventBody::LeaseAcquired {
            session: session(),
            token: FencingToken(1),
        },
        SwarmEventBody::LeaseReleased {
            session: session(),
            token: FencingToken(2),
        },
        SwarmEventBody::LeaseReclaimed {
            session: session(),
            token: FencingToken(3),
        },
        SwarmEventBody::LeaseExpired {
            session: session(),
            token: FencingToken(4),
        },
        SwarmEventBody::LeaseReserved {
            reason: MoveReason::Takeover,
            until: now,
        },
        SwarmEventBody::LeaseReservationEnded {
            reason: MoveReason::Spawn,
            until: now,
        },
        SwarmEventBody::SessionStarted {
            session: session(),
            harness: Harness::ClaudeCode,
        },
        SwarmEventBody::SessionEnded {
            session: session(),
            harness: Harness::Omp,
            reason: "done".into(),
        },
        SwarmEventBody::SessionPaused {
            why: "limit".into(),
        },
        SwarmEventBody::SessionFenced {
            why: "moved".into(),
        },
        SwarmEventBody::AgentSpawned {
            by: borant_core::model::Sender::Agent(agent.clone()),
            harness: Harness::Omp,
        },
        SwarmEventBody::AgentPruned {
            by: Some(agent.clone()),
            idle_days: 3.5,
        },
        SwarmEventBody::AgentParked {
            session: session(),
            git_ref: Some("wip/x".into()),
        },
        SwarmEventBody::PlacementChosen {
            harness: Harness::Pi,
            reason: MoveReason::Planned,
        },
        SwarmEventBody::PlacementRefused {
            reason: MoveReason::Takeover,
            rejected: vec![borant_core::events::RejectedHost {
                host: host.clone(),
                why: "no repo access".into(),
            }],
        },
        SwarmEventBody::MoveRequested {
            from_host: Some(host.clone()),
            to_host: id("h2"),
            reason: MoveReason::Planned,
        },
        SwarmEventBody::MoveLaunched {
            from_host: None,
            to_host: host.clone(),
            reason: MoveReason::Spawn,
        },
        SwarmEventBody::MoveReady {
            from_host: Some(host.clone()),
            to_host: id("h2"),
            reason: MoveReason::Takeover,
        },
        SwarmEventBody::MoveFailed {
            from_host: Some(host.clone()),
            to_host: id("h2"),
            reason: MoveReason::Planned,
            error: "clone failed".into(),
        },
        SwarmEventBody::MoveStaleSaved {
            git_ref: "refs/borant/stale".into(),
        },
        SwarmEventBody::ClaimsReleased {
            tasks: 4,
            after_secs: 1800,
        },
        SwarmEventBody::TaskStatus {
            from: TaskStatus::InProgress,
            to: TaskStatus::Stuck,
            claimed_by: Some(agent.clone()),
            attention_msg: Some("quiet".into()),
        },
        SwarmEventBody::DeliveryAckFailed {
            message: MessageId::new(),
            error: "timeout".into(),
            count: 2,
        },
        SwarmEventBody::DeliveryAppendFailed {
            message: MessageId::new(),
            error: "disk".into(),
            count: 1,
        },
        SwarmEventBody::DeliveryRedelivered {
            message: MessageId::new(),
            deliveries: 3,
            count: 1,
        },
        SwarmEventBody::DeliveryWakeLimited {
            message: MessageId::new(),
            count: 5,
        },
        SwarmEventBody::DeliveryBadEnvelope {
            error: "bad json".into(),
            count: 1,
        },
        SwarmEventBody::MemberLimited {
            bundle: bundle.clone(),
            member: member.clone(),
            until: Some(now),
        },
        SwarmEventBody::MemberReset { bundle, member },
        SwarmEventBody::ElasticCreated { pool: pool.clone() },
        SwarmEventBody::ElasticDeleted { pool },
        SwarmEventBody::AgentPaused {
            by: Sender::Human,
            why: "cap: 200 turns today".into(),
        },
        SwarmEventBody::AgentResumed,
        SwarmEventBody::ChangeUpdated {
            state: ChangeState::Open,
            head: "abc123".into(),
            checks: CheckState::Fail {
                failed: vec!["ci".into()],
            },
            review: ReviewState::ChangesRequested,
        },
        SwarmEventBody::AlertFired {
            rule: "needs_human".into(),
            subject: "api/01".into(),
        },
        SwarmEventBody::AlertResolved {
            rule: "needs_human".into(),
            subject: "api/01".into(),
        },
    ]
}

#[test]
fn events_every_kind_round_trips_with_its_kind() {
    for body in every_kind() {
        let event = SwarmEvent::hub(Timestamp(1_000), Severity::Info, body.clone())
            .about_agent(id("a1"))
            .about_host(id("h1"))
            .about_channel("api".parse::<ChannelName>().unwrap())
            .about_task(TaskId::new())
            .with_note("one line");
        let json = serde_json::to_value(&event).expect("serialize");
        let kind = kind_of(&event);
        assert_eq!(
            json["kind"].as_str(),
            Some(kind),
            "the body is tagged with its kind"
        );
        // The kind matches the variant, so a reader can filter on the JSON.
        let back: SwarmEvent = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, event);
        assert_eq!(kind_of(&back), kind);
    }
}

#[test]
fn events_an_unknown_kind_fails_clearly() {
    // A reader that meets a kind from a newer build fails the parse, so a
    // caller sees the event was not understood instead of losing it in a
    // silent skip. A reader that must tolerate new kinds parses the JSON into
    // a value first.
    let json = serde_json::json!({
        "id": EventId::new(),
        "at": 1_000,
        "source": "hub",
        "severity": "info",
        "kind": "nats.disconnected",
        "server": "nats://127.0.0.1:4222",
    });
    let err = serde_json::from_value::<SwarmEvent>(json).expect_err("an unknown kind");
    assert!(
        err.to_string().contains("unknown variant"),
        "the error names the problem: {err}"
    );
}

#[test]
fn events_a_host_event_names_its_source() {
    let host: HostName = id("h1");
    let event = SwarmEvent::host(
        Timestamp(1),
        &host,
        Severity::Error,
        SwarmEventBody::HubUnreachable {
            since: Timestamp(0),
        },
    );
    assert_eq!(event.source, borant_core::events::EventSource::Host(host));
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["source"]["host"], "h1");
    let hub = SwarmEvent::hub(
        Timestamp(1),
        Severity::Info,
        SwarmEventBody::HostDraining { agents: 0 },
    );
    assert_eq!(
        serde_json::to_value(&hub).unwrap()["source"],
        serde_json::json!("hub")
    );
}
