use std::collections::BTreeMap;

use borant_core::ids::{PoolName, Tag};
use borant_core::ports::provision::{HostSpec, ProviderHostId};
use borant_core::ports::{ChangeState, ChangeStatus, CheckState, ReviewState};
use borant_core::testing;
use borant_memory::{MemoryBus, MemoryChanges, MemoryProvisioner, MemorySession, MemoryState};

#[tokio::test]
async fn state_store_contract() {
    let store = MemoryState::new();
    testing::state_store_contract(&store).await;
    testing::state_store_key_rules(&store).await;
}

#[tokio::test]
async fn provisioner_contract() {
    let p = MemoryProvisioner::new();
    let pool: PoolName = testing::unique("p").parse().expect("pool name");
    // A host of another pool that the backend holds and the provisioner did
    // not create: a delete of it is Denied, not Ok.
    let foreign = ProviderHostId(testing::unique("foreign"));
    p.plant_foreign(foreign.clone());
    let spec = HostSpec {
        pool: pool.clone(),
        name: testing::unique("h").parse().expect("host name"),
        size: "small".to_owned(),
        region: "r1".to_owned(),
        image: None,
        provider_labels: BTreeMap::from([("swarm".to_owned(), "main".to_owned())]),
        tags: vec!["vps".parse::<Tag>().expect("tag")],
        boot: BTreeMap::from([("BORANT_JOIN".to_owned(), "one-time-value".to_owned())]),
    };
    testing::provisioner_contract(&p, &pool, spec, foreign).await;
}

#[tokio::test]
async fn message_bus_contract() {
    testing::message_bus_contract(&MemoryBus::new()).await;
}

#[tokio::test]
async fn changes_contract() {
    let changes = MemoryChanges::new();
    let url = "https://example.invalid/acme/app/pull/7";
    let status = ChangeStatus {
        state: ChangeState::Open,
        head: "abc123".to_owned(),
        checks: CheckState::Fail {
            failed: vec!["ci".to_owned()],
        },
        review: ReviewState::None,
    };
    changes.set(url, status.clone());
    testing::change_requests_contract(&changes, url, status).await;
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
