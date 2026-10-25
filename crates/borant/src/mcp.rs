//! `borant mcp`: a stdio MCP server that exposes the Borant tools to
//! Claude Code. It finds its session through its parent processes.

use std::time::Duration;

use borant_core::SessionId;
use borant_core::proto::{LocalSession, ToolCall};
use borant_host::LocalClient;
use borant_host::client::Outcome;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::config;

const CALL: Duration = Duration::from_secs(30);

/// The parent PIDs of this process, nearest first.
pub fn ancestors() -> Vec<u32> {
    let mut out = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..12 {
        if pid <= 1 {
            break;
        }
        out.push(pid);
        let Ok(o) = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
        else {
            break;
        };
        match String::from_utf8_lossy(&o.stdout).trim().parse() {
            Ok(p) => pid = p,
            Err(_) => break,
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct BorantMcp {
    client: LocalClient,
    session: std::sync::Arc<Mutex<Option<SessionId>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChannelName {
    /// The channel name: lowercase letters, digits, '-' and '_'.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChannelCreate {
    pub name: String,
    /// A JSON Schema object for task properties. Leave out for the default.
    pub schema: Option<Map<String, Value>>,
    /// A project name. A spawn in the channel clones the project.
    pub project: Option<String>,
    /// The base branch for spawns. Defaults to the default branch of the project.
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectRef {
    /// The project name.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChannelTransfer {
    pub name: String,
    /// The agent ID of the new owner. It must be a member.
    pub to: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendArgs {
    /// An agent ID for a direct message, or '#channel' for a broadcast.
    pub to: String,
    pub text: String,
    /// Wake every member for a broadcast. Direct messages always wake.
    pub urgent: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCreate {
    pub channel: String,
    pub title: String,
    pub body: Option<String>,
    /// IDs of tasks that must close before this task is ready.
    pub blocked_by: Option<Vec<String>>,
    /// The ID of a parent task, for grouping.
    pub parent: Option<String>,
    /// Properties that follow the task schema of the channel.
    pub properties: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskRef {
    pub channel: String,
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskList {
    pub channel: String,
    /// Only open tasks whose prerequisites are all closed.
    pub ready_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskUpdate {
    pub channel: String,
    pub id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    /// One of: open, in_progress, needs_human, stuck. Use task_close to close.
    pub status: Option<String>,
    /// A one-line reason, for needs_human or stuck.
    pub attention_msg: Option<String>,
    pub blocked_by: Option<Vec<String>>,
    /// Properties to merge. A null value removes a key.
    pub properties: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskClose {
    pub channel: String,
    pub id: String,
    /// One of: done, wontfix, duplicate, superseded.
    pub reason: String,
    /// What was done and how it was verified.
    pub message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChannelRef {
    pub channel: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SchemaExtend {
    pub channel: String,
    /// New property definitions. Existing keys cannot change.
    pub properties: Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Spawn {
    pub channel: String,
    /// The first prompt of the new agent.
    pub prompt: String,
    /// One of: claude-code, pi, omp. Defaults to your harness.
    pub harness: Option<String>,
    /// A host name, to pick the host yourself.
    pub host: Option<String>,
    /// An agent ID for the new agent.
    pub name: Option<String>,
    /// The model for the harness, for example 'opus'. Defaults to the harness default.
    pub model: Option<String>,
    /// An idempotency key: a retry with the same key and arguments returns the first agent.
    pub key: Option<String>,
    /// Bind the worker to an existing task, as CHANNEL/ID.
    pub task: Option<String>,
    /// Bind the worker to a new task with this title, in the channel.
    pub task_title: Option<String>,
    /// Stop the session when the bound task closes. True by default for a bound worker.
    pub stop_on_close: Option<bool>,
    /// Higher goes first in the spawn queue.
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct History {
    /// Page 0 holds the newest events.
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

fn target(to: &str) -> Value {
    match to.strip_prefix('#') {
        Some(c) => json!({"type": "channel", "id": c}),
        None => json!({"type": "agent", "id": to}),
    }
}

fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        other => other,
    }
}

#[tool_router]
impl BorantMcp {
    pub fn new(client: LocalClient) -> Self {
        Self {
            client,
            session: Default::default(),
            tool_router: Self::tool_router(),
        }
    }

    async fn session(&self) -> Result<SessionId, String> {
        let mut s = self.session.lock().await;
        if let Some(id) = *s {
            return Ok(id);
        }
        let body = json!({
            "ancestors": ancestors(),
            "harness_session_id": std::env::var("CLAUDE_CODE_SESSION_ID").ok(),
        });
        match self
            .client
            .post::<_, LocalSession>("/v1/resolve", &body, CALL)
            .await
        {
            Outcome::Reply(Ok(found)) => {
                *s = Some(found.session);
                Ok(found.session)
            }
            Outcome::Reply(Err(e)) => Err(format!(
                "Borant does not know this session yet: {}",
                e.message
            )),
            Outcome::Down(_) => {
                Err("The Borant host daemon is not running on this machine.".into())
            }
        }
    }

    async fn forward(&self, call: Value) -> Result<String, String> {
        let call: ToolCall = serde_json::from_value(strip_nulls(call))
            .map_err(|e| format!("invalid arguments: {e}"))?;
        // After `/clear`, Claude Code keeps this process, but the daemon has
        // a new session. Look the session up again once.
        for attempt in 0..2 {
            let session = self.session().await?;
            match self
                .client
                .post::<_, Value>("/v1/call", &json!({"session": session, "call": call}), CALL)
                .await
            {
                Outcome::Reply(Ok(v)) => {
                    return Ok(serde_json::to_string_pretty(&v).unwrap_or_default());
                }
                Outcome::Reply(Err(e))
                    if attempt == 0
                        && e.code == borant_core::proto::ApiErrorCode::NotFound
                        && e.message.contains("no local session") =>
                {
                    *self.session.lock().await = None;
                }
                Outcome::Reply(Err(e)) => return Err(e.message),
                Outcome::Down(e) => {
                    return Err(format!("The Borant host daemon is not reachable: {e}"));
                }
            }
        }
        Err("Borant does not know this session.".into())
    }

    #[tool(description = "Show your Borant agent ID, host, channels, and the channels you own.")]
    async fn whoami(&self, Parameters(_): Parameters<NoArgs>) -> Result<String, String> {
        self.forward(json!({"tool": "whoami"})).await
    }

    #[tool(description = "Create a channel. You become its owner.")]
    async fn channel_create(
        &self,
        Parameters(a): Parameters<ChannelCreate>,
    ) -> Result<String, String> {
        self.forward(json!({"tool": "channel_create", "name": a.name, "schema": a.schema, "project": a.project, "branch": a.branch}))
            .await
    }

    #[tool(description = "Join a channel to get its broadcasts and task events.")]
    async fn channel_join(&self, Parameters(a): Parameters<ChannelName>) -> Result<String, String> {
        self.forward(json!({"tool": "channel_join", "name": a.name}))
            .await
    }

    #[tool(description = "Leave a channel.")]
    async fn channel_leave(
        &self,
        Parameters(a): Parameters<ChannelName>,
    ) -> Result<String, String> {
        self.forward(json!({"tool": "channel_leave", "name": a.name}))
            .await
    }

    #[tool(description = "List all channels.")]
    async fn channel_list(&self, Parameters(_): Parameters<NoArgs>) -> Result<String, String> {
        self.forward(json!({"tool": "channel_list"})).await
    }

    #[tool(description = "Give the ownership of a channel to another member.")]
    async fn channel_transfer(
        &self,
        Parameters(a): Parameters<ChannelTransfer>,
    ) -> Result<String, String> {
        self.forward(json!({"tool": "channel_transfer", "name": a.name, "to": a.to}))
            .await
    }

    #[tool(
        description = "List the members of a channel, with their host and whether they are online."
    )]
    async fn members(&self, Parameters(a): Parameters<ChannelName>) -> Result<String, String> {
        self.forward(json!({"tool": "members", "name": a.name}))
            .await
    }

    #[tool(description = "Send a direct message to an agent, or a broadcast to '#channel'.")]
    async fn send(&self, Parameters(a): Parameters<SendArgs>) -> Result<String, String> {
        self.forward(json!({"tool": "send", "to": target(&a.to), "text": a.text, "urgent": a.urgent.unwrap_or(false)}))
            .await
    }

    #[tool(description = "Create a task in a channel.")]
    async fn task_create(&self, Parameters(a): Parameters<TaskCreate>) -> Result<String, String> {
        let task = strip_nulls(json!({
            "title": a.title, "body": a.body.unwrap_or_default(), "blocked_by": a.blocked_by.unwrap_or_default(),
            "parent": a.parent, "properties": a.properties.unwrap_or_default(),
        }));
        self.forward(json!({"tool": "task_create", "channel": a.channel, "task": task}))
            .await
    }

    #[tool(description = "Show one task.")]
    async fn task_get(&self, Parameters(a): Parameters<TaskRef>) -> Result<String, String> {
        self.forward(json!({"tool": "task_get", "channel": a.channel, "id": a.id}))
            .await
    }

    #[tool(
        description = "List the tasks of a channel. Set ready_only to see tasks you can start now."
    )]
    async fn task_list(&self, Parameters(a): Parameters<TaskList>) -> Result<String, String> {
        self.forward(json!({"tool": "task_list", "channel": a.channel, "ready_only": a.ready_only.unwrap_or(false)}))
            .await
    }

    #[tool(
        description = "Change a task: title, body, status, attention message, prerequisites, or properties."
    )]
    async fn task_update(&self, Parameters(a): Parameters<TaskUpdate>) -> Result<String, String> {
        let patch = strip_nulls(json!({
            "title": a.title, "body": a.body, "status": a.status, "attention_msg": a.attention_msg,
            "blocked_by": a.blocked_by, "properties": a.properties,
        }));
        self.forward(
            json!({"tool": "task_update", "channel": a.channel, "id": a.id, "patch": patch}),
        )
        .await
    }

    #[tool(description = "Claim a task so that you work on it. Fails if another agent holds it.")]
    async fn task_claim(&self, Parameters(a): Parameters<TaskRef>) -> Result<String, String> {
        self.forward(json!({"tool": "task_claim", "channel": a.channel, "id": a.id}))
            .await
    }

    #[tool(description = "Close a task when its work is done and verified.")]
    async fn task_close(&self, Parameters(a): Parameters<TaskClose>) -> Result<String, String> {
        self.forward(json!({"tool": "task_close", "channel": a.channel, "id": a.id, "reason": a.reason, "message": a.message}))
            .await
    }

    #[tool(description = "Show the task schema of a channel.")]
    async fn schema_get(&self, Parameters(a): Parameters<ChannelRef>) -> Result<String, String> {
        self.forward(json!({"tool": "schema_get", "channel": a.channel}))
            .await
    }

    #[tool(description = "Add property definitions to the task schema of a channel you own.")]
    async fn schema_extend(
        &self,
        Parameters(a): Parameters<SchemaExtend>,
    ) -> Result<String, String> {
        self.forward(
            json!({"tool": "schema_extend", "channel": a.channel, "properties": a.properties}),
        )
        .await
    }

    #[tool(description = "Start a new worker agent in a channel on a host that Borant picks.")]
    async fn spawn(&self, Parameters(a): Parameters<Spawn>) -> Result<String, String> {
        let task = match (a.task.as_deref(), a.task_title.as_deref()) {
            (Some(text), _) => {
                let (channel, id) = text
                    .split_once('/')
                    .ok_or_else(|| format!("{text} is not CHANNEL/ID"))?;
                Some(json!({ "channel": channel, "id": id }))
            }
            (None, Some(title)) => Some(json!({ "channel": a.channel, "title": title })),
            (None, None) => None,
        };
        self.forward(json!({
            "tool": "spawn",
            "channel": a.channel,
            "prompt": a.prompt,
            "harness": a.harness,
            "host": a.host,
            "name": a.name,
            "model": a.model,
            "key": a.key,
            "task": task,
            "stop_on_close": a.stop_on_close,
            "priority": a.priority,
        }))
        .await
    }

    #[tool(description = "List all projects: git repositories that spawns clone.")]
    async fn project_list(&self, Parameters(_): Parameters<NoArgs>) -> Result<String, String> {
        self.forward(json!({"tool": "project_list"})).await
    }

    #[tool(description = "Show one project.")]
    async fn project_get(&self, Parameters(a): Parameters<ProjectRef>) -> Result<String, String> {
        self.forward(json!({"tool": "project_get", "name": a.name}))
            .await
    }

    #[tool(
        description = "Read your Borant session history, newest page first. Use it after a move to see what you did before."
    )]
    async fn session_history(&self, Parameters(a): Parameters<History>) -> Result<String, String> {
        self.forward(json!({"tool": "session_history", "page": a.page.unwrap_or(0), "page_size": a.page_size.unwrap_or(100)}))
            .await
    }
}

#[tool_handler]
impl ServerHandler for BorantMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

pub async fn run() -> anyhow::Result<()> {
    let server = BorantMcp::new(LocalClient::new(config::socket()));
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_all_tools() {
        let server = BorantMcp::new(LocalClient::new("/nonexistent".into()));
        let mut names: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "channel_create",
                "channel_join",
                "channel_leave",
                "channel_list",
                "channel_transfer",
                "members",
                "project_get",
                "project_list",
                "schema_extend",
                "schema_get",
                "send",
                "session_history",
                "spawn",
                "task_claim",
                "task_close",
                "task_create",
                "task_get",
                "task_list",
                "task_update",
                "whoami",
            ]
        );
    }

    #[test]
    fn targets_parse() {
        assert_eq!(target("#api"), json!({"type": "channel", "id": "api"}));
        assert_eq!(target("w1"), json!({"type": "agent", "id": "w1"}));
    }

    #[test]
    fn task_update_patch_round_trips() {
        let patch =
            strip_nulls(json!({"status": "stuck", "attention_msg": "need creds", "title": null}));
        let call: ToolCall = serde_json::from_value(json!({"tool": "task_update", "channel": "api", "id": "01JABCDEFGHJKMNPQRSTVWXYZ0", "patch": patch})).unwrap();
        assert!(matches!(call, ToolCall::TaskUpdate { .. }));
    }
}
