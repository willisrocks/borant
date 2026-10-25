//! Domain records. Each one is stored as JSON in the state store or sent on
//! the bus.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{
    AgentId, BundleName, ChannelName, CollectionName, EventId, FencingToken, HostName, MemberName,
    MessageId, PoolName, ProjectName, RoleName, SessionId, Tag, TaskId,
};
use crate::rules::roles::Resolved;
use crate::time::Timestamp;

/// A kind of sandbox that a host can run an agent in. A kind, not a product:
/// the adapter that builds the sandbox lives in `borant-launch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxKind {
    /// A file-system namespace: the harness sees only its own mounts, and not
    /// the credentials, the other agents' clones, or the tmux socket.
    Namespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    ClaudeCode,
    Pi,
    Omp,
    Codex,
    Fake,
}

impl Harness {
    pub const ALL: [Harness; 5] = [
        Self::ClaudeCode,
        Self::Pi,
        Self::Omp,
        Self::Codex,
        Self::Fake,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Pi => "pi",
            Self::Omp => "omp",
            Self::Codex => "codex",
            Self::Fake => "fake",
        }
    }
}

impl fmt::Display for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Harness {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|h| h.as_str() == s)
            .ok_or_else(|| {
                format!("unknown harness '{s}': use claude-code, pi, omp, codex, or fake")
            })
    }
}

/// Where the working directory of an agent lives in git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub remote_url: String,
    pub branch: String,
    /// The working directory relative to the repository root. Empty for the root.
    pub subdir: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub preferred_harness: Harness,
    pub repo: Option<RepoInfo>,
    /// False for agents that share a directory or have no repository.
    pub movable: bool,
    pub channels: Vec<ChannelName>,
    pub session: Option<SessionId>,
    pub host: Option<HostName>,
    /// The native session ID of the last session, for a resume on the same host.
    pub harness_session_id: Option<String>,
    pub created: Timestamp,
    /// The model that `spawn` asked for. Moves and takeovers keep it.
    #[serde(default)]
    pub model: Option<String>,
    /// The harness that `model` was chosen for. A launch in another harness
    /// gets no model.
    #[serde(default)]
    pub model_harness: Option<Harness>,
    /// The project of the channel that the agent was spawned in.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The member of each bundle with several members, for example
    /// `claude_auth -> a`. Names only.
    #[serde(default)]
    pub members: BTreeMap<BundleName, MemberName>,
    /// A pinned agent is never replaced while its session may still run: the
    /// hub starts no second session, and a paused host keeps its tools.
    #[serde(default)]
    pub pinned: bool,
    /// The role from `[roles]` in `hub.toml`, for its harness, model,
    /// placement, and what it may spawn.
    #[serde(default)]
    pub role: Option<RoleName>,
    /// The collection that holds this agent, from its parent or its channel.
    #[serde(default)]
    pub collection: Option<CollectionName>,
    /// The agent that spawned this one. A helper runs on its parent's host.
    #[serde(default)]
    pub parent: Option<AgentId>,
    /// The idempotency key of the spawn that made this agent.
    #[serde(default)]
    pub spawn_key: Option<String>,
    /// The task that this worker is bound to.
    #[serde(default)]
    pub task: Option<TaskRef>,
    /// Stop the session when the bound task closes.
    #[serde(default)]
    pub stop_on_close: bool,
    /// A retired agent is never placed again; its session stops at the next
    /// turn end.
    #[serde(default)]
    pub retired: bool,
    /// A paused agent keeps its claims, gets no stall alert, and is never
    /// placed until it is resumed. A person or the channel owner stops it.
    #[serde(default)]
    pub paused: Option<PauseReason>,
}

/// Why an agent is paused (phase 6 spec, "Stop, resume, caps").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseReason {
    /// The person or the agent that stopped it.
    pub by: Sender,
    /// One line for the human inbox, for example `cap: 200 turns today`.
    pub why: String,
    pub at: Timestamp,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: an agent always has an ID.
#[cfg(any(test, feature = "testing"))]
impl Default for Agent {
    fn default() -> Self {
        Self {
            id: "test-agent".parse().expect("a valid agent ID"),
            preferred_harness: Harness::ClaudeCode,
            repo: None,
            movable: false,
            channels: Vec::new(),
            session: None,
            host: None,
            harness_session_id: None,
            created: Timestamp(0),
            model: None,
            model_harness: None,
            project: None,
            members: BTreeMap::new(),
            pinned: false,
            role: None,
            collection: None,
            parent: None,
            spawn_key: None,
            task: None,
            stop_on_close: false,
            retired: false,
            paused: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent: AgentId,
    pub harness: Harness,
    pub harness_session_id: Option<String>,
    pub host: HostName,
    pub cwd: String,
    pub pid: Option<u32>,
    pub started: Timestamp,
    pub ended: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveReason {
    Takeover,
    Planned,
    Spawn,
    /// A session stopped at a usage limit and moved to another member.
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LeaseState {
    Held {
        session: SessionId,
        host: HostName,
        expires: Timestamp,
    },
    Expired {
        since: Timestamp,
        /// The session that held the lease when the tick expired it. Only
        /// this session can take the lease back (`lease::reclaim`), and only
        /// while nobody else took the agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_session: Option<SessionId>,
    },
    Reserved {
        host: HostName,
        until: Timestamp,
        reason: MoveReason,
    },
    Released {
        since: Timestamp,
        /// The host parked the session to free its box. A parked agent wakes
        /// on a message that wakes it or on a claim, unlike a plain release.
        #[serde(default)]
        parked: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub agent: AgentId,
    pub token: FencingToken,
    #[serde(flatten)]
    pub state: LeaseState,
}

/// The lease of the hub daemon itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubLease {
    pub holder: String,
    pub token: FencingToken,
    pub expires: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: HostName,
    /// Harnesses that are installed and logged in on the host.
    pub harnesses: Vec<Harness>,
    pub launcher: String,
    pub max_agents: u32,
    pub always_on: bool,
    pub draining: bool,
    /// The SSH destination for `borant attach`, for example `me@vps-1`.
    pub ssh: Option<String>,
    /// The bundles of this host with two or more members.
    #[serde(default)]
    pub members: BTreeMap<BundleName, MemberBundle>,
    /// The pool that created this host. `None` for a static host.
    #[serde(default)]
    pub pool: Option<PoolName>,
    /// The tags of this host, for example `vps` for a pool host.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Every bundle that the host defines, for a launch of a collection.
    #[serde(default)]
    pub bundles: Vec<BundleName>,
    /// The memory in megabytes that the host keeps free for itself, so that
    /// no new agent lands on a host that would be starved. 1024 when unset.
    #[serde(default)]
    pub reserve_mem_mb: Option<u32>,
    /// The sandboxes that this host offers.
    #[serde(default)]
    pub sandboxes: Vec<SandboxKind>,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: a host always names itself.
#[cfg(any(test, feature = "testing"))]
impl Default for HostInfo {
    fn default() -> Self {
        Self {
            name: "test-host".parse().expect("a valid host name"),
            harnesses: Vec::new(),
            launcher: String::new(),
            max_agents: 0,
            always_on: false,
            draining: false,
            ssh: None,
            members: BTreeMap::new(),
            pool: None,
            tags: Vec::new(),
            bundles: Vec::new(),
            reserve_mem_mb: None,
            sandboxes: Vec::new(),
        }
    }
}

/// The author and committer of the commits of an agent, from its collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

/// The bundles of a host that have several members, and the harnesses whose
/// defaults include the bundle (`"*"` expanded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBundle {
    pub members: Vec<MemberName>,
    pub harnesses: Vec<Harness>,
}

/// The state of one bundle member, at the key `member.<bundle>.<member>`.
/// Written only by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberState {
    pub bundle: BundleName,
    pub member: MemberName,
    /// Until when this member is limited, when a session reported a limit.
    #[serde(default)]
    pub limited_until: Option<Timestamp>,
    /// The agents that wait for this member to reset, so a hub restart still
    /// wakes them.
    #[serde(default)]
    pub waiting: Vec<AgentId>,
}

/// One usage window of a member's latest report. `used` is a share from 0 to 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub name: String,
    pub used: f64,
    pub resets_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub name: ChannelName,
    pub owner: AgentId,
    pub schema: Value,
    pub created: Timestamp,
    pub closed: Option<Timestamp>,
    /// A spawn in this channel clones this project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The base branch for spawns. Set when `project` is set.
    #[serde(default)]
    pub branch: Option<String>,
    /// The collection of this channel, from its project or its creator. The
    /// field is the authority, not the name prefix.
    #[serde(default)]
    pub collection: Option<CollectionName>,
    /// A paused channel takes no new spawn, and pauses each member whose only
    /// channel it is.
    #[serde(default)]
    pub paused: bool,
}

/// A named git repository that spawns clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: ProjectName,
    pub remote_url: String,
    pub default_branch: String,
    /// Where checkpoints go, if not the remote of the repository.
    #[serde(default)]
    pub checkpoint_remote: Option<String>,
    /// A shell command that runs once in a new clone, before the harness.
    #[serde(default)]
    pub setup: Option<String>,
    /// The focused test command of the project.
    #[serde(default)]
    pub test: Option<String>,
    /// The collection that holds this project and its channels.
    #[serde(default)]
    pub collection: Option<CollectionName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub agent: AgentId,
    pub joined: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    NeedsHuman,
    Stuck,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Done,
    Wontfix,
    Duplicate,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub status: TaskStatus,
    pub close_reason: Option<CloseReason>,
    pub close_message: Option<String>,
    pub attention_msg: Option<String>,
    pub claimed_by: Option<AgentId>,
    /// When the task is due. A passed deadline puts the task in front of its
    /// owner once; it reassigns nothing.
    #[serde(default)]
    pub deadline: Option<Timestamp>,
    /// Claims that ended without a `done` close. One claim counts once.
    #[serde(default)]
    pub attempts: u32,
    /// The retry limit. At this many attempts the task goes to `needs_human`,
    /// and a claim of it is refused until the owner reopens it.
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub blocked_by: Vec<TaskId>,
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub properties: Map<String, Value>,
    pub created: Timestamp,
    pub updated: Timestamp,
    /// The idempotency key of the create that made this task.
    #[serde(default)]
    pub create_key: Option<String>,
}

/// The task that a worker is bound to, at `Agent.task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub channel: ChannelName,
    pub id: TaskId,
}

/// The task that a spawn binds to: an existing one by id, or a new one that
/// the hub creates with `title`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskBinding {
    Existing { channel: ChannelName, id: TaskId },
    New { channel: ChannelName, title: String },
}

/// The arguments of one spawn, as the queue stores them. Two spawns with one
/// key match when their specs are equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub channel: ChannelName,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub harness: Option<Harness>,
    #[serde(default)]
    pub host: Option<HostName>,
    #[serde(default)]
    pub name: Option<AgentId>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub role: Option<RoleName>,
    /// The bound task, by its id once the hub has made or found it.
    #[serde(default)]
    pub task: Option<TaskRef>,
    #[serde(default)]
    pub stop_on_close: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
}

/// One queued spawn, at the key `spawn.<agent>`. Written only by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnRecord {
    pub agent: AgentId,
    pub spec: SpawnSpec,
    /// The roles of the spawn at enqueue time, so a queue that outlives its
    /// parent still places the agent the same way.
    pub resolved: Resolved,
    pub by: Sender,
    pub queued_at: Timestamp,
    pub priority: i32,
    pub state: SpawnState,
    #[serde(default)]
    pub attempts: u32,
    /// The first failed rule of each host at the last refused placement, for
    /// `spawn_queue`. A v0.7.0 record holds one string and no host, so it
    /// parses and is dropped: the next attempt writes the list.
    #[serde(default, deserialize_with = "de_last_reject")]
    pub last_reject: Vec<(HostName, String)>,
}

/// Reads `last_reject` of a queue record: the list of this version, or the
/// one string of a v0.7.0 record.
fn de_last_reject<'de, D>(d: D) -> Result<Vec<(HostName, String)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        List(Vec<(HostName, String)>),
        Old(String),
    }
    Ok(match Option::<Wire>::deserialize(d)? {
        Some(Wire::List(list)) => list,
        Some(Wire::Old(text)) => {
            // A v0.7.0 reason is one string for the whole placement, so it
            // names no host. The next attempt writes the list.
            drop(text);
            Vec::new()
        }
        None => Vec::new(),
    })
}

/// Where a queued spawn is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnState {
    Queued,
    Launching,
    Running,
    Failed,
    Cancelled,
}

/// The input of `task_create`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub blocked_by: Vec<TaskId>,
    #[serde(default)]
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub properties: Map<String, Value>,
}

/// The input of `task_update`. Each present field replaces the old value.
/// `properties` merges key by key, and a `null` value removes the key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub attention_msg: Option<String>,
    #[serde(default)]
    pub blocked_by: Option<Vec<TaskId>>,
    #[serde(default)]
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub properties: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Sender {
    Agent(AgentId),
    Human,
}

/// Events written before phase 3 name the sender as a bare agent id, so both
/// `{"type":"agent","id":"w1"}` and `"w1"` parse.
impl<'de> Deserialize<'de> for Sender {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "id", rename_all = "snake_case")]
        enum Tagged {
            Agent(AgentId),
            Human,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Tagged(Tagged),
            Name(String),
        }

        match Repr::deserialize(d)? {
            Repr::Tagged(Tagged::Agent(id)) => Ok(Self::Agent(id)),
            Repr::Tagged(Tagged::Human) => Ok(Self::Human),
            Repr::Name(name) if name == "human" => Ok(Self::Human),
            Repr::Name(name) => name
                .parse::<AgentId>()
                .map(Self::Agent)
                .map_err(serde::de::Error::custom),
        }
    }
}

impl fmt::Display for Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(a) => a.fmt(f),
            Self::Human => f.write_str("human"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Target {
    Agent(AgentId),
    Channel(ChannelName),
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(a) => a.fmt(f),
            Self::Channel(c) => write!(f, "#{c}"),
        }
    }
}

impl FromStr for Target {
    type Err = crate::error::CoreError;
    /// `#name` is a channel. Any other text is an agent ID.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix('#') {
            Some(c) => Ok(Self::Channel(c.parse()?)),
            None => Ok(Self::Agent(s.parse()?)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    Direct,
    Broadcast,
    Event,
}

impl fmt::Display for EnvelopeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct message",
            Self::Broadcast => "broadcast",
            Self::Event => "event",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Created,
    Updated,
    Claimed,
    Unclaimed,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    pub kind: TaskEventKind,
    pub status: TaskStatus,
    pub attention_msg: Option<String>,
    /// The owner of the channel when the event happened.
    pub owner: AgentId,
}

/// A message or an event on the bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: MessageId,
    pub from: Sender,
    pub to: Target,
    pub kind: EnvelopeKind,
    pub text: String,
    #[serde(default)]
    pub urgent: bool,
    #[serde(default)]
    pub event: Option<TaskEvent>,
    pub sent: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: EventId,
    pub agent: AgentId,
    pub session: Option<SessionId>,
    pub harness: Harness,
    pub host: HostName,
    pub turn: u64,
    pub at: Timestamp,
    #[serde(flatten)]
    pub body: SessionEventBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEventBody {
    TurnStart,
    UserPrompt {
        text: String,
    },
    SwarmMessage {
        envelope: Envelope,
    },
    Delivered {
        message: MessageId,
    },
    AssistantMessage {
        text: String,
    },
    ToolCall {
        name: String,
        input: String,
    },
    ToolResult {
        name: String,
        output: String,
        is_error: bool,
    },
    Checkpoint {
        git_ref: String,
        sha: String,
    },
    SessionStart {
        cwd: String,
    },
    SessionEnd {
        reason: String,
    },
    Move {
        from_host: Option<HostName>,
        to_host: HostName,
        /// The harness of the new session. The event's own `harness` field
        /// names the harness that wrote the event.
        to_harness: Harness,
        reason: MoveReason,
    },
    /// The bundles that a launch issued. Names only, never a value.
    SecretsIssued {
        bundles: Vec<BundleName>,
        members: BTreeMap<BundleName, MemberName>,
    },
}

/// The load and counters of one host, sent in its heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStats {
    pub version: String,
    /// When the host daemon started.
    #[serde(default)]
    pub started: Option<Timestamp>,
    /// The one-minute load average.
    #[serde(default)]
    pub load1: f64,
    #[serde(default)]
    pub cpus: u32,
    /// The used memory, as a percentage from 0 to 100.
    #[serde(default)]
    pub mem_used_pct: f64,
    /// The memory available in megabytes, from `sysinfo`.
    #[serde(default)]
    pub mem_avail_mb: Option<u64>,
    /// The free space of the disk that holds `BORANT_HOME`, in megabytes.
    #[serde(default)]
    pub disk_avail_mb: Option<u64>,
    /// How full that disk is, as a percentage from 0 to 100.
    #[serde(default)]
    pub disk_used_pct: Option<f64>,
    #[serde(default)]
    pub outbox_depth: u64,
    /// The round trip of the previous heartbeat, in milliseconds.
    #[serde(default)]
    pub last_heartbeat_rtt_ms: Option<u64>,
    #[serde(default)]
    pub nats_reconnects: u64,
    /// One entry for each session of the host.
    #[serde(default)]
    pub sessions: Vec<SessionStats>,
}

/// What one session of a host did since it started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionStats {
    pub agent: AgentId,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub last_event_at: Option<Timestamp>,
    #[serde(default)]
    pub wakes: u64,
    #[serde(default)]
    pub wake_limited: u64,
    /// Delivery anomalies that the host logged, by kind.
    #[serde(default)]
    pub ack_failed: u64,
    #[serde(default)]
    pub append_failed: u64,
    #[serde(default)]
    pub redelivered: u64,
    #[serde(default)]
    pub bad_envelope: u64,
    /// The LLM usage of the session.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

/// The live view of the swarm, for `borant top` and `borant status --json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwarmView {
    #[serde(default)]
    pub hosts: Vec<HostStatus>,
    #[serde(default)]
    pub agents: Vec<AgentStatus>,
    #[serde(default)]
    pub inboxes: Vec<InboxStatus>,
    /// When the hub built the view.
    #[serde(default)]
    pub at: Option<Timestamp>,
}

/// One host in the live view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub info: HostInfo,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub last_seen: Option<Timestamp>,
    #[serde(default)]
    pub stats: Option<HostStats>,
}

/// One agent in the live view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub agent: AgentId,
    #[serde(default)]
    pub harness: Option<Harness>,
    #[serde(default)]
    pub host: Option<HostName>,
    /// The state of its lease, as the store holds it.
    #[serde(default)]
    pub lease: Option<LeaseState>,
    #[serde(default)]
    pub session: Option<SessionId>,
    #[serde(default)]
    pub busy: bool,
    /// The open task that it claimed.
    #[serde(default)]
    pub claim: Option<TaskId>,
    /// The role from `hub.toml`, when the agent was spawned or started with
    /// one.
    #[serde(default)]
    pub role: Option<RoleName>,
    /// The collection that holds the agent.
    #[serde(default)]
    pub collection: Option<CollectionName>,
    /// A pinned agent is never replaced, and its tools survive a pause.
    #[serde(default)]
    pub pinned: bool,
    /// A retired agent is never placed again.
    #[serde(default)]
    pub retired: bool,
    /// The model that the harness runs, when one was chosen.
    #[serde(default)]
    pub model: Option<String>,
}

/// The inbox counts of one agent in the live view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxStatus {
    pub agent: AgentId,
    #[serde(default)]
    pub stats: crate::ports::InboxStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v0.8.0 record has no durable-work fields; every new field is
    /// `serde(default)`, so the old JSON parses with the defaults.
    #[test]
    fn old_records_parse_with_the_durable_defaults() {
        let task = Task {
            id: TaskId::new(),
            title: "t".into(),
            body: String::new(),
            status: TaskStatus::Open,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            deadline: None,
            attempts: 0,
            max_attempts: None,
            blocked_by: Vec::new(),
            parent: None,
            properties: Map::new(),
            created: Timestamp(1),
            updated: Timestamp(2),
            create_key: None,
        };
        let mut old = serde_json::to_value(&task).unwrap();
        let obj = old.as_object_mut().unwrap();
        for key in ["deadline", "attempts", "max_attempts"] {
            obj.remove(key);
        }
        assert_eq!(serde_json::from_value::<Task>(old).unwrap(), task);

        let agent = Agent::default();
        let mut old = serde_json::to_value(&agent).unwrap();
        old.as_object_mut().unwrap().remove("paused");
        assert_eq!(serde_json::from_value::<Agent>(old).unwrap(), agent);

        let channel = Channel {
            name: "api".parse().unwrap(),
            owner: "w1".parse().unwrap(),
            schema: Value::Null,
            created: Timestamp(1),
            closed: None,
            project: None,
            branch: None,
            collection: None,
            paused: false,
        };
        let mut old = serde_json::to_value(&channel).unwrap();
        old.as_object_mut().unwrap().remove("paused");
        assert_eq!(serde_json::from_value::<Channel>(old).unwrap(), channel);
    }

    #[test]
    fn target_parses_channels_and_agents() {
        assert_eq!(
            "#api".parse::<Target>().unwrap(),
            Target::Channel("api".parse().unwrap())
        );
        assert_eq!(
            "worker-1".parse::<Target>().unwrap(),
            Target::Agent("worker-1".parse().unwrap())
        );
        assert!("#Bad".parse::<Target>().is_err());
    }

    #[test]
    fn old_channel_json_loads() {
        let ch: Channel = serde_json::from_value(serde_json::json!({
            "name": "api", "owner": "boss", "schema": {}, "created": 1, "closed": null,
        }))
        .unwrap();
        assert_eq!(ch.project, None);
        assert_eq!(ch.branch, None);
    }

    #[test]
    fn lease_serializes_flat() {
        let lease = Lease {
            agent: "a".parse().unwrap(),
            token: FencingToken(3),
            state: LeaseState::Expired {
                since: Timestamp(5),
                last_session: None,
            },
        };
        let v = serde_json::to_value(&lease).unwrap();
        assert_eq!(v["state"], "expired");
        assert_eq!(v["token"], 3);
        assert!(v.get("last_session").is_none());
        let back: Lease = serde_json::from_value(v).unwrap();
        assert_eq!(back, lease);
        let s = SessionId::new();
        let held_before = Lease {
            state: LeaseState::Expired {
                since: Timestamp(5),
                last_session: Some(s),
            },
            ..lease
        };
        let v = serde_json::to_value(&held_before).unwrap();
        let back: Lease = serde_json::from_value(v).unwrap();
        assert_eq!(back, held_before);
    }

    #[test]
    fn move_event_round_trips() {
        let ev = SessionEvent {
            id: EventId::new(),
            agent: "a".parse().unwrap(),
            session: None,
            harness: Harness::Pi,
            host: "h".parse().unwrap(),
            turn: 0,
            at: Timestamp(1),
            body: SessionEventBody::Move {
                from_host: None,
                to_host: "h2".parse().unwrap(),
                to_harness: Harness::Omp,
                reason: MoveReason::Takeover,
            },
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<SessionEvent>(&s).unwrap(), ev);
    }

    #[test]
    fn session_event_round_trips() {
        let ev = SessionEvent {
            id: EventId::new(),
            agent: "a".parse().unwrap(),
            session: None,
            harness: Harness::Pi,
            host: "h".parse().unwrap(),
            turn: 1,
            at: Timestamp(1),
            body: SessionEventBody::ToolResult {
                name: "bash".into(),
                output: "ok".into(),
                is_error: false,
            },
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"tool_result\""));
        assert_eq!(serde_json::from_str::<SessionEvent>(&s).unwrap(), ev);
    }
}
