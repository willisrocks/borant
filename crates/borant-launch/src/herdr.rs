//! The herdr launcher (phase 5 spec, "herdr launcher"): one herdr session per
//! host, a workspace per agent, and the harness started with `pane run`, which
//! takes the full argv, so the sandbox and the secret shim work unchanged.
//!
//! Facts behind the mapping: `docs/research/2026-10-25-herdr-facts.md` on
//! branch `research/p5-herdr`. The CLI answers JSON on stdout, errors as
//! `{"error":{"code":...,"message":...}}`, and a call fails with
//! `server_not_running` until a server exists for the session.

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::time::Duration;

use borant_core::AgentId;
use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};
use serde_json::Value;

use crate::{check_kind, path_str, run};

const KIND: &str = "herdr";
/// The longest name that herdr accepts, from its `invalid_agent_name` error.
const NAME_MAX: usize = 32;
/// How long the launcher waits for a server that it just started.
const SERVER_WAIT: Duration = Duration::from_millis(500);

/// The `[herdr]` section of `host.toml`, plus where a laptop reaches the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrConfig {
    /// The herdr session of this host. Its server outlives a host restart, so
    /// running sessions survive a deploy.
    pub session: String,
    /// The herdr program, for example a pinned release binary.
    pub program: String,
    /// The SSH destination of this host, for the attach command of a laptop.
    pub ssh: Option<String>,
}

impl Default for HerdrConfig {
    fn default() -> Self {
        Self {
            session: "borant".to_owned(),
            program: "herdr".to_owned(),
            ssh: None,
        }
    }
}

/// Starts agents in herdr workspaces of one session.
#[derive(Debug, Clone)]
pub struct HerdrLauncher {
    cfg: HerdrConfig,
}

impl HerdrLauncher {
    pub fn new(cfg: HerdrConfig) -> Self {
        Self { cfg }
    }

    /// Runs one herdr call. When no server is running yet, it starts one and
    /// retries the call once.
    async fn call(&self, args: Vec<String>) -> PortResult<Value> {
        let first = self.exec(&args).await?;
        if error_code(&first) != Some("server_not_running") {
            return Ok(first);
        }
        self.start_server().await?;
        tokio::time::sleep(SERVER_WAIT).await;
        let again = self.exec(&args).await?;
        match error_code(&again) {
            Some("server_not_running") => Err(PortError::unavailable(
                "the herdr server did not come up after it was started",
            )),
            _ => Ok(again),
        }
    }

    /// Runs the program and parses its JSON reply.
    async fn exec(&self, args: &[String]) -> PortResult<Value> {
        let out = run(&self.cfg.program, args).await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        match serde_json::from_str::<Value>(stdout.trim()) {
            Ok(v) => Ok(v),
            Err(e) => Err(PortError::other(format!(
                "{} {} answered no JSON ({e}): {}",
                self.cfg.program,
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    /// Starts the session's headless server, detached with `setsid` so that it
    /// outlives the host daemon. Panes inherit its environment, so every
    /// `BORANT_*` variable goes: a harness must not think it is a session.
    async fn start_server(&self) -> PortResult<()> {
        let env = without_borant(std::env::vars_os());
        let mut cmd = Command::new("setsid");
        cmd.arg(&self.cfg.program)
            .args(session_args(&self.cfg))
            .arg("server")
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PortError::unavailable("setsid is not on PATH, so the herdr server cannot start")
            } else {
                PortError::other(format!("cannot start the herdr server: {e}"))
            }
        })?;
        tracing::info!(
            session = %self.cfg.session,
            program = %self.cfg.program,
            "started the herdr server"
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl Launcher for HerdrLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let created = self.call(create_args(&self.cfg, spec)?).await?;
        let workspace = field(&created, "/result/workspace/workspace_id", "a workspace")?;
        let pane = field(&created, "/result/root_pane/pane_id", "a root pane")?;
        self.call(run_args(&self.cfg, pane, spec)?).await?;
        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id: format!("{workspace} {pane}"),
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        check_kind(handle, KIND)?;
        let (workspace, _) = ids(handle)?;
        let mut args = session_args(&self.cfg);
        args.extend([
            "workspace".to_owned(),
            "close".to_owned(),
            workspace.to_owned(),
        ]);
        match self.call(args).await {
            // Stopping a session that is already gone is not a failure.
            Err(e) if e.message.contains("workspace_not_found") => Ok(()),
            Err(e) => Err(e),
            Ok(_) => Ok(()),
        }
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        check_kind(handle, KIND)?;
        let (_, pane) = ids(handle)?;
        let mut args = session_args(&self.cfg);
        args.extend([
            "pane".to_owned(),
            "process-info".to_owned(),
            pane.to_owned(),
        ]);
        match self.call(args).await {
            Ok(v) => Ok(v
                .pointer("/result/process_info/foreground_processes")
                .and_then(Value::as_array)
                .is_some_and(|p| !p.is_empty())),
            // A pane that herdr no longer knows is not running.
            Err(e) if e.message.contains("pane_not_found") => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>> {
        check_kind(handle, KIND).ok()?;
        let mut args = vec![self.cfg.program.clone()];
        if let Some(ssh) = &self.cfg.ssh {
            args.extend(["--remote".to_owned(), ssh.clone()]);
        }
        args.extend(session_args(&self.cfg));
        Some(args)
    }

    fn capture(&self, handle: &LaunchHandle, lines: usize) -> Option<String> {
        check_kind(handle, KIND).ok()?;
        let (_, pane) = ids(handle).ok()?;
        let mut args = session_args(&self.cfg);
        args.extend([
            "pane".to_owned(),
            "read".to_owned(),
            pane.to_owned(),
            "--source".to_owned(),
            "recent".to_owned(),
        ]);
        let out = Command::new(&self.cfg.program)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Some(tail(&text, lines))
    }
}

/// The herdr label of an agent: `b-` plus its lowercase ID, cut to
/// [`NAME_MAX`] characters with a six-character hash suffix when it is
/// longer. herdr accepts `[a-z][a-z0-9_-]{0,31}`.
pub fn label(agent: &AgentId) -> String {
    // An agent ID is already `[a-z0-9_-]`, so the label only needs the `b-`
    // prefix and, when it is too long, a cut with a hash.
    let mut name = format!("b-{agent}");
    if name.len() <= NAME_MAX {
        return name;
    }
    // Room for the prefix, the separator, and the hash.
    let keep = NAME_MAX - 3 - 6;
    name.truncate(2 + keep);
    let cut = name.trim_end_matches(['-', '_']).to_owned();
    format!("{cut}-{}", hash6(agent.as_str()))
}

/// Six lowercase hex characters of an FNV-1a hash, for a shortened name.
fn hash6(value: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in value.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:06x}", h & 0xff_ffff)
}

/// `workspace create` argv: one workspace per agent, with its directory and
/// environment, without taking the focus.
pub fn create_args(cfg: &HerdrConfig, spec: &LaunchSpec) -> PortResult<Vec<String>> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    let mut args = session_args(cfg);
    args.extend([
        "workspace".to_owned(),
        "create".to_owned(),
        "--cwd".to_owned(),
        path_str(&spec.cwd)?.to_owned(),
        "--label".to_owned(),
        label(&spec.agent),
    ]);
    for (k, v) in &spec.env {
        args.extend(["--env".to_owned(), format!("{k}={v}")]);
    }
    args.push("--no-focus".to_owned());
    Ok(args)
}

/// `pane run` argv: the program of the launch, in the root pane of the
/// workspace. `pane run` takes the full argv, unlike `agent start`, which runs
/// the canonical program of a kind.
pub fn run_args(cfg: &HerdrConfig, pane: &str, spec: &LaunchSpec) -> PortResult<Vec<String>> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    let mut args = session_args(cfg);
    args.extend(["pane".to_owned(), "run".to_owned(), pane.to_owned()]);
    args.extend(spec.program.iter().cloned());
    Ok(args)
}

fn session_args(cfg: &HerdrConfig) -> Vec<String> {
    vec!["--session".to_owned(), cfg.session.clone()]
}

/// The environment of a server that the launcher starts: every variable but
/// the `BORANT_*` ones.
fn without_borant(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.filter(|(k, _)| {
        let k = k.to_string_lossy();
        !k.starts_with("BORANT_") && !crate::HOST_ONLY_ENV.contains(&k.as_ref())
    })
    .collect()
}

/// The error code of a herdr reply, when it is an error.
fn error_code(reply: &Value) -> Option<&str> {
    reply.pointer("/error/code").and_then(Value::as_str)
}

/// One string field of a herdr reply.
fn field<'a>(reply: &'a Value, pointer: &str, what: &str) -> PortResult<&'a str> {
    reply
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| PortError::other(format!("the herdr reply has no {what}: {reply}")))
}

/// The workspace and pane of a handle.
fn ids(handle: &LaunchHandle) -> PortResult<(&str, &str)> {
    handle.id.split_once(' ').ok_or_else(|| {
        PortError::other(format!(
            "the herdr handle {:?} is not '<workspace> <pane>'",
            handle.id
        ))
    })
}

/// The last `lines` lines of `text`, without a trailing empty line.
fn tail(text: &str, lines: usize) -> String {
    let mut all: Vec<&str> = text.lines().collect();
    if all.last().is_some_and(|l| l.is_empty()) {
        all.pop();
    }
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use borant_core::model::Harness;

    use super::*;

    fn cfg() -> HerdrConfig {
        HerdrConfig {
            session: "p5".into(),
            program: "herdr".into(),
            ssh: Some("me@vps-1".into()),
        }
    }

    fn spec(agent: &str) -> LaunchSpec {
        LaunchSpec {
            agent: agent.parse().unwrap(),
            harness: Harness::Fake,
            cwd: PathBuf::from("/work/a"),
            program: vec!["bwrap".into(), "--".into(), "claude".into()],
            env: vec![("A".into(), "1".into())],
        }
    }

    #[test]
    fn herdr_labels_are_short_and_lowercase() {
        assert_eq!(label(&"w1".parse().unwrap()), "b-w1");
        assert_eq!(label(&"w_p-1".parse().unwrap()), "b-w_p-1");
        // A name with room for the hash keeps 23 characters and a suffix.
        let long = "a".repeat(40).parse::<AgentId>().unwrap();
        let name = label(&long);
        assert_eq!(name.len(), NAME_MAX, "{name}");
        assert!(name.starts_with("b-aaaa"), "{name}");
        assert!(
            name.ends_with(&format!("-{}", hash6(long.as_str()))),
            "{name}"
        );
        // Two long agents that share a prefix get different labels.
        let other = format!("{}b", "a".repeat(39)).parse::<AgentId>().unwrap();
        assert_ne!(label(&long), label(&other));
    }

    #[test]
    fn herdr_argv_matches_the_mapping() {
        let spec = spec("w1");
        assert_eq!(
            create_args(&cfg(), &spec).unwrap(),
            [
                "--session",
                "p5",
                "workspace",
                "create",
                "--cwd",
                "/work/a",
                "--label",
                "b-w1",
                "--env",
                "A=1",
                "--no-focus"
            ]
        );
        assert_eq!(
            run_args(&cfg(), "w1:p1", &spec).unwrap(),
            [
                "--session",
                "p5",
                "pane",
                "run",
                "w1:p1",
                "bwrap",
                "--",
                "claude"
            ]
        );
    }

    #[test]
    fn herdr_server_env_drops_the_borant_variables() {
        let vars: Vec<(OsString, OsString)> = [
            ("BORANT_AGENT", "w1"),
            ("BORANT_HOME", "/h"),
            ("PATH", "/bin"),
            ("PI_SESSION_FILE", "/s"),
        ]
        .iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)))
        .collect();
        let kept: Vec<String> = without_borant(vars.into_iter())
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, ["PATH", "PI_SESSION_FILE"]);
    }

    #[test]
    fn herdr_capture_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("a", 5), "a");
        assert_eq!(tail("", 3), "");
    }

    #[test]
    fn herdr_attach_command_reaches_the_session() {
        let handle = LaunchHandle {
            kind: KIND.into(),
            id: "w1 w1:p1".into(),
        };
        let launcher = HerdrLauncher::new(cfg());
        assert_eq!(
            launcher.attach_command(&handle),
            Some(
                ["herdr", "--remote", "me@vps-1", "--session", "p5"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        let local = HerdrLauncher::new(HerdrConfig::default());
        assert_eq!(
            local.attach_command(&handle),
            Some(["herdr", "--session", "borant"].map(str::to_owned).to_vec())
        );
    }
}
