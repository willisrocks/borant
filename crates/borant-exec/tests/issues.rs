//! The issue-tracker port on the exec plugin protocol. The contract runs
//! against `tests/plugins/issues-file.sh`, a plugin outside the Rust tree,
//! which is the shape of the real plugin. The wire format is the one of the
//! private plugin, so the requests are asserted field by field.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use borant_core::ports::issues::{IssueState, IssueTracker, IssueWrite};
use borant_core::ports::{PortErrorKind, PortResult};
use borant_core::testing;
use borant_exec::ExecConfig;
use borant_exec::issues::{ExecIssues, KIND, WRITE_TIMEOUT};
use serde_json::{Value, json};

/// The file plugin of the repository root: the issues of one JSON file.
fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/plugins/issues-file.sh")
        .canonicalize()
        .expect("the file plugin")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn exec(command: Vec<String>) -> ExecConfig {
    ExecConfig {
        command,
        env: BTreeMap::new(),
        credential: None,
        timeout: WRITE_TIMEOUT,
        log_stderr: false,
    }
}

/// The config of the file plugin: where the issues are.
fn file_config(file: &Path) -> BTreeMap<String, Value> {
    BTreeMap::from([("file".to_owned(), json!(file.display().to_string()))])
}

/// One write of every shape: a team, a parent only, and neither.
fn write(key: &str, target: Option<&str>, parent: Option<&str>) -> IssueWrite {
    IssueWrite {
        key: key.to_owned(),
        target: target.map(str::to_owned),
        parent: parent.map(str::to_owned),
        title: "Mirror the sagas".into(),
        body: "The body.".into(),
        state: IssueState::Todo,
    }
}

fn recorded(file: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(file).expect("the request")).expect("json")
}

#[tokio::test]
async fn issues_the_file_plugin_passes_the_contract() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("issues.json");
    let issues = ExecIssues::open(
        exec(vec![plugin().display().to_string()]),
        file_config(&file),
    )
    .await
    .expect("an issues plugin");
    testing::issue_tracker_contract(&issues, "T").await;
}

#[tokio::test]
async fn issues_a_program_of_another_kind_is_invalid() {
    // The notifier fixture reports another kind, so the mirror stays off.
    let err = ExecIssues::open(
        exec(vec![fixture("describe_notifier.sh").display().to_string()]),
        BTreeMap::new(),
    )
    .await
    .expect_err("another kind");
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

#[tokio::test]
async fn issues_the_requests_match_the_wire_format() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let seen = dir.path().join("request.json");
    let config = file_config(Path::new("/nowhere.json"));
    let issues = ExecIssues::open(
        exec(vec![
            fixture("issues_request.sh").display().to_string(),
            seen.display().to_string(),
        ]),
        config.clone(),
    )
    .await
    .expect("an issues plugin");

    assert_eq!(
        recorded(&seen),
        json!({"v": 1, "op": "describe", "config": {"file": "/nowhere.json"}}),
        "describe carries the config, as a provisioner's does"
    );

    let first = write(
        "3f2b8c1e-9d4a-4f6b-8c2d-1e5a7b9c0d3f",
        Some("ENG"),
        Some("ENG-1"),
    );
    let got = issues.create(&first).await.expect("a create");
    assert_eq!(got.key, "T-1");
    assert_eq!(got.url, "u");
    assert_eq!(
        recorded(&seen),
        json!({
            "v": 1,
            "op": "create",
            "config": {"file": "/nowhere.json"},
            "write": {
                "key": "3f2b8c1e-9d4a-4f6b-8c2d-1e5a7b9c0d3f",
                "target": "ENG",
                "parent": "ENG-1",
                "title": "Mirror the sagas",
                "body": "The body.",
                "state": "todo",
            },
        })
    );

    // `target` and `parent` are always present, as `null` when unset.
    let bare = write("6a1d0f2c-3e4b-4c5d-9e6f-7a8b9c0d1e2f", None, Some("ENG-1"));
    issues.create(&bare).await.expect("a create");
    let request = recorded(&seen);
    assert_eq!(request["op"], "create");
    assert_eq!(
        request["write"]["target"],
        Value::Null,
        "an unset target goes on the wire: {request}"
    );
    assert_eq!(request["write"]["parent"], "ENG-1");

    let got = issues
        .update("ENG-123", &first, false)
        .await
        .expect("an update");
    assert_eq!(got.key, "T-1");
    assert_eq!(
        recorded(&seen),
        json!({
            "v": 1,
            "op": "update",
            "config": {"file": "/nowhere.json"},
            "issue": "ENG-123",
            "write": {
                "key": "3f2b8c1e-9d4a-4f6b-8c2d-1e5a7b9c0d3f",
                "target": "ENG",
                "parent": "ENG-1",
                "title": "Mirror the sagas",
                "body": "The body.",
                "state": "todo",
            },
            "full": false,
        })
    );

    issues
        .update("ENG-123", &first, true)
        .await
        .expect("a full update");
    let request = recorded(&seen);
    assert_eq!(request["full"], true);
    assert_eq!(KIND, "issues");
}

#[tokio::test]
async fn issues_error_codes_map_to_kinds() {
    let cases = [
        ("not_found", PortErrorKind::NotFound),
        ("invalid", PortErrorKind::Invalid),
        ("denied", PortErrorKind::Denied),
        ("unavailable", PortErrorKind::Unavailable),
        ("internal", PortErrorKind::Other),
    ];
    for (code, kind) in cases {
        let issues = ExecIssues::new(
            exec(vec![
                fixture("error.sh").display().to_string(),
                code.to_owned(),
            ]),
            BTreeMap::new(),
        );
        let got: PortResult<_> = issues
            .update("ENG-1", &write("k", Some("ENG"), None), false)
            .await;
        let err = got.expect_err(code);
        assert_eq!(err.kind, kind, "{code}: {err}");
    }
}
