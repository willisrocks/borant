//! The swarm event stream (phase 2 spec, "Events"). The hub and the hosts
//! build one `SwarmEvent` per thing that happened; readers filter and follow.

use serde::{Deserialize, Serialize};

use crate::ids::{
    AgentId, BundleName, ChannelName, EventId, FencingToken, HostName, MemberName, MessageId,
    PoolName, SessionId, TaskId,
};
use crate::model::{Harness, MoveReason, TaskStatus};
use crate::time::Timestamp;

/// How bad an event is. A reader can filter on the minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// Who wrote the event: the hub, or one host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Hub,
    Host(HostName),
}

/// One thing that happened in the swarm.
///
/// The body is flattened, so the JSON of an event has its `kind` next to the
/// common fields:
///
/// ```json
/// {"id": "01J...", "at": 1793, "source": "hub", "severity": "info",
///  "kind": "lease.expired", "session": "01J...", "token": 3}
/// ```
///
/// Two consequences of the flatten: a body never repeats a common field name,
/// so the host of `placement.chosen` or `lease.reserved` is the common `host`
/// (set with [`SwarmEvent::about_host`]), and the free-form line for people is
/// `note`, because every `reason` of a body (a `MoveReason`, an end reason) is
/// typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmEvent {
    pub id: EventId,
    pub at: Timestamp,
    pub source: EventSource,
    pub severity: Severity,
    /// The agent the event is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// The host the event is about, when it differs from the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// One line for people, for a reader that logs the event. The typed
    /// `reason` of a body stays in the body, so this note cannot be
    /// flattened over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(flatten)]
    pub body: SwarmEventBody,
}

/// The typed part of an event, tagged by `kind`.
///
/// The kinds are the table of the observability doc, without `nats.*` (the
/// NATS adapter has its own log) and `usage.budget` (phase 4), plus the kinds
/// of parking, elastic hosts, bundle members, and alerts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SwarmEventBody {
    #[serde(rename = "hub.started")]
    HubStarted {
        instance: String,
        token: u64,
        /// How long the hub waited before it served, as a restart grace.
        quiet_secs: u64,
    },
    #[serde(rename = "hub.stepped_down")]
    HubSteppedDown { instance: String, token: u64 },
    #[serde(rename = "hub.maintenance")]
    HubMaintenance { instance: String, on: bool },
    #[serde(rename = "host.registered")]
    HostRegistered {
        last_seen: Timestamp,
        always_on: bool,
        agents: u64,
    },
    #[serde(rename = "host.online")]
    HostOnline {
        last_seen: Timestamp,
        always_on: bool,
        agents: u64,
    },
    #[serde(rename = "host.offline")]
    HostOffline {
        last_seen: Timestamp,
        always_on: bool,
        agents: u64,
    },
    #[serde(rename = "host.draining")]
    HostDraining { agents: u64 },
    #[serde(rename = "hub.unreachable")]
    HubUnreachable { since: Timestamp },
    #[serde(rename = "lease.acquired")]
    LeaseAcquired {
        session: SessionId,
        token: FencingToken,
    },
    #[serde(rename = "lease.released")]
    LeaseReleased {
        session: SessionId,
        token: FencingToken,
    },
    #[serde(rename = "lease.reclaimed")]
    LeaseReclaimed {
        session: SessionId,
        token: FencingToken,
    },
    #[serde(rename = "lease.expired")]
    LeaseExpired {
        session: SessionId,
        token: FencingToken,
    },
    #[serde(rename = "lease.reserved")]
    /// The host is the one of the event context.
    LeaseReserved {
        reason: MoveReason,
        until: Timestamp,
    },
    #[serde(rename = "lease.reservation_ended")]
    LeaseReservationEnded {
        reason: MoveReason,
        until: Timestamp,
    },
    #[serde(rename = "session.started")]
    SessionStarted {
        session: SessionId,
        harness: Harness,
    },
    #[serde(rename = "session.ended")]
    SessionEnded {
        session: SessionId,
        harness: Harness,
        reason: String,
    },
    #[serde(rename = "session.paused")]
    SessionPaused { why: String },
    /// The hub is back: the tools of the session work again.
    #[serde(rename = "session.resumed")]
    SessionResumed {},
    #[serde(rename = "session.fenced")]
    SessionFenced { why: String },
    #[serde(rename = "agent.spawned")]
    AgentSpawned {
        /// The agent that spawned it.
        by: AgentId,
        harness: Harness,
    },
    #[serde(rename = "agent.pruned")]
    AgentPruned {
        /// The agent that pruned it, when a person did not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<AgentId>,
        idle_days: f64,
    },
    /// An idle session parked its harness (phase 0 parking).
    #[serde(rename = "agent.parked")]
    AgentParked {
        session: SessionId,
        /// The checkpoint of the park, when the host made one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
    },
    #[serde(rename = "placement.chosen")]
    /// The chosen host is the one of the event context.
    PlacementChosen {
        harness: Harness,
        reason: MoveReason,
    },
    #[serde(rename = "placement.refused")]
    PlacementRefused {
        reason: MoveReason,
        /// The hosts that were rejected, and why.
        rejected: Vec<RejectedHost>,
    },
    #[serde(rename = "move.requested")]
    MoveRequested {
        from_host: Option<HostName>,
        to_host: HostName,
        reason: MoveReason,
    },
    #[serde(rename = "move.launched")]
    MoveLaunched {
        from_host: Option<HostName>,
        to_host: HostName,
        reason: MoveReason,
    },
    #[serde(rename = "move.ready")]
    MoveReady {
        from_host: Option<HostName>,
        to_host: HostName,
        reason: MoveReason,
    },
    #[serde(rename = "move.failed")]
    MoveFailed {
        from_host: Option<HostName>,
        to_host: HostName,
        reason: MoveReason,
        error: String,
    },
    /// A moved session could not commit its work in progress.
    #[serde(rename = "move.stale_saved")]
    MoveStaleSaved { git_ref: String },
    #[serde(rename = "claims.released")]
    ClaimsReleased { tasks: u64, after_secs: u64 },
    #[serde(rename = "task.status")]
    TaskStatus {
        from: TaskStatus,
        to: TaskStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claimed_by: Option<AgentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attention_msg: Option<String>,
    },
    #[serde(rename = "delivery.ack_failed")]
    DeliveryAckFailed {
        message: MessageId,
        error: String,
        /// How many of this kind for this agent in the minute.
        count: u64,
    },
    #[serde(rename = "delivery.append_failed")]
    DeliveryAppendFailed {
        message: MessageId,
        error: String,
        count: u64,
    },
    #[serde(rename = "delivery.redelivered")]
    DeliveryRedelivered {
        message: MessageId,
        deliveries: u64,
        count: u64,
    },
    #[serde(rename = "delivery.wake_limited")]
    DeliveryWakeLimited { message: MessageId, count: u64 },
    #[serde(rename = "delivery.bad_envelope")]
    DeliveryBadEnvelope { error: String, count: u64 },
    /// Every member of a bundle is limited.
    #[serde(rename = "member.limited")]
    MemberLimited {
        bundle: BundleName,
        member: MemberName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Timestamp>,
    },
    #[serde(rename = "member.reset")]
    MemberReset {
        bundle: BundleName,
        member: MemberName,
    },
    /// A pool provisioner created a host.
    #[serde(rename = "elastic.created")]
    /// The host is the one of the event context.
    ElasticCreated { pool: PoolName },
    #[serde(rename = "elastic.deleted")]
    ElasticDeleted { pool: PoolName },
    #[serde(rename = "alert.fired")]
    AlertFired { rule: String, subject: String },
    #[serde(rename = "alert.resolved")]
    AlertResolved { rule: String, subject: String },
}

/// A host that a placement passed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedHost {
    pub host: HostName,
    pub why: String,
}

impl SwarmEvent {
    /// An event from the hub.
    pub fn hub(at: Timestamp, severity: Severity, body: SwarmEventBody) -> Self {
        Self {
            id: EventId::new(),
            at,
            source: EventSource::Hub,
            severity,
            agent: None,
            host: None,
            channel: None,
            task: None,
            note: None,
            body,
        }
    }

    /// An event from one host.
    pub fn host(at: Timestamp, host: &HostName, severity: Severity, body: SwarmEventBody) -> Self {
        Self {
            id: EventId::new(),
            at,
            source: EventSource::Host(host.clone()),
            severity,
            agent: None,
            host: None,
            channel: None,
            task: None,
            note: None,
            body,
        }
    }

    pub fn about_agent(mut self, agent: AgentId) -> Self {
        self.agent = Some(agent);
        self
    }

    pub fn about_host(mut self, host: HostName) -> Self {
        self.host = Some(host);
        self
    }

    pub fn about_channel(mut self, channel: ChannelName) -> Self {
        self.channel = Some(channel);
        self
    }

    pub fn about_task(mut self, task: TaskId) -> Self {
        self.task = Some(task);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Which events a reader wants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    /// Only events at or after this time.
    #[serde(default)]
    pub since: Option<Timestamp>,
    /// Only these kinds. An entry may end in `.*`, for example `lease.*`, to
    /// match every kind below it. No entry means every kind.
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub agent: Option<AgentId>,
    #[serde(default)]
    pub channel: Option<ChannelName>,
    #[serde(default)]
    pub host: Option<HostName>,
    /// Only this severity and worse.
    #[serde(default)]
    pub min_severity: Option<Severity>,
}

impl EventFilter {
    /// True when the event passes every field that the filter sets.
    pub fn matches(&self, event: &SwarmEvent) -> bool {
        if self.since.is_some_and(|since| event.at < since) {
            return false;
        }
        if self.min_severity.is_some_and(|min| event.severity < min) {
            return false;
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| event.agent.as_ref() != Some(agent))
        {
            return false;
        }
        if self
            .channel
            .as_ref()
            .is_some_and(|channel| event.channel.as_ref() != Some(channel))
        {
            return false;
        }
        if self
            .host
            .as_ref()
            .is_some_and(|host| event.host.as_ref() != Some(host))
        {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.iter().any(|kind| kind_matches(kind, event)) {
            return false;
        }
        true
    }
}

/// True when `pattern` names the kind of `event`. `lease.*` matches every kind
/// below `lease.`, and `*` matches all.
fn kind_matches(pattern: &str, event: &SwarmEvent) -> bool {
    let kind = kind_of(event);
    match pattern.strip_suffix(".*") {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == "*" || pattern == kind,
    }
}

/// The `kind` of an event, as it appears on the wire and in a subject.
pub fn kind_of(event: &SwarmEvent) -> &'static str {
    match &event.body {
        SwarmEventBody::HubStarted { .. } => "hub.started",
        SwarmEventBody::HubSteppedDown { .. } => "hub.stepped_down",
        SwarmEventBody::HubMaintenance { .. } => "hub.maintenance",
        SwarmEventBody::HostRegistered { .. } => "host.registered",
        SwarmEventBody::HostOnline { .. } => "host.online",
        SwarmEventBody::HostOffline { .. } => "host.offline",
        SwarmEventBody::HostDraining { .. } => "host.draining",
        SwarmEventBody::HubUnreachable { .. } => "hub.unreachable",
        SwarmEventBody::LeaseAcquired { .. } => "lease.acquired",
        SwarmEventBody::LeaseReleased { .. } => "lease.released",
        SwarmEventBody::LeaseReclaimed { .. } => "lease.reclaimed",
        SwarmEventBody::LeaseExpired { .. } => "lease.expired",
        SwarmEventBody::LeaseReserved { .. } => "lease.reserved",
        SwarmEventBody::LeaseReservationEnded { .. } => "lease.reservation_ended",
        SwarmEventBody::SessionStarted { .. } => "session.started",
        SwarmEventBody::SessionEnded { .. } => "session.ended",
        SwarmEventBody::SessionPaused { .. } => "session.paused",
        SwarmEventBody::SessionResumed { .. } => "session.resumed",
        SwarmEventBody::SessionFenced { .. } => "session.fenced",
        SwarmEventBody::AgentSpawned { .. } => "agent.spawned",
        SwarmEventBody::AgentPruned { .. } => "agent.pruned",
        SwarmEventBody::AgentParked { .. } => "agent.parked",
        SwarmEventBody::PlacementChosen { .. } => "placement.chosen",
        SwarmEventBody::PlacementRefused { .. } => "placement.refused",
        SwarmEventBody::MoveRequested { .. } => "move.requested",
        SwarmEventBody::MoveLaunched { .. } => "move.launched",
        SwarmEventBody::MoveReady { .. } => "move.ready",
        SwarmEventBody::MoveFailed { .. } => "move.failed",
        SwarmEventBody::MoveStaleSaved { .. } => "move.stale_saved",
        SwarmEventBody::ClaimsReleased { .. } => "claims.released",
        SwarmEventBody::TaskStatus { .. } => "task.status",
        SwarmEventBody::DeliveryAckFailed { .. } => "delivery.ack_failed",
        SwarmEventBody::DeliveryAppendFailed { .. } => "delivery.append_failed",
        SwarmEventBody::DeliveryRedelivered { .. } => "delivery.redelivered",
        SwarmEventBody::DeliveryWakeLimited { .. } => "delivery.wake_limited",
        SwarmEventBody::DeliveryBadEnvelope { .. } => "delivery.bad_envelope",
        SwarmEventBody::MemberLimited { .. } => "member.limited",
        SwarmEventBody::MemberReset { .. } => "member.reset",
        SwarmEventBody::ElasticCreated { .. } => "elastic.created",
        SwarmEventBody::ElasticDeleted { .. } => "elastic.deleted",
        SwarmEventBody::AlertFired { .. } => "alert.fired",
        SwarmEventBody::AlertResolved { .. } => "alert.resolved",
    }
}
