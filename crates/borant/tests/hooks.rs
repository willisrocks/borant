//! Hooks must never break Claude Code when the daemon is not running.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn daemon_down_exits_zero_quickly() {
    let home = tempfile::tempdir().unwrap();
    for event in [
        "session-start",
        "user-prompt-submit",
        "pre-tool-use",
        "post-tool-use",
        "stop",
        "stop-failure",
        "stop-wait",
        "session-end",
    ] {
        let start = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_borant"))
            .args(["hook", event])
            .env("BORANT_HOME", home.path())
            .env_remove("BORANT_SOCKET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"abc","cwd":"/tmp","tool_name":"Bash"}"#)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{event}");
        assert!(
            out.stdout.is_empty(),
            "{event}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{event} took {:?}",
            start.elapsed()
        );
    }
}

/// Reads one HTTP request from a Unix socket and answers it with an error
/// reply. Returns the body of the request.
fn one_request(listener: std::os::unix::net::UnixListener) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader, Read};

    let (mut stream, _) = listener.accept()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    // The hub is not in this test, so the hook gets an error and exits 0.
    let reply = r#"{"error":{"code":"internal","message":"no hub in the test"}}"#;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        reply.len(),
        reply
    )?;
    stream.flush()?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A chief that a person starts carries its role and collection, so the
/// registration in the host has them.
#[test]
fn hook_role_and_collection_reach_the_registration() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("host.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || one_request(listener));
    let mut child = Command::new(env!("CARGO_BIN_EXE_borant"))
        .args(["hook", "session-start"])
        .env("BORANT_HOME", dir.path())
        .env("BORANT_SOCKET", &socket)
        .env("BORANT_ROLE", "chief")
        .env("BORANT_COLLECTION", "personal")
        .env_remove("BORANT_AGENT")
        .env_remove("BORANT_CHANNEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"abc","cwd":"/tmp"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let body = server.join().unwrap().unwrap();
    assert!(body.contains(r#""role":"chief""#), "{body}");
    assert!(body.contains(r#""collection":"personal""#), "{body}");
}

/// A `StopFailure` of a rate limit becomes one `MemberLimited` event for the
/// session, with the reset time that its text names.
#[test]
fn hook_stop_failure_reports_one_member_limit() {
    let dir = tempfile::tempdir().unwrap();
    // A cached session answers the resolution without a request, so the hook
    // sends exactly one.
    let run = dir.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    let session = borant_core::SessionId::new();
    std::fs::write(run.join("claude-abc.sid"), session.to_string()).unwrap();
    let socket = dir.path().join("host.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || one_request(listener));
    let mut child = Command::new(env!("CARGO_BIN_EXE_borant"))
        .args(["hook", "stop-failure"])
        .env("BORANT_HOME", dir.path())
        .env("BORANT_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"session_id":"abc","error":{"kind":"rate_limit","message":"Usage limit reached. It resets in 2 hours."}}"#,
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let body = server.join().unwrap().unwrap();
    assert!(body.contains(r#""event":"member_limited""#), "{body}");
    // The reset time of the text reached the event, not a missing one.
    assert!(body.contains(r#""resets_at":"#), "{body}");
    assert!(!body.contains(r#""resets_at":null"#), "{body}");
    assert!(body.contains(r#""bundle":null"#), "{body}");
}
