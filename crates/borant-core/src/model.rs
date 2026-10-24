//! Domain records. Each one is stored as JSON in the state store or sent on
//! the bus.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{
    AgentId, ChannelName, EventId, FencingToken, HostName, MessageId, SessionId, TaskId,
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
    },
    Reserved {
        host: HostName,
        until: Timestamp,
        reason: MoveReason,
    },
    Released {
        since: Timestamp,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub name: ChannelName,
    pub owner: AgentId,
    pub schema: Value,
    pub created: Timestamp,
    pub closed: Option<Timestamp>,
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
    fn lease_serializes_flat() {
        let lease = Lease {
            agent: "a".parse().unwrap(),
            token: FencingToken(3),
            state: LeaseState::Expired {
                since: Timestamp(5),
            },
        };
        let v = serde_json::to_value(&lease).unwrap();
        assert_eq!(v["state"], "expired");
        assert_eq!(v["token"], 3);
        let back: Lease = serde_json::from_value(v).unwrap();
        assert_eq!(back, lease);
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
