use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};
use serde_json::Value;

use crate::{check_kind, path_str, run, session_name};

const KIND: &str = "orca";
/// The error code Orca returns for a terminal handle it no longer knows.
const STALE: &str = "terminal_handle_stale";

/// Opens an Orca terminal tab titled `borant-{agent}` that runs the program.
/// The handle ID is Orca's terminal handle (`term_...`).
///
/// It runs `orca terminal create --worktree path:{cwd} --title borant-{agent}
/// --command "exec env K=V ... program..." --json`. Orca types `--command`
/// into the user's login shell, so each word is single-quoted, and `exec`
/// makes the terminal end when the program exits.
///
/// Limits: `cwd` must be in a workspace that Orca knows, or Orca refuses the
/// create. The attach command, `orca terminal switch`, brings the tab to the
/// front of the Orca window; it does not attach the current terminal.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrcaLauncher;

/// Quotes one word for a POSIX shell.
fn sh_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The shell command line that Orca types into the new terminal.
pub fn shell_command(spec: &LaunchSpec) -> String {
    let mut words = vec!["exec".to_owned(), "env".to_owned()];
    // The host's own credentials go before the spec's variables are set.
    words.extend(crate::unset_host_only());
    words.extend(spec.env.iter().map(|(k, v)| sh_quote(&format!("{k}={v}"))));
    words.extend(spec.program.iter().map(|a| sh_quote(a)));
    words.join(" ")
}

/// The `orca` arguments that open the terminal for `spec`.
pub fn create_args(spec: &LaunchSpec) -> PortResult<Vec<String>> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    Ok(vec![
        "terminal".to_owned(),
        "create".to_owned(),
        "--worktree".to_owned(),
        format!("path:{}", path_str(&spec.cwd)?),
        "--title".to_owned(),
        session_name(&spec.agent),
        "--command".to_owned(),
        shell_command(spec),
        "--json".to_owned(),
    ])
}

/// Runs an `orca ... --json` command. Returns `Ok(result)` or
/// `Err((code, message))` from Orca's reply envelope.
async fn orca_json(args: &[String]) -> PortResult<Result<Value, (String, String)>> {
    let out = run("orca", args).await?;
    let reply: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        PortError::other(format!(
            "orca {} returned no JSON ({e}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    })?;
    if reply["ok"].as_bool() == Some(true) {
        return Ok(Ok(reply["result"].clone()));
    }
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    Ok(Err((
        text(&reply["error"]["code"]),
        text(&reply["error"]["message"]),
    )))
}

/// Finds the first `"handle": "term_..."` field in a reply.
fn find_handle(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get("handle")
            .and_then(Value::as_str)
            .filter(|h| h.starts_with("term_"))
            .map(str::to_owned)
            .or_else(|| map.values().find_map(find_handle)),
        Value::Array(items) => items.iter().find_map(find_handle),
        _ => None,
    }
}

fn terminal_args(command: &str, handle: &LaunchHandle) -> Vec<String> {
    vec![
        "terminal".to_owned(),
        command.to_owned(),
        "--terminal".to_owned(),
        handle.id.clone(),
        "--json".to_owned(),
    ]
}

#[async_trait::async_trait]
impl Launcher for OrcaLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let result = orca_json(&create_args(spec)?)
            .await?
            .map_err(|(code, message)| {
                PortError::other(format!("orca terminal create failed: {code}: {message}"))
            })?;
        let id = find_handle(&result)
            .ok_or_else(|| PortError::other("orca terminal create returned no handle"))?;
        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id,
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        check_kind(handle, KIND)?;
        match orca_json(&terminal_args("close", handle)).await? {
            Ok(_) => Ok(()),
            Err((code, _)) if code == STALE => Ok(()),
            Err((code, message)) => Err(PortError::other(format!(
                "orca terminal close failed: {code}: {message}"
            ))),
        }
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        check_kind(handle, KIND)?;
        match orca_json(&terminal_args("show", handle)).await? {
            Ok(result) => Ok(find_handle(&result).is_some()),
            Err((code, _)) if code == STALE => Ok(false),
            Err((code, message)) => Err(PortError::other(format!(
                "orca terminal show failed: {code}: {message}"
            ))),
        }
    }

    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>> {
        Some(vec![
            "orca".to_owned(),
            "terminal".to_owned(),
            "switch".to_owned(),
            "--terminal".to_owned(),
            handle.id.clone(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use borant_core::model::Harness;
    use serde_json::json;

    use super::*;

    #[test]
    fn create_argv() {
        let spec = LaunchSpec {
            agent: "o1".parse().unwrap(),
            harness: Harness::ClaudeCode,
            cwd: PathBuf::from("/work/repo"),
            program: vec![
                "claude".into(),
                "--dangerously-skip-permissions".into(),
                "fix the bug's cause; then $STOP".into(),
            ],
            env: vec![("BORANT_AGENT".into(), "o1".into())],
        };
        assert_eq!(
            create_args(&spec).unwrap(),
            [
                "terminal",
                "create",
                "--worktree",
                "path:/work/repo",
                "--title",
                "borant-o1",
                "--command",
                "exec env -u BORANT_NATS_USER -u BORANT_NATS_PASSWORD -u CREDENTIALS_DIRECTORY \
                 BORANT_AGENT=o1 claude --dangerously-skip-permissions \
                 'fix the bug'\\''s cause; then $STOP'",
                "--json"
            ]
        );
        assert_eq!(sh_quote(""), "''");
        assert_eq!(
            OrcaLauncher
                .attach_command(&LaunchHandle {
                    kind: "orca".into(),
                    id: "term_1".into()
                })
                .unwrap(),
            ["orca", "terminal", "switch", "--terminal", "term_1"]
        );
    }

    #[test]
    fn finds_terminal_handle_in_reply() {
        let reply = json!({"terminal": {"handle": "term_abc", "tabId": "t"}});
        assert_eq!(find_handle(&reply).as_deref(), Some("term_abc"));
        assert_eq!(find_handle(&json!({"handle": "tab_1"})), None);
    }
}
