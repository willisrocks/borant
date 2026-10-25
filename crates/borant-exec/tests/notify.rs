//! The exec notifier: the request shape of op `notify`, and the kind of the
//! errors of its replies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use borant_core::ports::PortErrorKind;
use borant_core::ports::{Notice, Notifier, Priority};
use borant_exec::notify::ExecNotifier;
use borant_exec::ExecConfig;
use serde_json::{Value, json};

/// The path of a fixture plugin.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A notifier whose plugin is `name`, with `args` after the program.
fn notifier(name: &str, args: &[&str]) -> ExecNotifier {
    let mut command = vec![fixture(name).display().to_string()];
    command.extend(args.iter().map(|a| a.to_string()));
    ExecNotifier::new(ExecConfig {
        command,
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(10),
        // A notice is not a secret, but the plugin is the only one who knows
        // what it printed, so the runner keeps its stderr out of the log.
        log_stderr: false,
    })
}

#[tokio::test]
async fn notify_sends_the_notice_under_the_notice_key() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let seen = dir.path().join("request.json");
    let n = notifier("echo_request.sh", &[&seen.display().to_string()]);
    let notice = Notice::new("w3 needs you", "task 4 is stuck").with_priority(Priority::Urgent);
    let notice = notice
        .with_tags(["api".to_owned(), "needs_human".to_owned()])
        .with_link("https://borant.example/view");
    n.notify(&notice).await.expect("the notify");

    let text = std::fs::read_to_string(&seen).expect("the request");
    let sent: Value = serde_json::from_str(&text).expect("the request is JSON");
    assert_eq!(sent["v"], 1);
    assert_eq!(sent["op"], "notify");
    assert_eq!(sent["notice"]["title"], "w3 needs you");
    assert_eq!(sent["notice"]["body"], "task 4 is stuck");
    // A lowercase string, as the plugins expect.
    assert_eq!(sent["notice"]["priority"], "urgent");
    assert_eq!(sent["notice"]["tags"], json!(["api", "needs_human"]));
    assert_eq!(sent["notice"]["link"], "https://borant.example/view");
    // Nothing of the notice at the top level of the request.
    assert!(sent.get("title").is_none(), "{sent}");
}

#[tokio::test]
async fn a_notify_error_reply_keeps_its_kind() {
    let n = notifier("error.sh", &["denied"]);
    let err = n
        .notify(&Notice::new("t", "b"))
        .await
        .expect_err("an error reply");
    assert_eq!(err.kind, PortErrorKind::Denied, "{err}");
    // The message of the reply is not the error message of the caller.
    assert!(!err.message.contains("marker-value"), "{err}");
}

#[tokio::test]
async fn notify_describe_is_the_handshake() {
    notifier("describe_notifier.sh", &[])
        .check()
        .await
        .expect("a notifier plugin");
    let other = notifier("describe.sh", &[]);
    let err = other.check().await.expect_err("a secrets plugin");
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}
