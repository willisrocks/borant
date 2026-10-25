//! State store listing against a real NATS server. The tests skip when
//! `BORANT_NATS_URL` is unset. Start a server with `just nats-up`.

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
        name: "borant-state-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

/// `list` must return every key under the prefix, run after run. A consumer
/// that delivers before its pending count is read must not stop the listing
/// early, which would silently shorten a task or lease list.
#[tokio::test]
async fn state_list_returns_every_key_under_the_prefix() {
    const KEYS: usize = 500;
    let Some(n) = nats().await else {
        return;
    };
    let store = n.state();
    let prefix = format!("{}.", testing::unique("many"));
    for i in 0..KEYS {
        store
            .create(&format!("{prefix}k{i:04}"), Bytes::from(format!("{i}")))
            .await
            .expect("create");
    }
    for round in 0..3 {
        let listed = store.list(&prefix).await.expect("list");
        assert_eq!(listed.len(), KEYS, "round {round} lost keys");
        for (i, e) in listed.iter().enumerate() {
            assert_eq!(e.key, format!("{prefix}k{i:04}"), "round {round}");
            assert_eq!(e.value, Bytes::from(format!("{i}")), "round {round}");
        }
    }
    // A sub-prefix lists only its own keys.
    let sub = format!("{prefix}k01");
    let listed = store.list(&sub).await.expect("list sub-prefix");
    assert_eq!(listed.len(), 100);
    assert!(listed.iter().all(|e| e.key.starts_with(&sub)));
}
