//! Shell fixture plugins for each case of the exec plugin protocol. The
//! plugins are files of this test tree, not files that a test writes: a fork
//! of another test while such a file is still open makes the `exec` of it
//! fail with "Text file busy".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use borant_core::ports::PortErrorKind;
use borant_exec::{ExecConfig, call, describe};
use serde_json::{Value, json};

/// The path of a fixture plugin.
fn plugin(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn cfg(name: &str) -> ExecConfig {
    ExecConfig {
        command: vec![plugin(name).display().to_string()],
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(10),
        log_stderr: false,
    }
}

#[tokio::test]
async fn a_good_reply_is_the_ok_value() {
    let cfg = cfg("describe.sh");
    let value = call(&cfg, "describe", Value::Null).await.unwrap();
    assert_eq!(value["kind"], "secrets");
    assert_eq!(value["resolve"], true);
    let value = describe(&cfg, "secrets").await.unwrap();
    assert_eq!(value["v"], 1);
    let err = describe(&cfg, "provisioner").await.unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

#[tokio::test]
async fn describe_requires_protocol_version_one() {
    let err = describe(&cfg("describe_v2.sh"), "secrets")
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
    assert!(!err.message.contains('2'), "{err}");
}

#[tokio::test]
async fn error_codes_map_to_port_kinds() {
    // The plugin echoes the code it gets as its first argument, and puts a
    // marker in the message that must not reach the caller's error.
    let cases = [
        ("unavailable", PortErrorKind::Unavailable),
        ("not_found", PortErrorKind::NotFound),
        ("invalid", PortErrorKind::Invalid),
        ("denied", PortErrorKind::Denied),
        ("internal", PortErrorKind::Other),
        ("something_else", PortErrorKind::Other),
    ];
    for (code, kind) in cases {
        let mut cfg = cfg("error.sh");
        cfg.command.push(code.to_owned());
        let err = call(&cfg, "list", Value::Null).await.unwrap_err();
        assert_eq!(err.kind, kind, "{code}: {err}");
        assert!(
            !err.message.contains("marker-value"),
            "the error message holds text of the reply: {err}"
        );
    }
}

#[tokio::test]
async fn a_reply_that_is_not_json_is_other() {
    let err = call(&cfg("bad_json.sh"), "list", Value::Null)
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Other, "{err}");
    assert!(!err.message.contains("marker-value"), "{err}");
}

#[tokio::test]
async fn a_failed_exit_without_a_reply_is_other() {
    let err = call(&cfg("exit3.sh"), "list", Value::Null)
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Other, "{err}");
    assert!(err.message.contains('3'), "{err}");
}

#[tokio::test]
async fn a_timeout_kills_the_program() {
    let mut cfg = cfg("slow.sh");
    cfg.timeout = Duration::from_millis(200);
    let started = Instant::now();
    let err = call(&cfg, "list", Value::Null).await.unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Timeout, "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the caller waited {:?}",
        started.elapsed()
    );
}

/// Runs the `env` fixture with `out` as its argument and returns the
/// environment that the plugin saw.
async fn environment_of(dir: &Path, cfg: &ExecConfig) -> BTreeMap<String, String> {
    let out = dir.join("env.txt");
    let mut cfg = cfg.clone();
    cfg.command.push(out.display().to_string());
    let value = call(&cfg, "describe", Value::Null).await.unwrap();
    assert_eq!(value, Value::Null);
    let text = std::fs::read_to_string(&out).unwrap();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The keys that `sh` itself adds to the environment of the plugin.
const SH_KEYS: [&str; 4] = ["PWD", "SHLVL", "OLDPWD", "_"];

#[tokio::test]
async fn the_environment_is_empty_plus_path_home_and_the_table() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = cfg("env.sh");
    cfg.env.insert("BORANT_TEST_KEY".into(), "value".into());
    let env = environment_of(d.path(), &cfg).await;
    assert_eq!(
        env.get("BORANT_TEST_KEY").map(String::as_str),
        Some("value")
    );
    assert!(env.contains_key("PATH"), "{env:?}");
    assert!(env.contains_key("HOME"), "{env:?}");
    assert!(!env.contains_key("BORANT_CREDENTIALS_DIRECTORY"), "{env:?}");
    assert!(!env.contains_key("BORANT_CREDENTIAL"), "{env:?}");
    // Nothing else of the environment of this test reaches the plugin.
    for (key, _) in std::env::vars() {
        if key == "PATH" || key == "HOME" || SH_KEYS.contains(&key.as_str()) {
            continue;
        }
        assert!(
            !env.contains_key(&key),
            "{key} of the caller reached the plugin: {env:?}"
        );
    }
}

#[tokio::test]
async fn the_credential_names_its_directory_and_its_file() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = cfg("env.sh");
    cfg.credential = Some((d.path().join("credentials"), "store-token".into()));
    let env = environment_of(d.path(), &cfg).await;
    assert_eq!(
        env.get("BORANT_CREDENTIALS_DIRECTORY").map(String::as_str),
        Some(d.path().join("credentials").to_string_lossy().as_ref())
    );
    assert_eq!(
        env.get("BORANT_CREDENTIAL").map(String::as_str),
        Some("store-token")
    );
}

#[tokio::test]
async fn the_request_is_version_op_and_the_arguments() {
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("request.json");
    let mut cfg = cfg("echo_request.sh");
    cfg.command.push(out.display().to_string());
    let value = call(
        &cfg,
        "resolve",
        json!({"refs": ["one", "two"], "v": 9, "op": "fake"}),
    )
    .await
    .unwrap();
    assert_eq!(value, Value::Null);
    let request: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(request["v"], 1, "{request}");
    assert_eq!(request["op"], "resolve", "{request}");
    assert_eq!(request["refs"], json!(["one", "two"]), "{request}");
}

#[tokio::test]
async fn the_arguments_are_a_json_object() {
    let err = call(&cfg("describe.sh"), "resolve", json!(["one"]))
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

#[tokio::test]
async fn a_missing_program_is_invalid() {
    let cfg = ExecConfig {
        command: vec!["/nonexistent/borant-exec-fixture".into()],
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(5),
        log_stderr: false,
    };
    let err = call(&cfg, "describe", Value::Null).await.unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

/// A writer that keeps what the subscriber formats.
#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the capture is not poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `f` with a subscriber that keeps the lines of level `max` and
/// returns them.
async fn log_lines<F>(max: tracing::Level, f: F) -> String
where
    F: Future<Output = ()>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(max)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    f.await;
    drop(guard);
    let bytes = capture
        .0
        .lock()
        .expect("the capture is not poisoned")
        .clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn the_stderr_of_the_program_is_a_size_by_default() {
    // A plugin that prints a secret value must not put it in the log.
    let cfg = cfg("stderr.sh");
    let lines = log_lines(tracing::Level::DEBUG, async {
        call(&cfg, "describe", Value::Null).await.unwrap();
    })
    .await;
    assert!(lines.contains("bytes to stderr"), "{lines}");
    assert!(!lines.contains("note for the log"), "{lines}");
}

#[tokio::test]
async fn the_stderr_of_the_program_goes_to_debug_when_enabled() {
    let mut cfg = cfg("stderr.sh");
    cfg.log_stderr = true;
    let lines = log_lines(tracing::Level::DEBUG, async {
        call(&cfg, "describe", Value::Null).await.unwrap();
    })
    .await;
    assert!(lines.contains("note for the log"), "{lines}");
    let lines = log_lines(tracing::Level::INFO, async {
        call(&cfg, "describe", Value::Null).await.unwrap();
    })
    .await;
    assert!(!lines.contains("note for the log"), "{lines}");
}
