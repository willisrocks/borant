//! The runner of the exec plugin protocol: the one way that Borant starts an
//! outside program for a port (spec "Exec plugin protocol" in
//! docs/superpowers/specs/2026-10-24-elastic-compute-design.md). The caller
//! starts the program once for each operation, writes one JSON request to
//! stdin, and reads one JSON reply from stdout.
//!
//! The reply of a plugin can hold secret values, so nothing from stdout goes
//! into a log line or an error message. The caller gets the `ok` value itself.

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
    let reply = match tokio::time::timeout(cfg.timeout, run(cfg, &program, &bytes)).await {
        Ok(reply) => reply?,
        Err(_) => {
            return Err(PortError::new(
                PortErrorKind::Timeout,
                format!("{program} {op} timed out after {:?}", cfg.timeout),
            ));
        }
    };
    parse(&program, op, &reply)
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
/// stdout. Its stderr goes to `debug`: the program uses it for logs only.
async fn run(cfg: &ExecConfig, program: &str, request: &[u8]) -> PortResult<Vec<u8>> {
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
    let stderr = String::from_utf8_lossy(&stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        tracing::debug!(%status, "{program} stderr: {stderr}");
    }
    if !status.success() && stdout.is_empty() {
        return Err(PortError::other(format!(
            "{program} exited with {status} and wrote no reply"
        )));
    }
    Ok(stdout)
}

/// Reads the reply. Nothing of it goes into an error message, because the
/// value of a secrets plugin holds values and the message of an error reply
/// can hold any text.
fn parse(program: &str, op: &str, stdout: &[u8]) -> PortResult<Value> {
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
        Some(error) => Err(PortError::new(
            kind_of(error.get("code").and_then(Value::as_str)),
            format!("{program} {op} failed"),
        )),
        None => Err(PortError::other(format!(
            "{program} {op} wrote a reply with neither ok nor error"
        ))),
    }
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
