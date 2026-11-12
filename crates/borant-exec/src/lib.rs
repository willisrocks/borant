//! The runner of the exec plugin protocol: the one way that Borant starts an
//! outside program for a port (spec "Exec plugin protocol" in
//! docs/superpowers/specs/2026-10-24-elastic-compute-design.md). The caller
//! starts the program once for each operation, writes one JSON request to
//! stdin, and reads one JSON reply from stdout.
//!
//! The reply of a plugin can hold secret values, so nothing from stdout goes
//! into a log line or an error message. The caller gets the `ok` value itself.

pub mod changes;
pub mod issues;
pub mod notify;
pub mod provision;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use borant_core::ports::{PortError, PortErrorKind, PortResult};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The protocol version of this caller.
pub const VERSION: u64 = 1;

/// How the runner starts an exec plugin, from its configuration.
#[derive(Debug, Clone)]
pub struct ExecConfig {
    /// The program and its arguments, argv, without secret values.
    pub command: Vec<String>,
    /// The environment of the program, on top of an empty environment.
    pub env: BTreeMap<String, String>,
    /// The folder of the systemd credentials of the caller unit and the file
    /// name in it, if the plugin reads its own credential from a file.
    pub credential: Option<(PathBuf, String)>,
    /// The limit for one operation, including the start of the program.
    pub timeout: Duration,
    /// Whether the runner logs the text of the stderr of the program. False
    /// for a plugin that can print a secret value: the runner cannot redact
    /// what it does not know, so it logs only the size and the status then.
    pub log_stderr: bool,
}

/// Runs one operation of the plugin and returns the `ok` value of its reply.
/// `args` is merged into the request, after `v` and `op` win over its keys.
pub async fn call(cfg: &ExecConfig, op: &str, args: Value) -> PortResult<Value> {
    let program = cfg
        .command
        .first()
        .ok_or_else(|| PortError::invalid("the exec configuration has no command"))?
        .clone();
    let request = request(op, args)?;
    let bytes = serde_json::to_vec(&request)
        .map_err(|e| PortError::invalid(format!("the request of {op} is not JSON: {e}")))?;
    let output = match tokio::time::timeout(cfg.timeout, run(cfg, &program, &bytes)).await {
        Ok(output) => output?,
        Err(_) => {
            return Err(PortError::new(
                PortErrorKind::Timeout,
                format!("{program} {op} timed out after {:?}", cfg.timeout),
            ));
        }
    };
    let result = if !output.status.success() && output.stdout.is_empty() {
        Err(PortError::other(format!(
            "{program} exited with {} and wrote no reply",
            output.status
        )))
    } else {
        parse(&program, op, &output.stdout, cfg.log_stderr)
    };
    if result.is_err() || !output.status.success() {
        warn_stderr(cfg, &program, op, &output);
    }
    result
}

/// The most bytes of stderr that a failed call puts in a warning.
const STDERR_WARN_LIMIT: usize = 1024;

/// Logs the stderr of a failed call at `warn`, trimmed, for a plugin whose
/// stderr is logs. A plugin that can print a secret keeps it out of the log.
fn warn_stderr(cfg: &ExecConfig, program: &str, op: &str, output: &Output) {
    if !cfg.log_stderr || output.stderr.is_empty() {
        return;
    }
    let mut end = output.stderr.len().min(STDERR_WARN_LIMIT);
    while !output.stderr.is_char_boundary(end) {
        end -= 1;
    }
    let cut = if end < output.stderr.len() { "…" } else { "" };
    tracing::warn!(status = %output.status, "{program} {op} failed, stderr: {}{cut}", &output.stderr[..end]);
}

/// What a finished program left: its stdout, its trimmed stderr, its status.
struct Output {
    stdout: Vec<u8>,
    stderr: String,
    status: std::process::ExitStatus,
}

/// Runs the `describe` handshake of a plugin of `kind` and returns its value.
/// The plugin must speak protocol version 1 and be a plugin of `kind`.
pub async fn describe(cfg: &ExecConfig, kind: &str) -> PortResult<Value> {
    let reply = call(cfg, "describe", Value::Object(Map::new())).await?;
    let version = reply.get("v").and_then(Value::as_u64);
    let got = reply.get("kind").and_then(Value::as_str);
    if version != Some(VERSION) || got != Some(kind) {
        return Err(PortError::invalid(format!(
            "the program is not a {kind} plugin of protocol version {VERSION}"
        )));
    }
    Ok(reply)
}

/// The request object: `{"v": 1, "op": op, ...args}`. The version and the
/// operation win over the same keys of `args`.
fn request(op: &str, args: Value) -> PortResult<Value> {
    let mut map = match args {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        _ => {
            return Err(PortError::invalid(
                "the arguments of an exec operation are a JSON object",
            ));
        }
    };
    map.insert("v".to_owned(), Value::from(VERSION));
    map.insert("op".to_owned(), Value::from(op));
    Ok(Value::Object(map))
}

/// Starts the program with its environment and its request, and returns its
/// stdout. Its stderr goes to `debug` as text when `cfg.log_stderr`, else as a
/// size and the status: a plugin that prints a secret value keeps it out of
/// the log.
async fn run(cfg: &ExecConfig, program: &str, request: &[u8]) -> PortResult<Output> {
    let mut cmd = Command::new(program);
    cmd.args(&cfg.command[1..]);
    cmd.env_clear();
    for key in ["PATH", "HOME"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    for (key, value) in &cfg.env {
        cmd.env(key, value);
    }
    if let Some((dir, name)) = &cfg.credential {
        cmd.env("BORANT_CREDENTIALS_DIRECTORY", dir);
        cmd.env("BORANT_CREDENTIAL", name);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A timeout drops this future, and with it the child.
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| PortError::invalid(format!("cannot start {program}: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| PortError::other("the program has no stdin"))?;
    if let Err(e) = stdin.write_all(request).await {
        // A program that never reads its request still sends a reply.
        tracing::debug!("{program}: cannot write the request: {e}");
    }
    drop(stdin);
    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| PortError::other("the program has no stdout"))?;
    let mut err = child
        .stderr
        .take()
        .ok_or_else(|| PortError::other("the program has no stderr"))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let (read_out, read_err, status) = tokio::join!(
        out.read_to_end(&mut stdout),
        err.read_to_end(&mut stderr),
        child.wait(),
    );
    read_out.map_err(|e| PortError::other(format!("cannot read the stdout of {program}: {e}")))?;
    read_err.map_err(|e| PortError::other(format!("cannot read the stderr of {program}: {e}")))?;
    let status = status.map_err(|e| PortError::other(format!("cannot wait for {program}: {e}")))?;
    let byte_count = stderr.len();
    let stderr = String::from_utf8_lossy(&stderr);
    let note = stderr.trim().to_owned();
    if !note.is_empty() {
        if cfg.log_stderr {
            tracing::debug!(%status, "{program} stderr: {note}");
        } else {
            tracing::debug!(%status, bytes = byte_count, "{program} wrote {byte_count} bytes to stderr");
        }
    }
    Ok(Output {
        stdout,
        stderr: note,
        status,
    })
}

/// Reads the reply. Nothing of `ok` goes into an error message, because the
/// value of a secrets plugin holds values. An error reply gives its `code`
/// when that is a short machine code, and its `message` only when
/// `with_message` is set: the message of a secrets plugin can hold values.
fn parse(program: &str, op: &str, stdout: &[u8], with_message: bool) -> PortResult<Value> {
    let text = String::from_utf8_lossy(stdout);
    let reply: Value = match serde_json::from_str(text.trim()) {
        Ok(reply) => reply,
        Err(_) => {
            return Err(PortError::other(format!(
                "{program} {op} wrote a reply that is not JSON"
            )));
        }
    };
    let Some(reply) = reply.as_object() else {
        return Err(PortError::other(format!(
            "{program} {op} wrote a reply that is not a JSON object"
        )));
    };
    if let Some(ok) = reply.get("ok") {
        return Ok(ok.clone());
    }
    match reply.get("error") {
        Some(error) => {
            let code = error.get("code").and_then(Value::as_str);
            let mut text = format!("{program} {op} failed");
            if let Some(code) = code.filter(|c| is_machine_code(c)) {
                text.push_str(&format!(": {code}"));
            }
            if with_message
                && let Some(message) = error.get("message").and_then(Value::as_str)
                && !message.trim().is_empty()
            {
                let message: String = message.trim().chars().take(300).collect();
                text.push_str(&format!(": {message}"));
            }
            Err(PortError::new(kind_of(code), text))
        }
        None => Err(PortError::other(format!(
            "{program} {op} wrote a reply with neither ok nor error"
        ))),
    }
}

/// Whether `code` is a short machine code: letters, digits, `_`, `-`, `.`.
fn is_machine_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The kind of an error code of the protocol. An unknown code is `Other`.
fn kind_of(code: Option<&str>) -> PortErrorKind {
    match code {
        Some("unavailable") => PortErrorKind::Unavailable,
        Some("not_found") => PortErrorKind::NotFound,
        Some("invalid") => PortErrorKind::Invalid,
        Some("denied") => PortErrorKind::Denied,
        _ => PortErrorKind::Other,
    }
}
