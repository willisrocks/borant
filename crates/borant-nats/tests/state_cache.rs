//! The cache behind `list` against a real NATS server. The tests skip when
//! `BORANT_NATS_URL` is unset. Start a server with `just nats-up`.
//!
//! The hub lists on a one-second tick. A consumer per list made the server
//! create and delete consumers every second and replay every key, so the
//! cache must stay at one consumer and still show a write to the next list.

use std::time::Duration;

use async_nats::jetstream;
use borant_core::ports::StateStore;
use borant_core::testing;
use borant_nats::{KV_BUCKET, Nats, NatsConfig, STATE_CACHE_CONSUMER};
use bytes::Bytes;
use futures::StreamExt;

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-state-cache-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

/// The consumers of the KV stream that feed a list cache.
async fn cache_consumers(n: &Nats) -> usize {
    let js = jetstream::new(n.client().clone());
    let stream = js
        .get_stream(format!("KV_{KV_BUCKET}"))
        .await
        .expect("the KV stream");
    let mut consumers = stream.consumers();
    let mut found = 0;
    while let Some(info) = consumers.next().await {
        let info = info.expect("consumer info");
        if info.config.description.as_deref() == Some(STATE_CACHE_CONSUMER) {
            found += 1;
        }
    }
    found
}

#[tokio::test]
async fn list_keeps_one_consumer_and_follows_writes() {
    let Some(n) = nats().await else {
        return;
    };
    let store = n.state();
    let prefix = format!("{}.", testing::unique("cache"));

    // The first list creates the one consumer of this store.
    assert!(store.list(&prefix).await.expect("list empty").is_empty());
    let after_warmup = cache_consumers(&n).await;
    assert!(after_warmup >= 1, "the cache has a consumer");

    // A thousand lists, as many as the hub's tick makes in a quarter hour,
    // create no consumer and replay nothing.
    for i in 0..1_000 {
        store
            .list(&prefix)
            .await
            .unwrap_or_else(|e| panic!("list {i}: {e:?}"));
    }
    assert_eq!(
        cache_consumers(&n).await,
        after_warmup,
        "a list must reuse the consumer, not create one"
    );

    // A write by this store is visible to the next list of this store.
    let key = format!("{prefix}k");
    let rev = store
        .create(&key, Bytes::from_static(b"one"))
        .await
        .expect("create");
    let listed = store.list(&prefix).await.expect("list after create");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, key);
    assert_eq!(listed[0].value, Bytes::from_static(b"one"));
    assert_eq!(listed[0].revision, rev);

    // A second store, as another process would open, sees the state and the
    // changes that follow.
    let other = n.state();
    let rev2 = store
        .update(&key, Bytes::from_static(b"two"), rev)
        .await
        .expect("update");
    let seen = other
        .list(&prefix)
        .await
        .expect("list from the second store");
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].value, Bytes::from_static(b"two"));
    assert_eq!(seen[0].revision, rev2);

    // A write through the second store reaches the first on its next list.
    let key2 = format!("{prefix}other");
    let rev3 = other
        .create(&key2, Bytes::from_static(b"three"))
        .await
        .expect("create from the second store");
    let seen = store.list(&prefix).await.expect("list after the write");
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[1].key, key2);
    assert_eq!(seen[1].revision, rev3);

    // A delete leaves the list of the writer at once, and the list of the
    // second store after its next sync.
    store.delete(&key, Some(rev2)).await.expect("delete");
    assert!(
        store
            .list(&prefix)
            .await
            .expect("list after delete")
            .iter()
            .all(|e| e.key != key)
    );
    assert!(
        other
            .list(&prefix)
            .await
            .expect("list of the second store")
            .iter()
            .all(|e| e.key != key),
        "the delete reaches the second store"
    );

    store
        .delete(&key2, None)
        .await
        .expect("delete the second key");

    // A cached key keeps the value it was last written with, not the first.
    let listed = store.list(&prefix).await.expect("final list");
    assert!(listed.is_empty(), "{listed:?}");

    // Dropping the second store deletes its consumer, so a restarted daemon
    // does not leave one behind.
    std::mem::drop(other);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if cache_consumers(&n).await == after_warmup {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a dropped store leaves its consumer behind"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
