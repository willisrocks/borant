//! The list cache must not hold a deleted key, or the hub's memory grows with
//! every key ever written. The tests skip when `BORANT_NATS_URL` is unset.
//! Start a server with `just nats-up`.

use borant_core::ports::StateStore;
use borant_core::testing;
use borant_nats::{Nats, NatsConfig};
use bytes::Bytes;

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-state-cache-entries-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

/// A thousand keys and their deletes: the map holds the keys while they are
/// live, and none of them once the delete markers have been read.
#[tokio::test]
async fn deleted_keys_leave_the_cache() {
    let Some(n) = nats().await else {
        return;
    };
    const KEYS: usize = 1_000;
    let store = n.state();
    let prefix = format!("{}.", testing::unique("gone"));

    // Prime the map, then add the keys.
    store.list(&prefix).await.expect("list empty");
    let before = store.cached_entries().await;
    for i in 0..KEYS {
        store
            .create(&format!("{prefix}k{i:04}"), Bytes::from_static(b"v"))
            .await
            .expect("create");
    }
    let listed = store.list(&prefix).await.expect("list the keys");
    assert_eq!(listed.len(), KEYS);
    assert_eq!(store.cached_entries().await, before + KEYS);

    // Every key is deleted, and the delete markers are read to their end.
    for i in 0..KEYS {
        store
            .delete(&format!("{prefix}k{i:04}"), None)
            .await
            .expect("delete");
    }
    assert!(
        store
            .list(&prefix)
            .await
            .expect("list after delete")
            .is_empty()
    );
    assert_eq!(
        store.cached_entries().await,
        before,
        "a delete marker must not stay in the map"
    );

    // A key written again after its delete is live and counted once.
    let again = format!("{prefix}again");
    store
        .create(&again, Bytes::from_static(b"new"))
        .await
        .expect("recreate after delete");
    let listed = store.list(&prefix).await.expect("list after recreate");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].value, Bytes::from_static(b"new"));
    assert_eq!(store.cached_entries().await, before + 1);
    store.delete(&again, None).await.expect("clean up");
}
