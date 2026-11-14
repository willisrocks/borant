//! `borant doctor`: the readiness of this host.
//!
//! The checks are the ones of the phase 10 spec: the local daemon on its
//! socket, NATS with the host's credentials (a `ListHosts` call), each
//! harness command on `PATH`, the launcher program, the disk use of
//! `BORANT_HOME`, the sandbox program when it is enabled, and the `describe`
//! handshake of each exec plugin of `host.toml`. Each check reports `ok`,
//! `warn`, or `fail` with one line of detail, and the exit code is 1 when a
//! check fails.
//!
//! Everything here is read-only: no check writes, starts an agent, or logs in
//! anywhere. The only program that a check starts is a plugin of `host.toml`,
//! for its `describe` handshake.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::{MessageBus, PortErrorKind};
use borant_core::proto::{CtlRequest, Reply, ToolCall};
use borant_core::rules::schedule::DEFAULT_DISK_STOP_PCT;
use borant_core::subjects;
use borant_host::LocalClient;
use borant_host::client::Outcome;
use serde_json::{Value, json};

use crate::config::{self, HostFile};

/// How long one network check waits for its reply.
const NET_TIMEOUT: Duration = Duration::from_secs(3);

/// The env file of the host daemon, in `BORANT_HOME`: the file that the
/// host's unit loads as its `EnvironmentFile` and, on a laptop, the file that
/// the host's own start script loads before `borant host`. It holds the
/// daemon's NATS login.
const ENV_FILE: &str = "nats.env";

/// The line of the NATS check when the server refused an anonymous
/// connection: neither doctor's environment nor the host daemon's env file
/// holds a login.
const MISSING_CREDENTIALS: &str = concat!(
    "NATS credentials are missing: load the env file of the host daemon ",
    "(for example ~/.borant/nats.env, or the EnvironmentFile of the ",
    "borant-host unit) and run again"
);

/// The result of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One check: what it looked at, and the one line of detail.
#[derive(Debug)]
pub struct Check {
    /// The subject of the check, for example `daemon` or `harness.pi`.
    pub name: String,
    pub level: Level,
    pub detail: String,
}

impl Check {
    fn new(name: impl Into<String>, level: Level, detail: impl Into<String>) -> Self {
        let detail: String = detail.into();
        Self {
            name: name.into(),
            level,
            detail: one_line(&detail),
        }
    }

    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, Level::Ok, detail)
    }

    fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, Level::Warn, detail)
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(name, Level::Fail, detail)
    }

    fn to_json(&self) -> Value {
        json!({
            "check": self.name,
            "status": self.level.as_str(),
            "detail": self.detail,
        })
    }
}

/// Runs the checks and prints them, and returns the exit code: 1 when a check
/// failed, else 0.
pub async fn run(json: bool) -> anyhow::Result<i32> {
    report(&checks().await, json)
}

/// Prints the checks, and returns the exit code: 1 when a check failed, else
/// 0.
fn report(checks: &[Check], json: bool) -> anyhow::Result<i32> {
    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    if json {
        let list: Vec<Value> = checks.iter().map(Check::to_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "ok": failed == 0, "checks": list }))?
        );
    } else {
        for c in checks {
            println!("{:<4} {:<22} {}", c.level.as_str(), c.name, c.detail);
        }
    }
    Ok(i32::from(failed > 0))
}

/// Every check of this host. A `host.toml` that does not parse is the one
/// check, because the others read it.
async fn checks() -> Vec<Check> {
    let file = match config::host_file() {
        Ok(file) => file,
        Err(e) => return vec![Check::fail("host.toml", format!("{e:#}"))],
    };
    let home = config::home();
    let mut out = Vec::new();
    out.push(daemon_check(&config::socket()).await);
    let (nats, source) = nats_config(file.nats.to_config("borant-doctor"), &home.join(ENV_FILE));
    out.push(nats_check(nats, source.as_deref(), NET_TIMEOUT).await);
    out.extend(harness_checks(&file));
    out.extend(launcher_checks(&file));
    out.push(disk_check(&home, &file));
    out.extend(sandbox_checks(&file));
    out.extend(plugin_checks(&file).await);
    out
}

/// The local daemon answers on its socket.
async fn daemon_check(socket: &Path) -> Check {
    let client = LocalClient::new(socket.to_path_buf());
    match client.get::<Value>("/v1/health", NET_TIMEOUT).await {
        Outcome::Reply(Ok(v)) => Check::ok(
            "daemon",
            format!(
                "the host daemon answers on {} (host {}, {} sessions)",
                socket.display(),
                v["host"].as_str().unwrap_or("?"),
                v["sessions"]
            ),
        ),
        Outcome::Reply(Err(e)) => Check::fail(
            "daemon",
            format!(
                "the host daemon on {} refused the health call: {}",
                socket.display(),
                e.message
            ),
        ),
        Outcome::Down(e) => Check::fail(
            "daemon",
            format!("no host daemon on {}: {e:#}", socket.display()),
        ),
    }
}

/// NATS is reachable with the host's credentials: a `ListHosts` request to
/// the hub proves the whole path. The caller builds the config, from
/// `[nats]` and the environment, so a test can point it at a closed port
/// without touching `BORANT_NATS_URL`. `source` is the env file that supplied
/// the login when doctor's own environment had none: the line names it, and
/// only it.
///
/// The connection is a single attempt, so a server that refuses the login
/// answers at once with an authorization error. That error means that the
/// server wants credentials, not that it is down, so the check names the
/// missing env file, or the refused login, instead of a request that timed
/// out. No credential of `cfg` goes into the line.
async fn nats_check(
    cfg: borant_nats::NatsConfig,
    source: Option<&Path>,
    timeout: Duration,
) -> Check {
    let urls = if cfg.urls.is_empty() {
        "nats://127.0.0.1:4222".to_owned()
    } else {
        cfg.urls.join(",")
    };
    let from = source
        .map(|p| format!(", credentials from {}", p.display()))
        .unwrap_or_default();
    let nats = match tokio::time::timeout(timeout, borant_nats::connect_once(&cfg)).await {
        Ok(Ok(nats)) => nats,
        Ok(Err(e)) if e.kind == PortErrorKind::Denied => {
            return Check::fail("nats", login_detail(&cfg, &urls, &from));
        }
        Ok(Err(e)) => return Check::fail("nats", format!("{urls}: {e}")),
        Err(_) => {
            return Check::fail(
                "nats",
                format!("{urls}: the connection was not up in {timeout:?}"),
            );
        }
    };
    let req = CtlRequest::Call {
        actor: None,
        key: None,
        call: ToolCall::ListHosts,
    };
    let payload = match serde_json::to_vec(&req) {
        Ok(payload) => payload,
        Err(e) => return Check::fail("nats", format!("the request is not JSON: {e}")),
    };
    let bytes = match nats
        .bus()
        .request(subjects::CTL, payload.into(), timeout)
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => return Check::fail("nats", format!("{urls}: {e}")),
    };
    let reply: Reply = match serde_json::from_slice(&bytes) {
        Ok(reply) => reply,
        Err(e) => return Check::fail("nats", format!("{urls}: the hub reply is not JSON: {e}")),
    };
    match reply.into_result() {
        Ok(hosts) => {
            let count = hosts.as_array().map_or(0, Vec::len);
            Check::ok(
                "nats",
                format!("the hub answered ListHosts over {urls} ({count} hosts){from}"),
            )
        }
        Err(e) => Check::fail(
            "nats",
            format!("the hub answered with an error over {urls}: {}", e.message),
        ),
    }
}

/// The line of a server that refused the login: the credentials of `cfg` are
/// missing when it carries none, and refused when it does. Neither the user
/// nor the password goes into the line; `from` names the env file, by path
/// only.
fn login_detail(cfg: &borant_nats::NatsConfig, urls: &str, from: &str) -> String {
    if has_credentials(cfg) {
        format!("NATS refused the credentials over {urls}{from}")
    } else {
        format!("{MISSING_CREDENTIALS} ({urls})")
    }
}

/// The config of the NATS check, and the env file that supplied its login
/// when doctor's own environment had none.
///
/// A login of doctor's own environment or of `host.toml` wins, so a shell
/// that has one is unafraid of a stale file. Otherwise the file is read as the
/// host daemon reads it, and only the fields that are unset are filled: the
/// user, the password, and the credentials file. A file that is missing, or
/// that holds no login, leaves the config alone, so the check keeps its
/// missing-credentials line. No value of the file leaves this function except
/// into `cfg`.
fn nats_config(
    mut cfg: borant_nats::NatsConfig,
    env_file: &Path,
) -> (borant_nats::NatsConfig, Option<PathBuf>) {
    if has_credentials(&cfg) {
        return (cfg, None);
    }
    let Ok(text) = std::fs::read_to_string(env_file) else {
        return (cfg, None);
    };
    let vars = parse_env_file(&text);
    let value = |name: &str| vars.get(name).filter(|v| !v.is_empty()).cloned();
    if cfg.creds_file.is_none() {
        cfg.creds_file = value("BORANT_NATS_CREDS").map(PathBuf::from);
    }
    if cfg.user.is_none() {
        cfg.user = value("BORANT_NATS_USER");
    }
    if cfg.password.is_none() {
        cfg.password = value("BORANT_NATS_PASSWORD");
    }
    if has_credentials(&cfg) {
        (cfg, Some(env_file.to_path_buf()))
    } else {
        (cfg, None)
    }
}

/// The variables of an env file, as systemd's `EnvironmentFile=` and a shell
/// that sources it consume them: one `KEY=VALUE` per line, a line whose first
/// character is `#` or `;` is a comment, an `export ` prefix is allowed, and
/// one pair of surrounding quotes comes off the value. A line without `=` or
/// without a name is skipped, and nothing is expanded.
fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        vars.insert(name.to_owned(), unquoted(value.trim()).to_owned());
    }
    vars
}

/// A value without the one pair of surrounding quotes that a shell or systemd
/// would remove.
fn unquoted(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Whether `cfg` carries a login: a credentials file, or a user and a
/// password. A user without a password is not a login, because the client
/// sends neither.
fn has_credentials(cfg: &borant_nats::NatsConfig) -> bool {
    cfg.creds_file.is_some() || (cfg.user.is_some() && cfg.password.is_some())
}

/// Each harness command of `host.toml` resolves on `PATH`.
fn harness_checks(file: &HostFile) -> Vec<Check> {
    harnesses(file)
        .into_iter()
        .map(|harness| {
            let argv = borant_launch::harness::command(harness, "", None, &file.commands, false);
            let program = argv.first().cloned().unwrap_or_default();
            let name = format!("harness.{harness}");
            match program_on_path(&program) {
                Some(path) => Check::ok(name, format!("{program} at {}", path.display())),
                None => Check::fail(name, format!("{program} is not on PATH")),
            }
        })
        .collect()
}

/// The harnesses that a host start would offer: `BORANT_HARNESSES`, then
/// `[host].harnesses`. A file that names neither detects the harnesses on
/// `PATH`, so the only commands to check are the `[commands]` overrides.
fn harnesses(file: &HostFile) -> Vec<Harness> {
    if let Some(v) = env("BORANT_HARNESSES") {
        return v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    }
    if let Some(list) = &file.host.harnesses {
        return list.clone();
    }
    let c = &file.commands;
    [
        (Harness::ClaudeCode, &c.claude),
        (Harness::Pi, &c.pi),
        (Harness::Omp, &c.omp),
        (Harness::Codex, &c.codex),
        (Harness::Fake, &c.fake),
    ]
    .into_iter()
    .filter(|(_, program)| program.is_some())
    .map(|(harness, _)| harness)
    .collect()
}

/// The launcher program exists, or the launcher is this binary.
fn launcher_checks(file: &HostFile) -> Vec<Check> {
    let launcher = env("BORANT_LAUNCHER")
        .or_else(|| file.host.launcher.clone())
        .unwrap_or_else(|| borant_launch::DEFAULT_LAUNCHER.to_owned());
    let program = match borant_launch::launcher_program(&launcher, &file.herdr_config()) {
        Ok(program) => program,
        Err(e) => return vec![Check::fail("launcher", e)],
    };
    match program {
        None => vec![Check::ok(
            "launcher",
            format!("the {launcher} launcher is this binary"),
        )],
        Some(program) => match program_on_path(&program) {
            Some(path) => vec![Check::ok(
                "launcher",
                format!("{launcher}: {program} at {}", path.display()),
            )],
            None => vec![Check::fail(
                "launcher",
                format!("{launcher}: {program} is not on PATH"),
            )],
        },
    }
}

/// The disk of `BORANT_HOME`: a warn at the alert line, a fail at the
/// placement stop line.
fn disk_check(home: &Path, file: &HostFile) -> Check {
    let warn_pct = file.alerts.to_config().disk_warn_pct;
    let Some(used) = borant_host::disk_used_pct(home) else {
        return Check::ok("disk", format!("the disk of {} is unknown", home.display()));
    };
    let detail = format!(
        "{} is {used:.0}% full (warn {warn_pct}%, fail {DEFAULT_DISK_STOP_PCT}%)",
        home.display()
    );
    match disk_level(used, warn_pct) {
        Level::Fail => Check::fail("disk", detail),
        Level::Warn => Check::warn("disk", detail),
        Level::Ok => Check::ok("disk", detail),
    }
}

/// The disk guard's two lines: a warn at `warn_pct`, a fail at the placement
/// stop point.
fn disk_level(used: f64, warn_pct: u8) -> Level {
    if used >= f64::from(DEFAULT_DISK_STOP_PCT) {
        Level::Fail
    } else if used >= f64::from(warn_pct) {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// The sandbox program, when `[sandbox] enabled`.
fn sandbox_checks(file: &HostFile) -> Vec<Check> {
    if !file.sandbox.enabled.unwrap_or(false) {
        return Vec::new();
    }
    let program = file
        .sandbox
        .program
        .clone()
        .unwrap_or_else(|| "bwrap".to_owned());
    match program_on_path(&program) {
        Some(path) => vec![Check::ok(
            "sandbox",
            format!("{program} at {}", path.display()),
        )],
        None => vec![Check::fail("sandbox", format!("{program} is not on PATH"))],
    }
}

/// The `describe` handshake of each exec plugin of `host.toml`, with the kind
/// that its port expects: a secrets store answers as `secrets` and offers
/// `resolve`, a notifier answers as `notifier`.
async fn plugin_checks(file: &HostFile) -> Vec<Check> {
    let credentials = env("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(secrets) = &file.secrets {
        for (name, store) in &secrets.stores {
            if !matches!(store, borant_secrets::StoreConfig::Exec { .. }) {
                continue;
            }
            let name = format!("plugin.secrets.{name}");
            let check = match borant_secrets::open_store(store, credentials.as_deref()).await {
                Ok(_) => Check::ok(name, "the store answers describe as a secrets plugin"),
                Err(e) => Check::fail(name, e.to_string()),
            };
            out.push(check);
        }
    }
    for (name, notifier) in &file.notifiers {
        let name = format!("plugin.notifier.{name}");
        let check = match config::notifier_exec(notifier) {
            Err(e) => Check::fail(name, format!("{e:#}")),
            Ok(exec) => match borant_exec::describe(&exec, "notifier").await {
                Ok(_) => Check::ok(name, "the notifier answers describe as a notifier plugin"),
                Err(e) => Check::fail(name, e.to_string()),
            },
        };
        out.push(check);
    }
    out
}

/// The first file of `program` on `PATH`, or the path itself when it has a
/// slash. A missing `PATH` resolves nothing.
fn program_on_path(program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = Path::new(program);
        return path.is_file().then(|| path.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The first line of `text`, so a check stays one line.
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn file(text: &str) -> HostFile {
        toml::from_str(text).unwrap_or_else(|e| panic!("parse {text:?}: {e}"))
    }

    /// A program of the exec plugin protocol with this reply.
    fn plugin(dir: &Path, name: &str, reply: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncat > /dev/null\necho '{reply}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A spawn of a harness whose `[commands]` name is not a program is a
    /// fail, and the line names the program.
    #[test]
    fn harness_a_missing_command_fails() {
        let f = file(
            r#"
            [host]
            harnesses = ["claude-code"]
            [commands]
            claude = ["borant-doctor-no-such-program"]
            "#,
        );
        let checks = harness_checks(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "harness.claude-code");
        assert_eq!(checks[0].level, Level::Fail);
        assert!(
            checks[0].detail.contains("borant-doctor-no-such-program"),
            "{}",
            checks[0].detail
        );
    }

    /// A harness command that resolves is an ok line with its path.
    #[test]
    fn harness_a_command_on_path_passes() {
        let f = file(
            r#"
            [host]
            harnesses = ["pi"]
            [commands]
            pi = ["sh"]
            "#,
        );
        let checks = harness_checks(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].level, Level::Ok);
        assert!(
            checks[0].detail.starts_with("sh at /"),
            "{}",
            checks[0].detail
        );
    }

    /// Without `[host].harnesses` the host detects the harnesses, so only the
    /// `[commands]` overrides are checked.
    #[test]
    fn harness_without_a_list_checks_the_overrides() {
        let f = file(
            r#"
            [commands]
            omp = ["borant-doctor-no-such-program"]
            "#,
        );
        let checks = harness_checks(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "harness.omp");
        assert_eq!(checks[0].level, Level::Fail);
        assert!(harness_checks(&file("")).is_empty());
    }

    /// The disk guard's lines: a warn at 85, a fail at the placement stop.
    #[test]
    fn disk_levels_follow_the_guard() {
        assert_eq!(disk_level(10.0, 85), Level::Ok);
        assert_eq!(disk_level(84.9, 85), Level::Ok);
        assert_eq!(disk_level(85.0, 85), Level::Warn);
        assert_eq!(disk_level(94.9, 85), Level::Warn);
        assert_eq!(disk_level(95.0, 85), Level::Fail);
        assert_eq!(disk_level(99.0, 95), Level::Fail);
        assert_eq!(disk_level(90.0, 95), Level::Ok, "a raised warn line");
        // A real temp dir is below the warn line of any disk.
        let dir = tempfile::tempdir().unwrap();
        let check = disk_check(dir.path(), &file(""));
        assert_eq!(check.level, Level::Ok, "{}", check.detail);
    }

    /// The process launcher is this binary; a herdr program that is not on
    /// PATH is a fail.
    #[test]
    fn launcher_a_missing_program_fails() {
        let f = file(
            r#"
            [host]
            launcher = "herdr"
            [herdr]
            program = "borant-doctor-no-such-herdr"
            "#,
        );
        let checks = launcher_checks(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].level, Level::Fail);
        assert!(
            checks[0].detail.contains("borant-doctor-no-such-herdr"),
            "{}",
            checks[0].detail
        );

        let f = file("[host]\nlauncher = \"process\"\n");
        let checks = launcher_checks(&f);
        assert_eq!(checks[0].level, Level::Ok);
        assert!(
            checks[0].detail.contains("is this binary"),
            "{}",
            checks[0].detail
        );

        let f = file("[host]\nlauncher = \"nope\"\n");
        let checks = launcher_checks(&f);
        assert_eq!(checks[0].level, Level::Fail);
        assert!(
            checks[0].detail.contains("unknown launcher"),
            "{}",
            checks[0].detail
        );
    }

    /// The sandbox program is checked only when `[sandbox] enabled`.
    #[test]
    fn sandbox_program_only_when_enabled() {
        assert!(sandbox_checks(&file("")).is_empty());
        let f = file("[sandbox]\nenabled = true\nprogram = \"borant-doctor-no-bwrap\"\n");
        let checks = sandbox_checks(&f);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].level, Level::Fail);
        let f = file("[sandbox]\nenabled = true\nprogram = \"sh\"\n");
        assert_eq!(sandbox_checks(&f)[0].level, Level::Ok);
    }

    /// A secrets plugin that answers as `secrets` with resolve passes; a
    /// notifier that answers as `notifier` passes; a plugin of the other kind
    /// is a fail.
    #[tokio::test]
    async fn plugins_answer_the_kind_of_their_port() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = plugin(
            dir.path(),
            "secrets.sh",
            r#"{"ok": {"v": 1, "kind": "secrets", "resolve": true}}"#,
        );
        let notifier = plugin(
            dir.path(),
            "notifier.sh",
            r#"{"ok": {"v": 1, "kind": "notifier"}}"#,
        );
        let f = file(&format!(
            r#"
            [secrets.stores.fake]
            kind = "exec"
            command = ["{}"]
            [notifiers.phone]
            adapter = "exec"
            command = ["{}"]
            "#,
            secrets.display(),
            notifier.display()
        ));
        let checks = plugin_checks(&f).await;
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "plugin.secrets.fake");
        assert_eq!(checks[0].level, Level::Ok, "{}", checks[0].detail);
        assert_eq!(checks[1].name, "plugin.notifier.phone");
        assert_eq!(checks[1].level, Level::Ok, "{}", checks[1].detail);

        // The kinds do not match the ports.
        let f = file(&format!(
            r#"
            [secrets.stores.fake]
            kind = "exec"
            command = ["{}"]
            [notifiers.phone]
            adapter = "exec"
            command = ["{}"]
            "#,
            notifier.display(),
            secrets.display()
        ));
        let checks = plugin_checks(&f).await;
        assert_eq!(checks[0].level, Level::Fail);
        assert_eq!(checks[1].level, Level::Fail);

        // A program that is not there.
        let f = file(
            r#"
            [secrets.stores.fake]
            kind = "exec"
            command = ["borant-doctor-no-such-plugin"]
            [notifiers.phone]
            adapter = "exec"
            command = ["borant-doctor-no-such-plugin"]
            "#,
        );
        let checks = plugin_checks(&f).await;
        assert_eq!(checks[0].level, Level::Fail);
        assert_eq!(checks[1].level, Level::Fail);
        assert!(
            checks[0].detail.contains("borant-doctor-no-such-plugin"),
            "{}",
            checks[0].detail
        );
    }

    /// A daemon that is not there is a fail that names the socket.
    #[tokio::test]
    async fn daemon_without_a_socket_fails() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host.sock");
        let check = daemon_check(&socket).await;
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains("host.sock"), "{}", check.detail);
    }

    /// A NATS server that wants a login and refuses every one: it sends the
    /// `INFO` line with `auth_required`, reads the `CONNECT` and the `PING` of
    /// the client, and answers `-ERR 'Authorization Violation'`.
    fn refusing_nats() -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"INFO {\"server_id\":\"doctor\",\"auth_required\":true}\r\n")
                .unwrap();
            stream.flush().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.starts_with("PING") {
                    break;
                }
            }
            stream
                .write_all(b"-ERR 'Authorization Violation'\r\n")
                .unwrap();
            stream.flush().unwrap();
        });
        (port, server)
    }

    /// A config of `port`, with no credentials.
    fn nats_cfg(port: u16) -> borant_nats::NatsConfig {
        borant_nats::NatsConfig {
            urls: vec![format!("nats://127.0.0.1:{port}")],
            ..borant_nats::NatsConfig::default()
        }
    }

    /// A server that wants a login refuses the anonymous connection at once:
    /// the check names the missing env file, and does not wait out the
    /// timeout that hid the cause.
    #[tokio::test]
    async fn nats_without_credentials_names_the_env_file() {
        let (port, server) = refusing_nats();
        let started = std::time::Instant::now();
        let check = nats_check(nats_cfg(port), None, Duration::from_secs(3)).await;
        server.join().unwrap();
        assert_eq!(check.level, Level::Fail);
        assert!(
            check
                .detail
                .starts_with("NATS credentials are missing: load the env file"),
            "{}",
            check.detail
        );
        assert!(
            check.detail.contains("~/.borant/nats.env"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("timed out"), "{}", check.detail);
        assert!(check.detail.contains(&port.to_string()), "{}", check.detail);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the check waited out the timeout: {:?}",
            started.elapsed()
        );
    }

    /// A login that the server refuses is a fail, and neither the user nor the
    /// password goes into the line.
    #[tokio::test]
    async fn nats_with_refused_credentials_says_so() {
        let (port, server) = refusing_nats();
        let cfg = borant_nats::NatsConfig {
            user: Some("u-doctor".to_owned()),
            password: Some("p-doctor".to_owned()),
            ..nats_cfg(port)
        };
        let check = nats_check(cfg, None, Duration::from_secs(3)).await;
        server.join().unwrap();
        assert_eq!(check.level, Level::Fail);
        assert!(
            check.detail.starts_with("NATS refused the credentials"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("u-doctor"), "{}", check.detail);
        assert!(!check.detail.contains("p-doctor"), "{}", check.detail);
    }

    /// A NATS server that does not answer is a fail that names the URL, even
    /// on a machine that runs one: the port is bound here, so nothing listens
    /// on it, and `BORANT_NATS_URL` plays no part.
    #[tokio::test]
    async fn nats_without_a_server_fails() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let check = nats_check(nats_cfg(port), None, Duration::from_millis(100)).await;
        assert_eq!(check.level, Level::Fail);
        assert!(check.detail.contains(&port.to_string()), "{}", check.detail);
    }

    /// An env file as the host's start script writes it: comments, a blank
    /// line, an `export` prefix, one pair of quotes, an empty value, and a
    /// line that is not KEY=VALUE.
    #[test]
    fn env_file_is_parsed_as_the_daemon_reads_it() {
        let vars = parse_env_file(
            "# the login of this host\n\
             \n\
             BORANT_NATS_USER=host-user\n\
             export BORANT_NATS_PASSWORD=\"host-password\"\n\
             ; another comment\n\
             BORANT_NATS_URL='nats://example:4222'\n\
             not a pair\n\
             = empty name\n\
             EMPTY=\n",
        );
        let get = |name: &str| vars.get(name).map(String::as_str);
        assert_eq!(get("BORANT_NATS_USER"), Some("host-user"));
        assert_eq!(get("BORANT_NATS_PASSWORD"), Some("host-password"));
        assert_eq!(get("BORANT_NATS_URL"), Some("nats://example:4222"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(vars.len(), 4, "{vars:?}");
        assert_eq!(unquoted("plain"), "plain");
        assert_eq!(unquoted("\"one"), "\"one", "an unpaired quote stays");
        assert_eq!(unquoted("\"one'"), "\"one'", "mismatched quotes stay");
    }

    /// The env file of the host daemon supplies the login when doctor's own
    /// config has none, and the source names the file, by path only.
    #[test]
    fn env_file_credentials_fill_a_config_without_a_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nats.env");
        std::fs::write(
            &path,
            "BORANT_NATS_USER=host-user\nBORANT_NATS_PASSWORD=host-secret\n",
        )
        .unwrap();
        let (cfg, source) = nats_config(nats_cfg(4222), &path);
        assert_eq!(cfg.user.as_deref(), Some("host-user"));
        assert_eq!(cfg.password.as_deref(), Some("host-secret"));
        assert!(has_credentials(&cfg));
        assert_eq!(source.as_deref(), Some(path.as_path()));
        let detail = login_detail(&cfg, "nats://example:4222", ", credentials from ");
        assert!(!detail.contains("host-user"), "{detail}");
        assert!(!detail.contains("host-secret"), "{detail}");
    }

    /// A login of doctor's own environment wins, and a file that is not there
    /// or holds no login changes nothing.
    #[test]
    fn own_credentials_win_and_a_file_without_a_login_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nats.env");
        std::fs::write(
            &path,
            "BORANT_NATS_USER=host-user\nBORANT_NATS_PASSWORD=host-secret\n",
        )
        .unwrap();
        let own = borant_nats::NatsConfig {
            user: Some("shell-user".to_owned()),
            password: Some("shell-secret".to_owned()),
            ..nats_cfg(4222)
        };
        let (cfg, source) = nats_config(own, &path);
        assert_eq!(cfg.user.as_deref(), Some("shell-user"));
        assert_eq!(cfg.password.as_deref(), Some("shell-secret"));
        assert_eq!(source, None, "the file did not supply the login");

        let missing = dir.path().join("absent.env");
        let (cfg, source) = nats_config(nats_cfg(4222), &missing);
        assert!(!has_credentials(&cfg));
        assert_eq!(source, None);

        std::fs::write(&path, "# no login here\nBORANT_POOL=box\n").unwrap();
        let (cfg, source) = nats_config(nats_cfg(4222), &path);
        assert!(!has_credentials(&cfg));
        assert_eq!(source, None);

        // A user without a password is not a login, and no source is named.
        std::fs::write(&path, "BORANT_NATS_USER=host-user\n").unwrap();
        let (cfg, source) = nats_config(nats_cfg(4222), &path);
        assert_eq!(cfg.password, None);
        assert!(!has_credentials(&cfg));
        assert_eq!(source, None);
    }

    /// A detail is one line, whatever the error held.
    #[test]
    fn details_stay_one_line() {
        let check = Check::fail("x", "one\ntwo\r\n  three");
        assert_eq!(check.detail, "one");
        assert_eq!(Check::fail("x", "\n\n  first  \nlater").detail, "first");
    }

    /// The exit code is 1 only when a check failed: a warn is not a failure.
    #[test]
    fn the_exit_code_is_one_only_when_a_check_fails() {
        let all_ok = [Check::ok("a", "fine"), Check::warn("b", "nearly")];
        assert_eq!(report(&all_ok, false).unwrap(), 0);
        assert_eq!(report(&all_ok, true).unwrap(), 0);
        let one_fail = [Check::ok("a", "fine"), Check::fail("b", "broken")];
        assert_eq!(report(&one_fail, false).unwrap(), 1);
        assert_eq!(report(&one_fail, true).unwrap(), 1);
    }
}
