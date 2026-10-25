//! Claude Code hook handlers: `borant hook <event>`.
//!
//! Every handler reads the hook JSON from stdin. If the host daemon is not
//! running, a handler exits 0 with no output, so Claude Code works as if
//! Borant were absent.

use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use borant_core::SessionId;
use borant_core::model::{Envelope, Harness};
use borant_core::proto::{EventReply, LocalEvent, LocalRegister, LocalSession, format_delivery};
use borant_host::LocalClient;
use borant_host::client::Outcome;
use serde_json::{Value, json};

use crate::config;

const CALL: Duration = Duration::from_secs(5);
/// The long poll of the idle waiter. The hook timeout is much longer.
const WAIT_MS: u64 = 300_000;

/// The text that tells Claude about Borant at session start.
pub fn start_context(agent: &str, host: &str) -> String {
    format!(
        "You are Borant agent {agent} on host {host}. Use the borant MCP tools to send messages and work on tasks. Messages from other agents arrive as system reminders that start with [borant]."
    )
}

pub fn deliveries(msgs: &[Envelope]) -> String {
    msgs.iter()
        .map(format_delivery)
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn context_output(event: &str, text: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } })
}

pub fn deny_output(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

/// The deny reply for a paused session, or `None` when every tool may run.
///
/// A pinned agent keeps every tool during a pause: the hub never starts a
/// second session of a pinned agent, so fencing does not need to stop it. A
/// movable session keeps the full block, because a takeover may start it
/// elsewhere.
pub fn pause_denial(s: &LocalSession) -> Option<Value> {
    match &s.fenced {
        Some(reason) if !s.pinned => Some(deny_output(reason)),
        _ => None,
    }
}

/// The `ToolCall` and `ToolResult` events of a `PostToolUse` input.
pub fn tool_events(input: &Value) -> (LocalEvent, LocalEvent) {
    let name = input["tool_name"].as_str().unwrap_or("unknown").to_owned();
    let as_text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let response = &input["tool_response"];
    let is_error = response
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || response.get("error").is_some_and(|e| !e.is_null());
    (
        LocalEvent::ToolCall {
            name: name.clone(),
            input: as_text(&input["tool_input"]),
        },
        LocalEvent::ToolResult {
            name,
            output: as_text(response),
            is_error,
        },
    )
}

fn claude_pid() -> u32 {
    std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(std::os::unix::process::parent_id)
}

fn cache_path(claude_session: &str) -> PathBuf {
    config::home()
        .join("run")
        .join(format!("claude-{claude_session}.sid"))
}

struct Ctx {
    client: LocalClient,
    input: Value,
}

impl Ctx {
    fn claude_session(&self) -> &str {
        self.input["session_id"].as_str().unwrap_or_default()
    }

    async fn session(&self) -> Option<SessionId> {
        if let Some(id) = std::fs::read_to_string(cache_path(self.claude_session()))
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return Some(id);
        }
        let body =
            json!({ "ancestors": [claude_pid()], "harness_session_id": self.claude_session() });
        match self
            .client
            .post::<_, LocalSession>("/v1/resolve", &body, CALL)
            .await
        {
            Outcome::Reply(Ok(s)) => Some(s.session),
            _ => None,
        }
    }

    async fn event(&self, id: SessionId, ev: &LocalEvent) -> Option<EventReply> {
        match self
            .client
            .post::<_, EventReply>(&format!("/v1/sessions/{id}/events"), ev, CALL)
            .await
        {
            Outcome::Reply(Ok(r)) => Some(r),
            _ => None,
        }
    }
}

fn print(v: &Value) {
    println!("{v}");
}

/// Runs one hook and returns the process exit code.
pub async fn run(event: &str) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin()
        .take(16 * 1024 * 1024)
        .read_to_string(&mut raw);
    let input: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let ctx = Ctx {
        client: LocalClient::new(config::socket()),
        input,
    };
    if !ctx.client.socket().exists() {
        return 0;
    }
    match event {
        "session-start" => session_start(&ctx).await,
        "user-prompt-submit" => {
            let Some(id) = ctx.session().await else {
                return 0;
            };
            let text = ctx.input["prompt"].as_str().unwrap_or_default().to_owned();
            if let Some(r) = ctx.event(id, &LocalEvent::UserPrompt { text }).await
                && !r.messages.is_empty()
            {
                print(&context_output(
                    "UserPromptSubmit",
                    &deliveries(&r.messages),
                ));
            }
            0
        }
        "pre-tool-use" => {
            let Some(id) = ctx.session().await else {
                return 0;
            };
            if let Outcome::Reply(Ok(s)) = ctx
                .client
                .get::<LocalSession>(&format!("/v1/sessions/{id}"), CALL)
                .await
                && let Some(deny) = pause_denial(&s)
            {
                print(&deny);
            }
            0
        }
        "post-tool-use" => {
            let Some(id) = ctx.session().await else {
                return 0;
            };
            let (call, result) = tool_events(&ctx.input);
            let mut msgs = Vec::new();
            for ev in [call, result] {
                if let Some(r) = ctx.event(id, &ev).await {
                    msgs.extend(r.messages);
                }
            }
            if !msgs.is_empty() {
                print(&context_output("PostToolUse", &deliveries(&msgs)));
            }
            0
        }
        "stop" => {
            let Some(id) = ctx.session().await else {
                return 0;
            };
            if let Some(text) = ctx.input["last_assistant_message"].as_str() {
                ctx.event(
                    id,
                    &LocalEvent::AssistantMessage {
                        text: text.to_owned(),
                    },
                )
                .await;
            }
            ctx.event(id, &LocalEvent::TurnEnd).await;
            0
        }
        "stop-wait" => stop_wait(&ctx).await,
        "session-end" => {
            let Some(id) = ctx.session().await else {
                return 0;
            };
            let reason = ctx.input["reason"]
                .as_str()
                .unwrap_or("session end")
                .to_owned();
            ctx.event(id, &LocalEvent::End { reason }).await;
            let _ = std::fs::remove_file(cache_path(ctx.claude_session()));
            0
        }
        _ => 0,
    }
}

async fn session_start(ctx: &Ctx) -> i32 {
    let reg = LocalRegister {
        harness: Harness::ClaudeCode,
        harness_session_id: Some(ctx.claude_session().to_owned()).filter(|s| !s.is_empty()),
        pid: Some(claude_pid()),
        cwd: ctx.input["cwd"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|d| d.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
        agent: std::env::var("BORANT_AGENT")
            .ok()
            .and_then(|a| a.parse().ok()),
        channel: std::env::var("BORANT_CHANNEL")
            .ok()
            .and_then(|c| c.parse().ok()),
        // A chief that a person starts reports its role and collection.
        role: std::env::var("BORANT_ROLE")
            .ok()
            .and_then(|r| r.parse().ok()),
        collection: std::env::var("BORANT_COLLECTION")
            .ok()
            .and_then(|c| c.parse().ok()),
    };
    let Outcome::Reply(Ok(s)) = ctx
        .client
        .post::<_, LocalSession>("/v1/sessions", &reg, CALL)
        .await
    else {
        return 0;
    };
    let path = cache_path(ctx.claude_session());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, s.session.to_string());
    let text = match &s.agent {
        Some(a) => start_context(a.as_str(), s.host.as_str()),
        None => "Borant is installed, but this session is not connected to the Borant hub yet. The borant tools start to work when the hub is reachable.".to_owned(),
    };
    print(&context_output("SessionStart", &text));
    0
}

/// The idle waiter (`asyncRewake`). It waits for messages that wake the
/// session, prints them to stderr, and exits 2 to start a new turn.
async fn stop_wait(ctx: &Ctx) -> i32 {
    // At session start, this waiter can run before the registration ends.
    let mut found = None;
    for _ in 0..30 {
        found = ctx.session().await;
        if found.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let Some(id) = found else { return 0 };
    let run = config::home().join("run");
    let _ = std::fs::create_dir_all(&run);
    let pidfile = run.join(format!("waiter-{id}.pid"));
    if let Some(old) = std::fs::read_to_string(&pidfile)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        && old != std::process::id() as i32
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(old),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    let me = std::process::id().to_string();
    let _ = std::fs::write(&pidfile, &me);
    let claude = claude_pid();
    let path = format!("/v1/sessions/{id}/wait?timeout_ms={WAIT_MS}");
    loop {
        // Another waiter took over, or Claude Code exited.
        if std::fs::read_to_string(&pidfile).ok().as_deref() != Some(me.as_str()) {
            return 0;
        }
        if !borant_alive(claude) {
            let _ = std::fs::remove_file(&pidfile);
            return 0;
        }
        let started = Instant::now();
        match ctx
            .client
            .get::<Value>(&path, Duration::from_millis(WAIT_MS + 10_000))
            .await
        {
            Outcome::Reply(Ok(v)) => {
                let msgs: Vec<Envelope> =
                    serde_json::from_value(v["messages"].clone()).unwrap_or_default();
                if !msgs.is_empty() {
                    eprintln!("{}", deliveries(&msgs));
                    let _ = std::fs::remove_file(&pidfile);
                    return 2;
                }
                if started.elapsed() < Duration::from_secs(1) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            Outcome::Reply(Err(e)) if e.code == borant_core::proto::ApiErrorCode::NotFound => {
                let _ = std::fs::remove_file(&pidfile);
                return 0;
            }
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
}

fn borant_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None),
        Ok(()) | Err(Errno::EPERM)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::model::{EnvelopeKind, Sender, Target};
    use borant_core::{MessageId, Timestamp};

    #[test]
    fn maps_post_tool_use_input() {
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
            "tool_response": {"stdout": "a\nb", "is_error": true}
        });
        let (call, result) = tool_events(&input);
        assert_eq!(
            call,
            LocalEvent::ToolCall {
                name: "Bash".into(),
                input: r#"{"command":"ls"}"#.into()
            }
        );
        let LocalEvent::ToolResult {
            output, is_error, ..
        } = result
        else {
            panic!()
        };
        assert!(output.contains("stdout"));
        assert!(is_error);
    }

    #[test]
    fn formats_delivery_text() {
        let env = Envelope {
            id: MessageId::new(),
            from: Sender::Agent("boss".parse().unwrap()),
            to: Target::Agent("w1".parse().unwrap()),
            kind: EnvelopeKind::Direct,
            text: "go".into(),
            urgent: false,
            event: None,
            sent: Timestamp(0),
        };
        assert_eq!(
            deliveries(std::slice::from_ref(&env)),
            format!(
                "[borant] direct message from boss to w1 (id {}):\ngo",
                env.id
            )
        );
    }

    #[test]
    fn pre_tool_use_denies_when_fenced() {
        assert_eq!(
            deny_output("moved"),
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "moved"}})
        );
    }

    fn paused(pinned: bool) -> LocalSession {
        LocalSession {
            session: borant_core::SessionId::new(),
            agent: Some("w1".parse().unwrap()),
            host: "h1".parse().unwrap(),
            state: borant_core::proto::LocalState::Attached,
            movable: true,
            channels: Vec::new(),
            fenced: Some(
                "The Borant hub is unreachable, so tools are paused until it returns.".into(),
            ),
            pinned,
        }
    }

    #[test]
    fn hook_allows_a_paused_pinned_session() {
        assert_eq!(pause_denial(&paused(true)), None);
        // A pinned session that is not fenced gets nothing either.
        let mut s = paused(true);
        s.fenced = None;
        assert_eq!(pause_denial(&s), None);
    }

    #[test]
    fn hook_blocks_a_paused_movable_session() {
        let s = paused(false);
        assert_eq!(
            pause_denial(&s),
            Some(deny_output(s.fenced.as_deref().unwrap()))
        );
    }

    #[test]
    fn start_context_names_agent_and_host() {
        assert_eq!(
            start_context("w1", "vps-1"),
            "You are Borant agent w1 on host vps-1. Use the borant MCP tools to send messages and work on tasks. Messages from other agents arrive as system reminders that start with [borant]."
        );
    }
}
