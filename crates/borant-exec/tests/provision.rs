//! The provisioner port on the exec plugin protocol. The contract runs
//! against `tests/plugins/provision-file.sh`, a plugin outside the Rust tree,
//! which is the shape of the real plugins.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use borant_core::ids::{PoolName, Tag};
use borant_core::ports::PortErrorKind;
use borant_core::ports::provision::{HostRecord, HostSpec, ProviderHostId, Provisioner};
use borant_core::testing;
use borant_exec::ExecConfig;
use borant_exec::provision::ExecProvisioner;
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The file plugin of the repository root: hosts in one JSON file.
fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/plugins/provision-file.sh")
        .canonicalize()
        .expect("the file plugin")
}

fn exec(command: Vec<String>) -> ExecConfig {
    ExecConfig {
        command,
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(60),
    }
}

/// The config of the file plugin: where the hosts are.
fn file_config(file: &Path) -> BTreeMap<String, Value> {
    BTreeMap::from([("file".to_owned(), json!(file.display().to_string()))])
}

fn spec(pool: &PoolName) -> HostSpec {
    HostSpec {
        pool: pool.clone(),
        name: testing::unique("h").parse().expect("a host name"),
        size: "small".to_owned(),
        region: "r1".to_owned(),
        image: None,
        provider_labels: BTreeMap::from([("swarm".to_owned(), "main".to_owned())]),
        tags: vec!["vps".parse::<Tag>().expect("a tag")],
        boot: BTreeMap::from([("BORANT_JOIN".to_owned(), "one-time-value".to_owned())]),
    }
}

#[tokio::test]
async fn the_file_plugin_passes_the_provisioner_contract() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("hosts.json");
    // The backend holds a host that this provisioner did not create: a delete
    // of it is denied, not Ok.
    let foreign = ProviderHostId(testing::unique("foreign"));
    let state = json!({ "next": 1, "hosts": [], "foreign": [&foreign.0] });
    std::fs::write(&file, serde_json::to_vec(&state).expect("the state")).expect("seed the state");
    let p = ExecProvisioner::new(
        exec(vec![plugin().display().to_string()]),
        file_config(&file),
    );
    let pool: PoolName = testing::unique("p").parse().expect("a pool name");
    testing::provisioner_contract(&p, &pool, spec(&pool), foreign).await;
}

#[tokio::test]
async fn every_provisioner_request_carries_the_config_and_the_wire_names() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let seen = dir.path().join("request.json");
    let command = vec![
        fixture("echo_request.sh").display().to_string(),
        seen.display().to_string(),
    ];
    let exec = exec(command);
    let p = ExecProvisioner::new(
        exec,
        BTreeMap::from([("firewall".to_owned(), json!("borant-locked"))]),
    );
    let pool: PoolName = testing::unique("p").parse().expect("a pool name");
    let spec = spec(&pool);
    // The echo plugin replies `null`, which is not a host record.
    let err = p
        .create(&spec)
        .await
        .expect_err("create needs a host record");
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
    assert!(
        !err.message.contains("one-time-value"),
        "no boot value in an error: {err}"
    );
    let sent: Value =
        serde_json::from_slice(&std::fs::read(&seen).expect("the request")).expect("the request");
    assert_eq!(sent["v"], 1);
    assert_eq!(sent["op"], "create");
    assert_eq!(sent["config"]["firewall"], "borant-locked");
    assert_eq!(sent["spec"]["name"], spec.name.as_str());
    assert_eq!(sent["spec"]["size"], "small");
    assert_eq!(sent["spec"]["region"], "r1");
    assert_eq!(sent["spec"]["labels"], json!({"swarm": "main"}));
    assert_eq!(sent["spec"]["boot"]["BORANT_JOIN"], "one-time-value");
    assert!(sent["spec"].get("provider_labels").is_none(), "{sent}");
    assert!(sent["spec"].get("tags").is_none(), "{sent}");

    // The next request writes over the file, so each op is read at once.
    p.describe().await.expect_err("describe needs capabilities");
    let sent: Value =
        serde_json::from_slice(&std::fs::read(&seen).expect("the request")).expect("the request");
    assert_eq!(sent["op"], "describe");
    assert_eq!(sent["config"]["firewall"], "borant-locked");
}

#[tokio::test]
async fn one_provisioner_op_runs_at_a_time() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lock = dir.path().join("lock");
    let command = vec![
        fixture("provision-serial.sh").display().to_string(),
        lock.display().to_string(),
    ];
    let p = ExecProvisioner::new(
        exec(command),
        BTreeMap::from([("marker".to_owned(), json!("ok"))]),
    );
    // The plugin answers `internal` if the two run at once. Both get Ok only
    // if the adapter runs one op at a time for this instance.
    let first_id = ProviderHostId("first".to_owned());
    let second_id = ProviderHostId("second".to_owned());
    let first = p.delete(&first_id);
    let second = p.delete(&second_id);
    let (first, second) = tokio::join!(first, second);
    first.expect("the first delete");
    second.expect("the second delete");
}

#[tokio::test]
async fn a_provisioner_op_times_out() {
    let p = ExecProvisioner::new(
        exec(vec![fixture("slow.sh").display().to_string()]),
        BTreeMap::new(),
    )
    .with_timeout(Some(Duration::from_millis(200)));
    let started = Instant::now();
    let err = p
        .delete(&ProviderHostId("any".to_owned()))
        .await
        .expect_err("the plugin never answers");
    assert_eq!(err.kind, PortErrorKind::Timeout, "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout is {:?}",
        started.elapsed()
    );
}

/// The record of a create is a full `HostRecord`: the protocol's `created` is
/// an RFC 3339 time and its `labels` are the port's `provider_labels`.
#[tokio::test]
async fn a_provisioner_record_reads_the_wire_names() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("hosts.json");
    let p = ExecProvisioner::new(
        exec(vec![plugin().display().to_string()]),
        file_config(&file),
    );
    let pool: PoolName = testing::unique("p").parse().expect("a pool name");
    let spec = spec(&pool);
    let record: HostRecord = p.create(&spec).await.expect("create");
    assert_eq!(record.pool, pool);
    assert_eq!(record.name, spec.name);
    assert_eq!(record.provider_labels, spec.provider_labels);
    assert_eq!(record.state.as_str(), "running");
    // A real date, not the epoch: 2020-09-13 in ms. The exact reading of the
    // RFC 3339 form is a unit test of the adapter.
    assert!(record.created.0 > 1_600_000_000_000, "{record:?}");
    let hosts = p.list(&pool).await.expect("list");
    assert_eq!(hosts, vec![record]);
}
