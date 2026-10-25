//! A scripted stand-in for a harness, for the Docker multi-host tests.
//!
//! It registers with the local host daemon, waits for messages like the
//! Claude Code idle waiter, and runs commands that it finds in message text,
//! one per line:
//!
//! - `WRITE <path> <text>`: write a file, as a tool call
//! - `READ <path>`: read a file, as a tool call, and append the result to
//!   `reads.log` in the working directory (a sandbox test reads it outside)
//! - `CHANNEL <name> [project]` and `JOIN <name>`: create or join a channel
//! - `CREATE <channel> <title>`: create a task
//! - `AFTER <channel> <blocker> <title>`: create a task blocked by another
//! - `CLAIM <channel> <task>` and `CLOSE <channel> <task>`
//! - `SEND <agent|#channel> <text>`
//! - `SPAWN <channel> <prompt>`: spawn an agent, and log `SPAWNED <agent>`
//! - `SPAWNROLE <channel> <role> <prompt>`: the same with a role of `hub.toml`
//! - `LIMIT <resets_at>`: report the usage limit of the bundle member, as the
//!   Claude Code `StopFailure` hook does, with the reset time in RFC 3339
//! - `HISTORY`: read the session history
//! - `EXIT`: end the session and exit
//!
//! It writes every event to `$BORANT_HOME/fake/{agent}.log`.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use borant_core::SessionId;
use borant_core::model::{Envelope, Harness};
use borant_core::proto::{EventReply, LocalEvent, LocalRegister, LocalSession, LocalState};
use borant_host::LocalClient;
use borant_host::client::Outcome;
use serde_json::{Value, json};

const CALL: Duration = Duration::from_secs(15);

struct Fake {
    client: LocalClient,
    session: SessionId,
    log: std::fs::File,
}

fn home() -> PathBuf {
    std::env::var_os("BORANT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".borant"))
}

impl Fake {
    fn line(&mut self, s: &str) {
        let _ = writeln!(self.log, "{s}");
        let _ = self.log.flush();
    }

    /// Posts one event and writes the messages that the reply carries. The
    /// host drains its queue into the first request that can show messages,
    /// so a harness that drops them loses them.
    async fn event(&mut self, ev: LocalEvent) -> anyhow::Result<EventReply> {
        let reply = match self
            .client
            .post::<_, EventReply>(&format!("/v1/sessions/{}/events", self.session), &ev, CALL)
            .await
        {
            Outcome::Reply(r) => r?,
            Outcome::Down(e) => return Err(e),
        };
        self.show(&reply.messages);
        Ok(reply)
    }

    /// Writes messages the way the harness shows them to its model.
    fn show(&mut self, msgs: &[Envelope]) {
        for m in msgs {
            self.line(&format!(
                "MSG from={} kind={:?} {}",
                m.from,
                m.kind,
                m.text.replace('\n', " / ")
            ));
        }
    }

    async fn call(&self, call: Value) -> anyhow::Result<Value> {
        let body = json!({ "session": self.session, "call": call });
        match self.client.post::<_, Value>("/v1/call", &body, CALL).await {
            Outcome::Reply(r) => Ok(r?),
            Outcome::Down(e) => Err(e),
        }
    }

    async fn run_command(&mut self, line: &str) -> anyhow::Result<bool> {
        let mut parts = line.splitn(3, ' ');
        let cmd = parts.next().unwrap_or_default();
        let a = parts.next().unwrap_or_default().to_owned();
        let b = parts.next().unwrap_or_default().to_owned();
        match cmd {
            "WRITE" => {
                std::fs::write(&a, format!("{b}\n")).with_context(|| format!("write {a}"))?;
                self.event(LocalEvent::ToolCall {
                    name: "write".into(),
                    input: json!({"path": a}).to_string(),
                })
                .await?;
                let r = self
                    .event(LocalEvent::ToolResult {
                        name: "write".into(),
                        output: format!("wrote {a}"),
                        is_error: false,
                    })
                    .await?;
                if let Some(f) = r.fenced {
                    self.line(&format!("FENCED {f}"));
                }
                self.line(&format!("WROTE {a}"));
            }
            "READ" => {
                let read = std::fs::read_to_string(&a);
                let ok = read.is_ok();
                let detail = match &read {
                    Ok(text) => format!("{} bytes", text.len()),
                    Err(e) => e.to_string(),
                };
                self.event(LocalEvent::ToolCall {
                    name: "read".into(),
                    input: json!({"path": a.clone()}).to_string(),
                })
                .await?;
                let r = self
                    .event(LocalEvent::ToolResult {
                        name: "read".into(),
                        output: format!("read {a}: {detail}"),
                        is_error: !ok,
                    })
                    .await?;
                if let Some(f) = r.fenced {
                    self.line(&format!("FENCED {f}"));
                }
                self.line(&format!("READ {a} {}", if ok { "ok" } else { "failed" }));
                // The line is visible outside a sandbox, where the fake log of
                // the agent is not: the clone is bound, its home is not.
                append_read(&a, ok)?;
            }
            "CREATE" => {
                let v = self
                    .call(json!({"tool": "task_create", "channel": a, "task": {"title": b}}))
                    .await?;
                self.line(&format!(
                    "CREATED {} {}",
                    v["id"].as_str().unwrap_or("?"),
                    b
                ));
            }
            "CHANNEL" => {
                let mut create = json!({"tool": "channel_create", "name": a});
                if !b.is_empty() {
                    create["project"] = json!(b);
                }
                let r = self.call(create).await;
                self.line(&format!(
                    "CHANNEL {a} {}",
                    if r.is_ok() { "ok" } else { "failed" }
                ));
            }
            "JOIN" => {
                let r = self.call(json!({"tool": "channel_join", "name": a})).await;
                self.line(&format!(
                    "JOIN {a} {}",
                    if r.is_ok() { "ok" } else { "failed" }
                ));
            }
            "AFTER" => {
                let (blocker, title) = b.split_once(' ').unwrap_or((b.as_str(), "task"));
                let v = self
                    .call(json!({"tool": "task_create", "channel": a, "task": {"title": title, "blocked_by": [blocker]}}))
                    .await?;
                self.line(&format!(
                    "CREATED {} {}",
                    v["id"].as_str().unwrap_or("?"),
                    title
                ));
            }
            "CLAIM" => {
                let r = self
                    .call(json!({"tool": "task_claim", "channel": a, "id": b}))
                    .await;
                self.line(&format!(
                    "CLAIM {b} {}",
                    if r.is_ok() { "ok" } else { "failed" }
                ));
            }
            "CLOSE" => {
                let r = self
                    .call(json!({"tool": "task_close", "channel": a, "id": b, "reason": "done", "message": "done by the fake harness"}))
                    .await;
                self.line(&format!(
                    "CLOSE {b} {}",
                    if r.is_ok() { "ok" } else { "failed" }
                ));
            }
            "SEND" => {
                let to = match a.strip_prefix('#') {
                    Some(c) => json!({"type": "channel", "id": c}),
                    None => json!({"type": "agent", "id": a}),
                };
                self.call(json!({"tool": "send", "to": to, "text": b}))
                    .await?;
                self.line(&format!("SENT {a}"));
            }
            "SPAWN" => {
                let v = self
                    .call(json!({"tool": "spawn", "channel": a, "prompt": b}))
                    .await?;
                self.line(&format!("SPAWNED {}", spawned_agent(&v)?));
            }
            "SPAWNROLE" => {
                // `role` then the prompt; a spawn needs a session, so a test
                // drives the sandboxed spawn through an agent.
                let (role, prompt) = b.split_once(' ').unwrap_or((b.as_str(), ""));
                let v = self
                    .call(json!({
                        "tool": "spawn",
                        "channel": a,
                        "prompt": prompt,
                        "role": role,
                    }))
                    .await?;
                self.line(&format!("SPAWNED {}", spawned_agent(&v)?));
            }
            "LIMIT" => {
                // The same event as `borant hook stop-failure`: the bundle
                // and the member come from the session record of the host.
                let resets_at =
                    iso8601_ms(&a).with_context(|| format!("LIMIT {a} is not an RFC 3339 time"))?;
                self.event(LocalEvent::MemberLimited {
                    bundle: None,
                    resets_at: Some(borant_core::Timestamp(resets_at)),
                })
                .await?;
                self.line(&format!("LIMITED {a}"));
            }
            "HISTORY" => self.history().await?,
            "EXIT" => {
                let _ = self
                    .event(LocalEvent::End {
                        reason: "fake harness exit".into(),
                    })
                    .await;
                self.line("EXIT");
                return Ok(false);
            }
            _ => {}
        }
        Ok(true)
    }

    async fn history(&mut self) -> anyhow::Result<()> {
        let v = self
            .call(json!({"tool": "session_history", "page": 0, "page_size": 500}))
            .await?;
        let events = v["events"].as_array().cloned().unwrap_or_default();
        let wrote: Vec<String> = events
            .iter()
            .filter(|e| e["kind"] == "tool_result")
            .filter_map(|e| e["output"].as_str().map(str::to_owned))
            .collect();
        self.line(&format!(
            "HISTORY total={} tool_results={}",
            v["total"],
            wrote.join("|")
        ));
        Ok(())
    }

    async fn handle(&mut self, msgs: Vec<Envelope>) -> anyhow::Result<bool> {
        self.event(LocalEvent::TurnStart).await?;
        let mut go_on = true;
        for m in msgs {
            self.line(&format!(
                "MSG from={} kind={:?} {}",
                m.from,
                m.kind,
                m.text.replace('\n', " / ")
            ));
            for line in m.text.lines() {
                match self.run_command(line.trim()).await {
                    Ok(true) => {}
                    Ok(false) => go_on = false,
                    Err(e) => self.line(&format!("ERROR {e:#}")),
                }
            }
        }
        self.event(LocalEvent::TurnEnd).await?;
        Ok(go_on)
    }
}

/// The agent id of a spawn reply: `{"agent": {"agent": <id>, "placed": <bool>}}`.
fn spawned_agent(v: &Value) -> anyhow::Result<&str> {
    v["agent"]["agent"]
        .as_str()
        .or_else(|| v["agent"].as_str())
        .with_context(|| format!("the spawn reply holds no agent id: {v}"))
}

/// Appends one `READ` result to `reads.log` in the working directory. A
/// sandbox test reads that file from the host: the fake log in the home of the
/// agent is inside the sandbox, and the clone is the bound directory.
fn append_read(path: &str, ok: bool) -> anyhow::Result<()> {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("reads.log")?;
    writeln!(log, "READ {path} {}", if ok { "ok" } else { "failed" })?;
    Ok(())
}

/// The unix milliseconds of an RFC 3339 time, the argument of `LIMIT`.
fn iso8601_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp_millis())
}

async fn register(client: &LocalClient) -> anyhow::Result<LocalSession> {
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let reg = LocalRegister {
        harness: Harness::Fake,
        harness_session_id: Some(format!("fake-{}", std::process::id())),
        pid: Some(std::process::id()),
        cwd,
        agent: std::env::var("BORANT_AGENT")
            .ok()
            .and_then(|a| a.parse().ok()),
        channel: std::env::var("BORANT_CHANNEL")
            .ok()
            .and_then(|c| c.parse().ok()),
        role: None,
        collection: None,
    };
    for _ in 0..60 {
        match client
            .post::<_, LocalSession>("/v1/sessions", &reg, CALL)
            .await
        {
            Outcome::Reply(r) => return Ok(r?),
            Outcome::Down(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    bail!("the host daemon is not reachable")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket = std::env::var_os("BORANT_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("host.sock"));
    let client = LocalClient::new(socket);
    let mut s = register(&client).await?;
    for _ in 0..60 {
        if s.state == LocalState::Attached {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Outcome::Reply(Ok(v)) = client
            .get::<LocalSession>(&format!("/v1/sessions/{}", s.session), CALL)
            .await
        {
            s = v;
        }
    }
    let agent = s
        .agent
        .clone()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "unknown".into());
    let dir = home().join("fake");
    std::fs::create_dir_all(&dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{agent}.log")))?;
    let mut fake = Fake {
        client,
        session: s.session,
        log,
    };
    let host = s.host.to_string();
    fake.line(&format!(
        "START agent={agent} host={host} session={} pid={}",
        s.session,
        std::process::id()
    ));

    if let Ok(prompt) = std::env::var("BORANT_PROMPT") {
        fake.event(LocalEvent::UserPrompt {
            text: prompt.clone(),
        })
        .await?;
        fake.line(&format!("PROMPT {prompt}"));
        if prompt.contains("You continue Borant agent") {
            fake.history().await?;
        }
        for line in prompt.lines() {
            let _ = fake.run_command(line.trim()).await;
        }
        fake.event(LocalEvent::TurnEnd).await?;
    }

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let path = format!("/v1/sessions/{}/wait?timeout_ms=30000", fake.session);
        let got = tokio::select! {
            _ = term.recv() => {
                fake.line("SIGTERM");
                return Ok(());
            }
            r = fake.client.get::<Value>(&path, Duration::from_secs(40)) => r,
        };
        match got {
            Outcome::Reply(Ok(v)) => {
                let msgs: Vec<Envelope> =
                    serde_json::from_value(v["messages"].clone()).unwrap_or_default();
                if msgs.is_empty() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                if !fake.handle(msgs).await? {
                    return Ok(());
                }
            }
            Outcome::Reply(Err(e)) => {
                fake.line(&format!("WAIT_ERROR {}", e.message));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Outcome::Down(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_takes_an_rfc_3339_time() {
        assert_eq!(iso8601_ms("2026-10-25T12:00:00Z"), Some(1_792_929_600_000));
        assert_eq!(iso8601_ms("2026-10-25T12:00:30Z"), Some(1_792_929_630_000));
        assert_eq!(
            iso8601_ms(" 2026-10-25T12:00:00Z "),
            Some(1_792_929_600_000)
        );
        assert_eq!(iso8601_ms("in two hours"), None);
        assert_eq!(iso8601_ms(""), None);
    }
}
