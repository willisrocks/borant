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
use crate::time::Timestamp;

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
    #[serde(default)]
    pub blocked_by: Vec<TaskId>,
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub properties: Map<String, Value>,
    pub created: Timestamp,
    pub updated: Timestamp,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum Sender {
    Agent(AgentId),
    Human,
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

#[cfg(test)]
mod tests {
    use super::*;

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
