//! Wire types: requests to the hub, commands to hosts, and the local API of
//! the host daemon.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::CoreError;
use crate::ids::{
    AgentId, BundleName, ChannelName, CollectionName, FencingToken, HostName, MemberName,
    ProjectName, RoleName, SessionId, TaskId,
};
use crate::model::{
    CloseReason, ContinuePolicy, Envelope, GitAuthor, Harness, HostInfo, HostStats, MoveReason,
    NewTask, Project, RepoInfo, SandboxKind, Target, TaskBinding, TaskPatch, TaskStatus,
    UsageWindow,
};
use crate::ports::{PortError, PortErrorKind};
use crate::time::Timestamp;

/// The kind of a failed call.
///
/// A client keeps a kind that it does not know as `Other`: during a rolling
/// deploy a newer daemon may send a kind that this build predates, and the
/// message of the refusal must still reach the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiErrorCode {
    Invalid,
    NotFound,
    Conflict,
    StaleToken,
    Unavailable,
    Internal,
    /// The role of the caller does not allow the request.
    Denied,
    /// A kind that this build does not know, as it arrived on the wire.
    Other(String),
}

impl ApiErrorCode {
    /// The name of the kind on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::StaleToken => "stale_token",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
            Self::Denied => "denied",
            Self::Other(name) => name,
        }
    }
}

impl std::fmt::Display for ApiErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ApiErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ApiErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        Ok(match name.as_str() {
            "invalid" => Self::Invalid,
            "not_found" => Self::NotFound,
            "conflict" => Self::Conflict,
            "stale_token" => Self::StaleToken,
            "unavailable" => Self::Unavailable,
            "internal" => Self::Internal,
            "denied" => Self::Denied,
            _ => Self::Other(name),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
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

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Denied, message)
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
            PortErrorKind::Invalid => ApiErrorCode::Invalid,
            PortErrorKind::Denied => ApiErrorCode::Denied,
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
    /// The end of the last turn of the session, or `None` while it is in a
    /// turn. The hub parks agents that stay idle past the pool timeout.
    #[serde(default)]
    pub idle_since: Option<Timestamp>,
    /// When the session wrote its last event, for the stall alert.
    #[serde(default)]
    pub last_event_at: Option<Timestamp>,
    /// The agent is pinned, as the host remembered it before a restart. The
    /// hub sets the flag again when its record lost it.
    #[serde(default)]
    pub pinned: bool,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: a lease always names its session.
#[cfg(any(test, feature = "testing"))]
impl Default for SessionLeaseRef {
    fn default() -> Self {
        Self {
            session: SessionId::default(),
            agent: "test-agent".parse().expect("a valid agent ID"),
            token: FencingToken(0),
            idle_since: None,
            last_event_at: None,
            pinned: false,
        }
    }
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
    /// The role that the session reports, for example from `BORANT_ROLE`.
    #[serde(default)]
    pub role: Option<RoleName>,
    /// The collection that the session reports.
    #[serde(default)]
    pub collection: Option<CollectionName>,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: a session always names itself.
#[cfg(any(test, feature = "testing"))]
impl Default for RegisterSession {
    fn default() -> Self {
        Self {
            session: SessionId::default(),
            host: "test-host".parse().expect("a valid host name"),
            harness: Harness::ClaudeCode,
            harness_session_id: None,
            pid: None,
            cwd: String::new(),
            repo: None,
            agent: None,
            channel: None,
            shared_dir: false,
            role: None,
            collection: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    pub agent: AgentId,
    pub session: SessionId,
    pub token: FencingToken,
    pub movable: bool,
    pub channels: Vec<ChannelName>,
    /// The agent is pinned: the host keeps its tools during a pause.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MoveSelector {
    Agent(AgentId),
    FromHost(HostName),
    Channel(ChannelName),
}

/// What `stop` and `resume` name: one agent, or a whole channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PauseSelector {
    Agent(AgentId),
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
    /// Waits for a task to reach one of `until`, then the hub sends the agent
    /// a direct message. The wait survives a hub restart.
    Wait {
        channel: ChannelName,
        task: TaskId,
        until: Vec<TaskStatus>,
        /// The limit of the wait, at most one day.
        timeout_secs: u64,
    },
    /// The questions of the swarm, for `borant inbox`.
    Inbox,
    /// Answers a task's question: the claimer gets the text, or the channel
    /// owner when nobody holds the task.
    Answer {
        channel: ChannelName,
        task: TaskId,
        text: String,
        /// The `updated` of the task that the person saw.
        expect_updated: Timestamp,
        #[serde(default)]
        key: Option<String>,
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
        /// The role from `hub.toml`, for its harness, model, and placement.
        #[serde(default)]
        role: Option<RoleName>,
        /// An idempotency key: a second spawn with the same key and the same
        /// arguments returns the first agent.
        #[serde(default)]
        key: Option<String>,
        /// The task that the worker is bound to.
        #[serde(default)]
        task: Option<TaskBinding>,
        /// Stop the session when the bound task closes. True by default for a
        /// bound worker.
        #[serde(default)]
        stop_on_close: Option<bool>,
        /// The continues of a bound worker (phase 11 spec). The hub refuses a
        /// spawn that asks for them without a bound task.
        #[serde(default)]
        continues: Option<ContinuePolicy>,
        /// Higher goes first in the spawn queue.
        #[serde(default)]
        priority: Option<i32>,
        /// The bundles that this spawn adds. An agent may name only bundles
        /// that it holds; a person may name any.
        #[serde(default)]
        bundles: Vec<BundleName>,
    },
    /// Stops an agent and cancels its queued spawn. A repeat succeeds.
    Cancel {
        agent: AgentId,
        /// Close the bound task with this reason, when it is open.
        #[serde(default)]
        close_task: Option<CloseReason>,
    },
    /// The queued spawns, in the order that they will be placed.
    SpawnQueue,
    /// The session log of any agent, for `borant logs`.
    AgentHistory {
        agent: AgentId,
        #[serde(default)]
        page: usize,
        #[serde(default = "default_page_size")]
        page_size: usize,
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
        /// Switch the agent to this member of its bundle, on the target host
        /// (the current host when `to.host` is absent).
        #[serde(default)]
        member: Option<MemberName>,
    },
    Drain {
        host: HostName,
    },
    /// Clears the drain flag of a host, so the hub places agents on it again.
    /// A host that is not draining is a no-op.
    Undrain {
        host: HostName,
    },
    /// Deletes the record of an offline host that runs no agent, so that
    /// `borant hosts` and placement stop naming a box that is gone. Only a
    /// human may call it.
    RemoveHost {
        host: HostName,
    },
    ListAgents,
    ListHosts,
    /// Pins the agent or unpins it. Only a human may call it.
    Pin {
        agent: AgentId,
        on: bool,
    },
    /// Stops an agent or a channel: no placement, no claim expiry, no stall
    /// alert, and the session stops at the next turn end (or is fenced after
    /// `grace_secs`). A person or the channel owner may call it.
    Stop {
        selector: PauseSelector,
        #[serde(default)]
        grace_secs: Option<u32>,
    },
    /// Resumes a stopped agent or channel and places the agent again.
    Resume {
        selector: PauseSelector,
    },
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
    /// The live view of the swarm, for `borant top` and `borant status --json`.
    View,
    /// The latest usage report of one bundle member, from a human.
    UsageReport {
        bundle: BundleName,
        member: MemberName,
        windows: Vec<UsageWindow>,
    },
    /// The members of each bundle, with their state and their latest report.
    ListMembers,
    /// The `[schedules.<name>]` of `hub.toml`, with the next and the last
    /// fire of each. A person asks this from `borant schedules`.
    Schedules,
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
        /// The load and counters of the host, for the live view.
        #[serde(default)]
        stats: Option<HostStats>,
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
        /// The idempotency key of a spawn or a task create. A retry with the
        /// same key and the same arguments returns the first result.
        #[serde(default)]
        key: Option<String>,
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
        /// The launch timeout that the host waited, so the hub can say why
        /// the spawn failed. An older host sends none.
        #[serde(default)]
        timeout_secs: Option<f64>,
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
    PrepareMove {
        agent: AgentId,
        session: SessionId,
    },
    Fence {
        session: SessionId,
        reason: String,
    },
    Stop {
        session: SessionId,
    },
    /// Parks an idle session: a final checkpoint, then the harness stops, so
    /// the hub can delete the host. The host refuses a session in a turn.
    Park {
        session: SessionId,
    },
    /// Deletes the work dir of a retired agent, so its clone does not fill the
    /// disk. The host keeps it while a session of the agent runs there.
    DeleteWork {
        agent: AgentId,
    },
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
    /// The bundles of the collection, the role, the channel, and the spawn, in
    /// that order, without duplicates. The host resolves them after the
    /// harness defaults, so a later bundle wins.
    #[serde(default)]
    pub bundles: Vec<BundleName>,
    /// The author and committer of this agent's commits, from its collection.
    #[serde(default)]
    pub git_author: Option<GitAuthor>,
    /// The sandbox that this launch asks of its host, from its role.
    #[serde(default)]
    pub sandbox: Option<SandboxKind>,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: an order always names its agent.
#[cfg(any(test, feature = "testing"))]
impl Default for LaunchOrder {
    fn default() -> Self {
        Self {
            agent: "test-agent".parse().expect("a valid agent ID"),
            harness: Harness::ClaudeCode,
            repo: None,
            wip: None,
            channels: Vec::new(),
            prompt: String::new(),
            reason: MoveReason::Spawn,
            from_harness: None,
            from_host: None,
            model: None,
            project: None,
            reserve_secs: None,
            members: BTreeMap::new(),
            bundles: Vec::new(),
            git_author: None,
            sandbox: None,
        }
    }
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
    /// The role that a person started this session with.
    #[serde(default)]
    pub role: Option<RoleName>,
    /// The collection that a person started this session in.
    #[serde(default)]
    pub collection: Option<CollectionName>,
}

/// A record with placeholder values, for tests that fill the fields they
/// read. Production code has none: a session always names its directory.
#[cfg(any(test, feature = "testing"))]
impl Default for LocalRegister {
    fn default() -> Self {
        Self {
            harness: Harness::ClaudeCode,
            harness_session_id: None,
            pid: None,
            cwd: String::new(),
            agent: None,
            channel: None,
            role: None,
            collection: None,
        }
    }
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
    /// The agent is pinned, so a pause keeps its tools.
    #[serde(default)]
    pub pinned: bool,
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
    /// The session stopped at the usage limit of a bundle member. The host adds
    /// the bundle and the member of the session and tells the hub.
    MemberLimited {
        /// The bundle, when the adapter knows it; otherwise the only member
        /// bundle of the session.
        #[serde(default)]
        bundle: Option<BundleName>,
        /// When the member resets, when the harness knows.
        #[serde(default)]
        resets_at: Option<Timestamp>,
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

    /// A host before this version names no launch timeout, and its
    /// `LaunchFailed` still parses.
    #[test]
    fn launch_failed_parses_without_a_timeout() {
        let req: CtlRequest = serde_json::from_str(
            r#"{"op":"launch_failed","agent":"w1","host":"h1","error":"stuck"}"#,
        )
        .unwrap();
        assert_eq!(
            req,
            CtlRequest::LaunchFailed {
                agent: "w1".parse().unwrap(),
                host: "h1".parse().unwrap(),
                error: "stuck".into(),
                timeout_secs: None,
            }
        );
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
    fn every_known_error_kind_parses_and_round_trips() {
        let cases = [
            ("invalid", ApiErrorCode::Invalid),
            ("not_found", ApiErrorCode::NotFound),
            ("conflict", ApiErrorCode::Conflict),
            ("stale_token", ApiErrorCode::StaleToken),
            ("unavailable", ApiErrorCode::Unavailable),
            ("internal", ApiErrorCode::Internal),
            ("denied", ApiErrorCode::Denied),
        ];
        for (name, code) in cases {
            let wire = serde_json::json!({"code": name, "message": "no"});
            let err: ApiError = serde_json::from_value(wire.clone()).expect(name);
            assert_eq!(err.code, code, "{name}");
            assert_eq!(err.to_string(), format!("{name}: no"));
            let back = serde_json::to_value(&err).expect(name);
            assert_eq!(
                back,
                serde_json::json!({"code": name, "message": "no"}),
                "{name}"
            );
        }
    }

    /// A newer daemon sends a kind that this build predates, and the refusal
    /// still reaches the caller with the kind and the message.
    #[test]
    fn an_unknown_error_kind_keeps_its_name_and_message() {
        let reply: Reply = serde_json::from_str(
            r#"{"error":{"code":"rate_limited","message":"slow down, agent"}}"#,
        )
        .expect("a reply with a kind this build does not know");
        let Reply::Error(err) = reply else {
            panic!("an error reply: {reply:?}");
        };
        assert_eq!(err.code, ApiErrorCode::Other("rate_limited".into()));
        assert_eq!(err.message, "slow down, agent");
        assert_eq!(err.to_string(), "rate_limited: slow down, agent");
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            serde_json::json!({"code": "rate_limited", "message": "slow down, agent"})
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

    #[test]
    fn the_durable_tools_use_the_tool_tag() {
        let wait: ToolCall = serde_json::from_str(
            r#"{"tool":"wait","channel":"api","task":"01M5S7YF2GSX8GPG3YQ2K5KH4A","until":["closed"],"timeout_secs":60}"#,
        )
        .unwrap();
        assert!(
            matches!(&wait, ToolCall::Wait { until, timeout_secs: 60, .. }
                if until == &vec![TaskStatus::Closed]),
            "{wait:?}"
        );
        let stop: ToolCall = serde_json::from_str(
            r#"{"tool":"stop","selector":{"type":"agent","value":"w1"},"grace_secs":60}"#,
        )
        .unwrap();
        assert_eq!(
            stop,
            ToolCall::Stop {
                selector: PauseSelector::Agent("w1".parse().unwrap()),
                grace_secs: Some(60),
            }
        );
        let resume: ToolCall = serde_json::from_str(
            r#"{"tool":"resume","selector":{"type":"channel","value":"api"}}"#,
        )
        .unwrap();
        assert_eq!(
            resume,
            ToolCall::Resume {
                selector: PauseSelector::Channel("api".parse().unwrap()),
            }
        );
        let answer: ToolCall = serde_json::from_str(
            r#"{"tool":"answer","channel":"api","task":"01M5S7YF2GSX8GPG3YQ2K5KH4A","text":"go","expect_updated":5}"#,
        )
        .unwrap();
        assert!(
            matches!(
                answer,
                ToolCall::Answer {
                    expect_updated: Timestamp(5),
                    key: None,
                    ..
                }
            ),
            "{answer:?}"
        );
        assert_eq!(
            serde_json::from_str::<ToolCall>(r#"{"tool":"inbox"}"#).unwrap(),
            ToolCall::Inbox
        );
    }

    #[test]
    fn a_human_may_stop_resume_inbox_and_answer() {
        for call in [
            ToolCall::Stop {
                selector: PauseSelector::Channel("api".parse().unwrap()),
                grace_secs: None,
            },
            ToolCall::Resume {
                selector: PauseSelector::Agent("w1".parse().unwrap()),
            },
            ToolCall::Inbox,
            ToolCall::Schedules,
            ToolCall::Answer {
                channel: "api".parse().unwrap(),
                task: TaskId::new(),
                text: "go".into(),
                expect_updated: Timestamp(0),
                key: None,
            },
        ] {
            assert!(call.allowed_for_human(), "{call:?}");
        }
        let claim = ToolCall::TaskClaim {
            channel: "api".parse().unwrap(),
            id: TaskId::new(),
        };
        assert!(!claim.allowed_for_human(), "an agent claims a task");
    }

    #[test]
    fn a_human_may_remove_a_host() {
        let call = ToolCall::RemoveHost {
            host: "borant-3".parse().unwrap(),
        };
        assert!(call.allowed_for_human(), "a person removes a host");
        assert_eq!(
            serde_json::to_value(&call).unwrap(),
            serde_json::json!({ "tool": "remove_host", "host": "borant-3" })
        );
    }
}
