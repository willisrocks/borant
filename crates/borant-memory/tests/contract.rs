use borant_core::testing;
use borant_memory::{MemoryBus, MemorySession, MemoryState};

#[tokio::test]
async fn state_store_contract() {
    testing::state_store_contract(&MemoryState::new()).await;
}

#[tokio::test]
async fn message_bus_contract() {
    testing::message_bus_contract(&MemoryBus::new()).await;
}

#[tokio::test]
async fn session_store_contract() {
    testing::session_store_contract(&MemorySession::new()).await;
}

#[tokio::test]
async fn unavailable_backend_fails() {
    let s = MemoryState::new();
    s.set_available(false);
    let err = borant_core::ports::StateStore::get(&s, "x")
        .await
        .unwrap_err();
    assert_eq!(err.kind, borant_core::ports::PortErrorKind::Unavailable);
}
