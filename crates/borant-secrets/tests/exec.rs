//! The exec store against the shell fixtures of the exec protocol. The
//! fixtures are files of this test tree, not files that a test writes: a fork
//! of another test while such a file is still open makes the `exec` of it
//! fail with "Text file busy".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use borant_core::ports::PortErrorKind;
use borant_core::ports::secrets::{SecretRef, SecretStore};
use borant_core::testing;
use borant_exec::ExecConfig;
use borant_secrets::{ExecStore, StoreConfig, open_store};

/// The path of the fixture plugin.
fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/secrets.sh")
}

fn exec_cfg() -> ExecConfig {
    ExecConfig {
        command: vec![plugin().display().to_string()],
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(10),
        log_stderr: false,
    }
}

fn store_config(env: BTreeMap<String, String>, credential: Option<&str>) -> StoreConfig {
    StoreConfig::Exec {
        command: vec![plugin().display().to_string()],
        env,
        credential: credential.map(str::to_owned),
        timeout_secs: 10,
    }
}

fn pairs<'a>(known: &[(&'a str, &'a str)]) -> Vec<(SecretRef, &'a str)> {
    known
        .iter()
        .map(|(r, v)| (SecretRef((*r).into()), *v))
        .collect()
}

#[tokio::test]
async fn exec_store_contract() {
    let store: &dyn SecretStore = &ExecStore::new(exec_cfg());
    let known = [
        ("fixture-token", "fixture-value-one"),
        ("fixture-other", "fixture-value-two"),
    ];
    testing::secret_store_contract(store, &pairs(&known), SecretRef("fixture-absent".into())).await;
}

#[tokio::test]
async fn exec_store_debug_holds_no_value() {
    let mut cfg = exec_cfg();
    cfg.env
        .insert("PLUGIN_TOKEN".into(), "env-secret-value".into());
    let store = ExecStore::new(cfg);
    let printed = format!("{store:?}");
    assert!(printed.contains("secrets.sh"), "{printed}");
    assert!(printed.contains("PLUGIN_TOKEN"), "{printed}");
    assert!(!printed.contains("env-secret-value"), "{printed}");

    let resolved = store
        .resolve(&[SecretRef("fixture-token".into())])
        .await
        .unwrap();
    let printed = format!("{resolved:?}");
    assert!(!printed.contains("fixture-value-one"), "{printed}");
}

#[tokio::test]
async fn exec_store_maps_a_plugin_error_kind_and_hides_its_message() {
    let store = ExecStore::new(exec_cfg());
    let err = store
        .resolve(&[SecretRef("plugin-error".into())])
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Denied, "{err}");
    assert!(!err.message.contains("s3cret-plugin-error"), "{err}");
    assert!(!format!("{err:?}").contains("s3cret-plugin-error"), "{err}");

    let err = store
        .resolve(&[SecretRef("plugin-missing".into())])
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::NotFound, "{err}");
}

#[tokio::test]
async fn exec_store_asks_the_plugin_nothing_for_an_empty_list() {
    // A plugin that would fail is never started: no command means Invalid.
    let mut cfg = exec_cfg();
    cfg.command.clear();
    let store = ExecStore::new(cfg);
    let resolved = store.resolve(&[]).await.unwrap();
    assert!(resolved.values.is_empty());
    assert!(store.resolve(&[SecretRef("x".into())]).await.is_err());
}

#[tokio::test]
async fn exec_open_store_describes_the_plugin() {
    let store = open_store(&store_config(BTreeMap::new(), None), None)
        .await
        .unwrap();
    assert_eq!(store.kind(), "exec");
    assert!(store.capabilities().resolve);
    let resolved = store
        .resolve(&[SecretRef("fixture-token".into())])
        .await
        .unwrap();
    assert_eq!(resolved.values.len(), 1);
}

#[tokio::test]
async fn exec_open_store_sets_the_credential_vars() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = BTreeMap::new();
    env.insert("BORANT_EXPECT_CREDENTIAL".into(), "store-token".into());
    let store = open_store(&store_config(env, Some("store-token")), Some(dir.path()))
        .await
        .unwrap();
    assert_eq!(store.kind(), "exec");
}

#[tokio::test]
async fn exec_open_store_refuses_a_credential_without_a_directory() {
    let err = open_store(&store_config(BTreeMap::new(), Some("store-token")), None)
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
}

#[tokio::test]
async fn exec_open_store_refuses_a_plugin_without_resolve() {
    let mut env = BTreeMap::new();
    env.insert("BORANT_FIXTURE_RESOLVE".into(), "false".into());
    let err = open_store(&store_config(env, None), None)
        .await
        .unwrap_err();
    assert_eq!(err.kind, PortErrorKind::Invalid, "{err}");
    assert!(err.message.contains("resolve"), "{err}");
}
