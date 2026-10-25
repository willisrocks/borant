//! The change-request port on the exec plugin protocol. The contract runs
//! against `tests/plugins/changes-file.sh`, a plugin outside the Rust tree,
//! which is the shape of the real plugin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use borant_core::ports::PortErrorKind;
use borant_core::ports::changes::{ChangeRequests, ChangeState, ChangeStatus, CheckState, ReviewState};
use borant_core::testing;
use borant_exec::ExecConfig;
use borant_exec::changes::{ExecChangeRequests, KIND};
use serde_json::{Value, json};

/// The file plugin of the repository root: statuses in one JSON file.
fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/plugins/changes-file.sh")
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
        timeout: Duration::from_secs(30),
        log_stderr: false,
    }
}

/// The config of the file plugin: where the statuses are.
fn file_config(file: &Path) -> BTreeMap<String, Value> {
    BTreeMap::from([("file".to_owned(), json!(file.display().to_string()))])
}

/// One status per shape of the protocol.
fn statuses() -> Vec<ChangeStatus> {
    vec![
        ChangeStatus {
            state: ChangeState::Open,
            head: "aaaa1111".to_owned(),
            checks: CheckState::Pass {},
            review: ReviewState::Approved,
        },
        ChangeStatus {
            state: ChangeState::Open,
            head: "aaaa1111".to_owned(),
            checks: CheckState::Fail {
                failed: vec!["clippy".to_owned(), "test".to_owned()],
            },
            review: ReviewState::ChangesRequested,
        },
        ChangeStatus {
            state: ChangeState::Open,
            head: "bbbb2222".to_owned(),
            checks: CheckState::Pending {},
            review: ReviewState::None,
        },
        ChangeStatus {
            state: ChangeState::Merged,
            head: "cccc3333".to_owned(),
            checks: CheckState::None {},
            review: ReviewState::Approved,
        },
        ChangeStatus {
            state: ChangeState::Closed,
            head: "dddd4444".to_owned(),
            checks: CheckState::None {},
            review: ReviewState::None,
        },
    ]
}

/// The JSON file of the fixture: one status for each URL.
fn seed(file: &Path, url: &str, status: &ChangeStatus) {
    let mut map = serde_json::Map::new();
    map.insert(
        url.to_owned(),
        serde_json::to_value(status).expect("a status"),
    );
    std::fs::write(file, serde_json::to_vec(&map).expect("json")).expect("seed the statuses");
}

#[tokio::test]
async fn the_file_plugin_passes_the_change_requests_contract() {
    let dir = tempfile::tempdir().expect("a temp dir");
    for (i, expected) in statuses().into_iter().enumerate() {
        let file = dir.path().join(format!("changes-{i}.json"));
        let url = format!("https://example.test/borant/pull/{}", i + 1);
        seed(&file, &url, &expected);
        let changes = ExecChangeRequests::open(
            exec(vec![plugin().display().to_string()]),
            file_config(&file),
        )
        .await
        .expect("a change-request plugin");
        testing::change_requests_contract(&changes, &url, expected).await;
    }
}

#[tokio::test]
async fn a_program_that_is_not_a_change_plugin_is_invalid() {
    // The notifier fixture reports another kind, so the watch stays off.
    let err = ExecChangeRequests::open(
        exec(vec![fixture("describe_notifier.sh").display().to_string()]),
        BTreeMap::new(),
    )
    .await
    .expect_err("another kind");
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

#[tokio::test]
async fn the_status_request_carries_the_config_and_the_url() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let seen = dir.path().join("request.json");
    let changes = ExecChangeRequests::new(
        exec(vec![
            fixture("changes_request.sh").display().to_string(),
            seen.display().to_string(),
        ]),
        file_config(Path::new("/nowhere.json")),
    );
    let status = changes
        .status("https://example.test/1")
        .await
        .expect("a status");
    assert_eq!(status.head, "h1", "{status:?}");
    let request: Value =
        serde_json::from_slice(&std::fs::read(&seen).expect("the request")).expect("json");
    assert_eq!(request["v"], 1, "{request}");
    assert_eq!(request["op"], "status", "{request}");
    assert_eq!(request["url"], "https://example.test/1", "{request}");
    assert_eq!(
        request["config"]["file"],
        json!("/nowhere.json"),
        "the config table goes in every request: {request}"
    );
    assert_eq!(KIND, "change_requests");
}
