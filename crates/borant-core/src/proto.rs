//! Wire types: requests to the hub, commands to hosts, and the local API of
//! the host daemon.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::CoreError;
use crate::ids::{
    AgentId, BundleName, ChannelName, FencingToken, HostName, MemberName, ProjectName, SessionId,
    TaskId,
};
use crate::model::{
    CloseReason, Envelope, Harness, HostInfo, MoveReason, NewTask, Project, RepoInfo, Target,
    TaskPatch, UsageWindow,
};
use crate::ports::{PortError, PortErrorKind};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Invalid,
    NotFound,
    Conflict,
    StaleToken,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Invalid, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::NotFound, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Unavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Internal, message)
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        let code = match &e {
            CoreError::UnknownTask(_) => ApiErrorCode::NotFound,
            CoreError::StaleToken => ApiErrorCode::StaleToken,
            CoreError::LeaseHeld { .. } | CoreError::Claimed(_) => ApiErrorCode::Conflict,
            _ => ApiErrorCode::Invalid,
        };
        Self::new(code, e.to_string())
    }
}

impl From<PortError> for ApiError {
    fn from(e: PortError) -> Self {
        let code = match e.kind {
            PortErrorKind::NotFound => ApiErrorCode::NotFound,
            PortErrorKind::Conflict => ApiErrorCode::Conflict,
            PortErrorKind::Unavailable | PortErrorKind::Timeout => ApiErrorCode::Unavailable,
            PortErrorKind::Other => ApiErrorCode::Internal,
        };
        Self::new(code, e.message)
    }
}

/// A reply from the hub or the host daemon: `{"ok": ...}` or `{"error": ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Value),
    Error(ApiError),
}

impl Reply {
    pub fn ok<T: Serialize>(v: &T) -> Self {
        match serde_json::to_value(v) {
            Ok(v) => Self::Ok(v),
            Err(e) => Self::Error(ApiError::internal(e.to_string())),
        }
    }

    pub fn into_result(self) -> Result<Value, ApiError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Error(e) => Err(e),
        }
    }

    pub fn parse<T: serde::de::DeserializeOwned>(self) -> Result<T, ApiError> {
        let v = self.into_result()?;
        serde_json::from_value(v).map_err(|e| ApiError::internal(format!("bad reply: {e}")))
    }
}

impl<T: Serialize> From<Result<T, ApiError>> for Reply {
    fn from(r: Result<T, ApiError>) -> Self {
        match r {
            Ok(v) => Self::ok(&v),
            Err(e) => Self::Error(e),
        }
    }
}

/// The identity of a session that makes a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub agent: AgentId,
    pub session: SessionId,
    pub token: FencingToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLeaseRef {
    pub session: SessionId,
    pub agent: AgentId,
    pub token: FencingToken,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatReply {
    /// Sessions that lost their lease and must stop.
    pub lost: Vec<SessionId>,
    /// Sessions that took their lease back after an expiry with no takeover.
    /// They continue with the new token.
    #[serde(default)]
    pub renewed: Vec<RenewedLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenewedLease {
    pub session: SessionId,
    pub token: FencingToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterSession {
    pub session: SessionId,
    pub host: HostName,
    pub harness: Harness,
    pub harness_session_id: Option<String>,
    pub pid: Option<u32>,
    pub cwd: String,
    pub repo: Option<RepoInfo>,
    pub agent: Option<AgentId>,
    pub channel: Option<ChannelName>,
    /// True if another live session on the host uses the same directory.
    pub shared_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    pub agent: AgentId,
    pub session: SessionId,
    pub token: FencingToken,
    pub movable: bool,
    pub channels: Vec<ChannelName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MoveSelector {
    Agent(AgentId),
    FromHost(HostName),
    Channel(ChannelName),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveTarget {
    pub host: Option<HostName>,
    pub harness: Option<Harness>,
}

/// One tool of the agent API. The tag is the MCP tool name, and the fields
/// are its arguments. The host daemon handles `send` and `session_history`
/// itself and forwards the rest to the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolCall {
    Whoami,
    ChannelCreate {
        name: ChannelName,
        #[serde(default)]
        schema: Option<Value>,
        /// A spawn in the channel clones this project.
        #[serde(default)]
        project: Option<ProjectName>,
        /// The base branch. Defaults to the default branch of the project.
        #[serde(default)]
        branch: Option<String>,
    },
    ChannelJoin {
        name: ChannelName,
    },
    ChannelLeave {
        name: ChannelName,
    },
    ChannelList,
    ChannelTransfer {
        name: ChannelName,
        to: AgentId,
    },
    Members {
        name: ChannelName,
    },
    Send {
        to: Target,
        text: String,
        #[serde(default)]
        urgent: bool,
    },
    TaskCreate {
        channel: ChannelName,
        task: NewTask,
    },
    TaskGet {
        channel: ChannelName,
        id: TaskId,
    },
    TaskList {
        channel: ChannelName,
        #[serde(default)]
        ready_only: bool,
    },
    TaskUpdate {
        channel: ChannelName,
        id: TaskId,
        patch: TaskPatch,
    },
    TaskClaim {
        channel: ChannelName,
        id: TaskId,
    },
    TaskClose {
        channel: ChannelName,
        id: TaskId,
        reason: CloseReason,
        message: String,
    },
    SchemaGet {
        channel: ChannelName,
    },
    SchemaExtend {
        channel: ChannelName,
        properties: Map<String, Value>,
    },
    Spawn {
        channel: ChannelName,
        prompt: String,
        #[serde(default)]
        harness: Option<Harness>,
        #[serde(default)]
        host: Option<HostName>,
        #[serde(default)]
        name: Option<AgentId>,
        #[serde(default)]
        model: Option<String>,
    },
    SessionHistory {
        #[serde(default)]
        page: usize,
        #[serde(default = "default_page_size")]
        page_size: usize,
    },
    Move {
        selector: MoveSelector,
        #[serde(default)]
        to: MoveTarget,
    },
    Drain {
        host: HostName,
    },
    ListAgents,
    ListHosts,
    /// Lists the agents that a prune would remove. Writes and deletes nothing.
    PruneAgents {
        #[serde(default = "default_prune_secs")]
        older_than_secs: u64,
    },
    /// Creates or replaces a project.
    ProjectAdd {
        project: Project,
    },
    ProjectGet {
        name: ProjectName,
    },
    ProjectList,
    /// The latest usage report of one bundle member, from a human.
    UsageReport {
        bundle: BundleName,
        member: MemberName,
        windows: Vec<UsageWindow>,
    },
    /// The members of each bundle, with their state and their latest report.
    ListMembers,
}

fn default_page_size() -> usize {
    100
}

/// One day, the default age of an agent that a prune may remove.
fn default_prune_secs() -> u64 {
    24 * 60 * 60
}

impl ToolCall {
    /// Calls that a human may make from the CLI without a session.
    pub fn allowed_for_human(&self) -> bool {
        !matches!(
            self,
            Self::ChannelCreate { .. }
                | Self::ChannelJoin { .. }
                | Self::ChannelLeave { .. }
                | Self::TaskClaim { .. }
                | Self::Spawn { .. }
                | Self::SessionHistory { .. }
        )
    }
}

/// Requests to the hub on `subjects::CTL`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CtlRequest {
    RegisterHost {
        info: HostInfo,
    },
    HostHeartbeat {
        host: HostName,
        sessions: Vec<SessionLeaseRef>,
    },
    RegisterSession(RegisterSession),
    EndSession {
        session: SessionId,
        agent: AgentId,
        token: FencingToken,
        reason: String,
    },
    Call {
        actor: Option<Actor>,
        call: ToolCall,
    },
    MoveReady {
        agent: AgentId,
        session: SessionId,
        token: FencingToken,
    },
    LaunchFailed {
        agent: AgentId,
        host: HostName,
        error: String,
    },
    Maintenance {
        on: bool,
    },
    /// Writes a snapshot of the hub state to `out` on the hub box, without
    /// maintenance mode.
    Snapshot {
        out: std::path::PathBuf,
    },
    /// A session stopped at the usage limit of its bundle member.
    MemberLimited {
        agent: AgentId,
        session: SessionId,
        token: FencingToken,
        bundle: BundleName,
        member: MemberName,
        /// When the member resets, when the harness knows.
        resets_at: Option<Timestamp>,
    },
}

/// Commands from the hub to one host on `subjects::host_cmd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
// A launch is rare and short-lived, so its size does not matter.
#[allow(clippy::large_enum_variant)]
pub enum HostCommand {
    Launch(LaunchOrder),
    PrepareMove { agent: AgentId, session: SessionId },
    Fence { session: SessionId, reason: String },
    Stop { session: SessionId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOrder {
    pub agent: AgentId,
    pub harness: Harness,
    pub repo: Option<RepoInfo>,
    /// The SHA of the WIP commit to restore, if any.
    pub wip: Option<String>,
    pub channels: Vec<ChannelName>,
    pub prompt: String,
    pub reason: MoveReason,
    pub from_harness: Option<Harness>,
    pub from_host: Option<HostName>,
    #[serde(default)]
    pub model: Option<String>,
    /// The project of the agent, for its setup command and checkpoint remote.
    #[serde(default)]
    pub project: Option<Project>,
    /// How long the hub waits for the session to register before it
    /// launches the agent again. The host fits the setup inside it.
    #[serde(default)]
    pub reserve_secs: Option<u64>,
    /// The member of each bundle with several members, picked by the hub.
    #[serde(default)]
    pub members: BTreeMap<BundleName, MemberName>,
}

// ---- Local API of the host daemon ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalRegister {
    pub harness: Harness,
    #[serde(default)]
    pub harness_session_id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    pub cwd: String,
    #[serde(default)]
    pub agent: Option<AgentId>,
    #[serde(default)]
    pub channel: Option<ChannelName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    /// Registered with the hub.
    Attached,
    /// The hub was unreachable. The daemon retries in the background.
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSession {
    pub session: SessionId,
    pub agent: Option<AgentId>,
    pub host: HostName,
    pub state: LocalState,
    pub movable: bool,
    pub channels: Vec<ChannelName>,
    /// Set when the session must stop using tools.
    #[serde(default)]
    pub fenced: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LocalEvent {
    TurnStart,
    UserPrompt {
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
    AssistantMessage {
        text: String,
    },
    TurnEnd,
    End {
        reason: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventReply {
    /// Messages to deliver into the session now.
    pub messages: Vec<Envelope>,
    /// Set when the session must stop using tools.
    pub fenced: Option<String>,
}

/// The text that a harness adapter shows for one message.
pub fn format_delivery(env: &Envelope) -> String {
    format!(
        "[borant] {} from {} to {} (id {}):\n{}",
        env.kind, env.from, env.to, env.id, env.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MessageId;
    use crate::model::{EnvelopeKind, Sender};
    use crate::time::Timestamp;

    #[test]
    fn tool_call_uses_tool_tag() {
        let call: ToolCall =
            serde_json::from_str(r#"{"tool":"task_list","channel":"api"}"#).unwrap();
        assert_eq!(
            call,
            ToolCall::TaskList {
                channel: "api".parse().unwrap(),
                ready_only: false
            }
        );
        let v = serde_json::to_value(ToolCall::Whoami).unwrap();
        assert_eq!(v, serde_json::json!({"tool": "whoami"}));
    }

    #[test]
    fn reply_shape() {
        let ok = serde_json::to_value(Reply::ok(&1)).unwrap();
        assert_eq!(ok, serde_json::json!({"ok": 1}));
        let err = serde_json::to_value(Reply::Error(ApiError::invalid("no"))).unwrap();
        assert_eq!(
            err,
            serde_json::json!({"error": {"code": "invalid", "message": "no"}})
        );
    }

    #[test]
    fn formats_delivery_text() {
        let env = Envelope {
            id: MessageId::new(),
            from: Sender::Agent("boss".parse().unwrap()),
            to: Target::Channel("api".parse().unwrap()),
            kind: EnvelopeKind::Broadcast,
            text: "ship it".into(),
            urgent: false,
            event: None,
            sent: Timestamp(0),
        };
        assert_eq!(
            format_delivery(&env),
            format!(
                "[borant] broadcast from boss to #api (id {}):\nship it",
                env.id
            )
        );
    }
}
