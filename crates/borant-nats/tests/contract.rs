//! Runs the port contract suites against a real NATS server. The tests skip
//! when `BORANT_NATS_URL` is unset. Start a server with `just nats-up`.

use borant_core::testing;
use borant_nats::{Nats, NatsConfig};

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-contract-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

#[tokio::test]
async fn state_store_contract() {
    if let Some(n) = nats().await {
        testing::state_store_contract(&n.state()).await;
    }
}

#[tokio::test]
async fn message_bus_contract() {
    if let Some(n) = nats().await {
        testing::message_bus_contract(&n.bus()).await;
    }
}

#[tokio::test]
async fn session_store_contract() {
    if let Some(n) = nats().await {
        testing::session_store_contract(&n.session()).await;
    }
}
