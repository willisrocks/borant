//! `borant doctor` on a temp `BORANT_HOME`: the checks, the exit code, and
//! the JSON shape. The daemon is a fake socket that answers `/v1/health`, the
//! plugins are scripts, and the harness and the sandbox name a program that
//! is not there.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A daemon socket that answers the health call once, as the local API does.
fn fake_daemon(socket: &Path) -> std::thread::JoinHandle<()> {
    if socket.exists() {
        std::fs::remove_file(socket).unwrap();
    }
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let reply = r#"{"ok":{"host":"h1","sessions":0}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            reply.len(),
            reply
        )
        .unwrap();
        stream.flush().unwrap();
    })
}

/// An exec plugin script with this reply.
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

/// A host with a fake daemon, one good plugin of each kind, a missing harness
/// command, and a missing sandbox program. The NATS address is a port that
/// this test holds and nothing answers on, so a real server of the machine or
/// of CI cannot make the check pass.
fn host() -> (tempfile::TempDir, PathBuf, std::net::TcpListener) {
    let nats = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = nats.local_addr().unwrap().port();
    let (dir, socket) = host_at(port);
    (dir, socket, nats)
}

/// The same host, with `[nats] urls` pointing at `port`.
fn host_at(port: u16) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("host.sock");
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
    std::fs::write(
        dir.path().join("host.toml"),
        format!(
            r#"
            [host]
            name = "h1"
            launcher = "process"
            harnesses = ["claude-code"]

            [nats]
            urls = ["nats://127.0.0.1:{port}"]

            [commands]
            claude = ["borant-doctor-no-such-program"]

            [secrets.stores.fake]
            kind = "exec"
            command = ["{}"]

            [notifiers.phone]
            adapter = "exec"
            command = ["{}"]

            [sandbox]
            enabled = true
            program = "borant-doctor-no-such-bwrap"
            "#,
            secrets.display(),
            notifier.display()
        ),
    )
    .unwrap();
    (dir, socket)
}

fn doctor(home: &Path, socket: &Path, json: bool) -> Output {
    doctor_env(home, socket, json, &[])
}

/// The same run, with `extra` in the environment.
fn doctor_env(home: &Path, socket: &Path, json: bool, extra: &[(&str, &str)]) -> Output {
    let mut args = vec!["doctor"];
    if json {
        args.push("--json");
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_borant"));
    command
        .args(args)
        .env("BORANT_HOME", home)
        .env("BORANT_SOCKET", socket)
        .env_remove("BORANT_NATS_URL")
        .env_remove("BORANT_NATS_CREDS")
        .env_remove("BORANT_NATS_USER")
        .env_remove("BORANT_NATS_PASSWORD")
        .env_remove("BORANT_HARNESSES")
        .env_remove("BORANT_LAUNCHER")
        .env_remove("CREDENTIALS_DIRECTORY");
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

/// Every file of the home, for the read-only check.
fn entries(home: &Path) -> BTreeSet<String> {
    std::fs::read_dir(home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// The text run reports each check on one line, and exits 1 because the
/// harness, the sandbox, and NATS fail.
#[test]
fn doctor_prints_each_check_and_exits_one() {
    let (home, socket, _nats) = host();
    let server = fake_daemon(&socket);
    let before = entries(home.path());
    let out = doctor(home.path(), &socket, false);
    server.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "ok   daemon",
        "fail nats",
        "fail harness.claude-code",
        "ok   launcher",
        "ok   disk",
        "fail sandbox",
        "ok   plugin.secrets.fake",
        "ok   plugin.notifier.phone",
    ] {
        assert!(
            stdout.lines().any(|line| line.starts_with(expected)),
            "no line {expected:?} in:\n{stdout}"
        );
    }
    for line in stdout.lines() {
        let level = line.split_whitespace().next().unwrap_or_default();
        assert!(
            matches!(level, "ok" | "warn" | "fail"),
            "line {line:?} does not start with a level"
        );
    }
    assert_eq!(before, entries(home.path()), "doctor wrote in BORANT_HOME");
}

/// `--json` prints the checks with their status and the exit code of the run,
/// and exits 1 when one failed.
#[test]
fn doctor_json_has_the_checks_and_the_exit_code() {
    let (home, socket, _nats) = host();
    let server = fake_daemon(&socket);
    let out = doctor(home.path(), &socket, true);
    server.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    let checks = v["checks"].as_array().expect("a checks array");
    let status = |name: &str| {
        let found: &serde_json::Value = checks
            .iter()
            .find(|c| c["check"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("no check {name:?} in {v}"));
        found["status"].as_str().unwrap_or_default().to_owned()
    };
    for name in ["daemon", "launcher", "disk"] {
        assert_eq!(status(name), "ok", "{name} in {v}");
    }
    for name in ["nats", "harness.claude-code", "sandbox"] {
        assert_eq!(status(name), "fail", "{name} in {v}");
    }
    assert_eq!(status("plugin.secrets.fake"), "ok", "{v}");
    assert_eq!(status("plugin.notifier.phone"), "ok", "{v}");
    for c in checks {
        let detail = c["detail"].as_str().expect("a detail string");
        assert!(!detail.is_empty(), "{c}");
        assert!(!detail.contains('\n'), "{c}");
    }
}

/// A NATS server that wants a login and refuses every one: it sends the
/// `INFO` line with `auth_required`, reads the `CONNECT` and the `PING` of the
/// client, and answers `-ERR 'Authorization Violation'`. The CLI reads no
/// answer from it, so the run must not wait for the request timeout.
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

/// The detail of the one check named `name` of a JSON run.
fn detail_of(v: &serde_json::Value, name: &str) -> String {
    v["checks"]
        .as_array()
        .expect("a checks array")
        .iter()
        .find(|c| c["check"] == serde_json::json!(name))
        .unwrap_or_else(|| panic!("no check {name:?} in {v}"))["detail"]
        .as_str()
        .expect("a detail string")
        .to_owned()
}

/// A shell without NATS credentials against a server that wants a login: the
/// check names the env file of the host daemon at once. It used to report
/// `Timeout: request to borant.ctl timed out`, which hid the cause.
#[test]
fn doctor_names_missing_nats_credentials() {
    let (port, nats) = refusing_nats();
    let (home, socket) = host_at(port);
    let server = fake_daemon(&socket);
    let started = std::time::Instant::now();
    let out = doctor(home.path(), &socket, true);
    server.join().unwrap();
    nats.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let detail = detail_of(&v, "nats");
    assert!(
        detail.starts_with(
            "NATS credentials are missing: load the env file of the host daemon \
             (for example ~/.borant/nats.env, or the EnvironmentFile of the \
             borant-host unit) and run again"
        ),
        "{detail}"
    );
    assert!(!detail.contains("timed out"), "{detail}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the run waited out the network timeout: {:?}",
        started.elapsed()
    );
}

/// Credentials that the server refuses: the check says so, and prints neither
/// the user nor the password.
#[test]
fn doctor_names_refused_nats_credentials() {
    let (port, nats) = refusing_nats();
    let (home, socket) = host_at(port);
    let server = fake_daemon(&socket);
    let out = doctor_env(
        home.path(),
        &socket,
        true,
        &[
            ("BORANT_NATS_USER", "doctor-user"),
            ("BORANT_NATS_PASSWORD", "doctor-password"),
        ],
    );
    server.join().unwrap();
    nats.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let detail = detail_of(&v, "nats");
    assert!(
        detail.starts_with("NATS refused the credentials"),
        "{detail}"
    );
    assert!(!detail.contains("timed out"), "{detail}");
    assert!(!detail.contains("doctor-user"), "{detail}");
    assert!(!detail.contains("doctor-password"), "{detail}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("doctor-password"));
}
