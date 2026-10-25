use std::collections::BTreeMap;

use borant_core::ports::PortErrorKind;
use borant_core::ports::secrets::{SecretRef, SecretStore};
use borant_core::testing;
use borant_secrets::{EnvStore, FileStore};

/// `secret_store_contract` takes the known refs beside their values.
fn pairs<'a>(known: &[(&'a str, &'a str)]) -> Vec<(SecretRef, &'a str)> {
    known
        .iter()
        .map(|(r, v)| (SecretRef((*r).into()), *v))
        .collect()
}

#[tokio::test]
async fn env_store_contract() {
    let known = [
        ("BORANT_TEST_TOKEN", "env-value-one"),
        ("BORANT_TEST_OTHER", "env-value-two"),
    ];
    let vars = BTreeMap::from([
        ("BORANT_TEST_TOKEN".to_string(), "env-value-one".to_string()),
        ("BORANT_TEST_OTHER".to_string(), "env-value-two".to_string()),
        // An unasked variable, so a store that returns everything fails.
        (
            "BORANT_TEST_UNASKED".to_string(),
            "env-value-three".to_string(),
        ),
    ]);
    let store: &dyn SecretStore = &EnvStore::from_vars(vars);
    testing::secret_store_contract(
        store,
        &pairs(&known),
        SecretRef("BORANT_TEST_MISSING".into()),
    )
    .await;
}

#[tokio::test]
async fn file_store_contract() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token"), "file-value-one\n").unwrap();
    std::fs::write(dir.path().join("other"), "file-value-two").unwrap();
    let store: &dyn SecretStore = &FileStore::new(dir.path());
    let known = [("token", "file-value-one"), ("other", "file-value-two")];
    testing::secret_store_contract(store, &pairs(&known), SecretRef("absent".into())).await;
}

#[tokio::test]
async fn file_store_refuses_refs_outside_the_root() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("secrets");
    std::fs::create_dir(&dir).unwrap();
    // A file that `../x` would reach, so a leak shows as a value.
    std::fs::write(root.path().join("x"), "outside-value").unwrap();
    let store = FileStore::new(&dir);
    for bad in [
        "../x",
        "/etc/passwd",
        "a/b",
        "..",
        ".",
        "sub/../token",
        "./token",
        "",
    ] {
        let err = match store.resolve(&[SecretRef(bad.into())]).await {
            Ok(resolved) => panic!("{bad:?} must be refused, got {resolved:?}"),
            Err(err) => err,
        };
        assert_eq!(err.kind, PortErrorKind::Other, "{bad:?}: {err}");
        assert!(
            !err.message.contains("outside-value"),
            "{bad:?} leaks a value: {err}"
        );
    }
}

#[tokio::test]
async fn file_store_refuses_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let store = FileStore::new(dir.path());
    let err = store
        .resolve(&[SecretRef("nested".into())])
        .await
        .expect_err("a directory is not a secret file");
    assert_eq!(err.kind, PortErrorKind::Other);
}
