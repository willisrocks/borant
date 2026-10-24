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
