//! The registration of a session, and its retry after a failed write.
//!
//! A host retries a registration with the same session ID (see
//! `borant-host`). When the hub fails after it took the lease, for example on
//! the agent write or because a reply was lost, that retry must take its own
//! lease back instead of meeting "the lease of the agent is held by a session
//! on host". Another session still meets that refusal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use borant_core::model::{Harness, HostInfo, Lease, LeaseState, RepoInfo};
use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
use borant_core::proto::{CtlRequest, RegisterSession, Registered, SessionLeaseRef};
use borant_core::{AgentId, FencingToken, HostName, ManualClock, SessionId, Timestamp, keys};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use bytes::Bytes;
use futures::stream::BoxStream;

fn host(s: &str) -> HostName {
    s.parse().unwrap()
}

fn repo() -> RepoInfo {
    RepoInfo {
        remote_url: "git://git/demo.git".into(),
        branch: "main".into(),
        subdir: String::new(),
    }
}

/// A state store that fails the first create of one key, like a write whose
/// reply the hub never saw, and then behaves.
#[derive(Debug)]
struct FailOnce {
    inner: MemoryState,
    key: String,
    armed: AtomicBool,
}

#[async_trait::async_trait]
impl StateStore for FailOnce {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        self.inner.get(key).await
    }

    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64> {
        if key == self.key && self.armed.swap(false, Ordering::SeqCst) {
            return Err(PortError::unavailable("the reply of the write was lost"));
        }
        self.inner.create(key, value).await
    }

    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64> {
        self.inner.update(key, value, expected).await
    }

    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
        self.inner.delete(key, expected).await
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        self.inner.list(prefix).await
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        self.inner.watch(prefix).await
    }
}

async fn lease_of(state: &MemoryState, agent: &AgentId) -> Lease {
    let entry = state
        .get(&keys::lease_agent(agent))
        .await
        .expect("read the lease")
        .expect("the lease is there");
    serde_json::from_slice(&entry.value).expect("a lease")
}

#[tokio::test]
async fn a_register_retry_keeps_its_lease() {
    let agent: AgentId = "boss".parse().unwrap();
    let name = host("host-a");
    let session = SessionId::new();
    let state = MemoryState::new();
    // The write right after the lease create fails once: the agent record.
    let store = Arc::new(FailOnce {
        inner: state.clone(),
        key: keys::agent(&agent),
        armed: AtomicBool::new(true),
    });
    let hub = Hub::new(
        store,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        Arc::new(ManualClock::new(Timestamp(1_000))),
        HubConfig::default(),
    );
    hub.handle(CtlRequest::RegisterHost {
        info: HostInfo {
            name: name.clone(),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 4,
            ..Default::default()
        },
    })
    .await
    .into_result()
    .expect("register the host");

    let register = |session: SessionId| {
        CtlRequest::RegisterSession(RegisterSession {
            session,
            host: name.clone(),
            harness: Harness::Fake,
            pid: Some(7),
            cwd: "/work".into(),
            repo: Some(repo()),
            agent: Some(agent.clone()),
            ..Default::default()
        })
    };

    // The first attempt takes the lease and fails on the agent write.
    let err = hub
        .handle(register(session))
        .await
        .into_result()
        .expect_err("the first registration fails");
    assert!(
        err.message.contains("lost"),
        "the failure is the agent write: {err:?}"
    );
    let leased = lease_of(&state, &agent).await;
    assert_eq!(leased.token, FencingToken(1));
    assert_eq!(
        leased.state,
        LeaseState::Held {
            session,
            host: name.clone(),
            expires: Timestamp(1_000).plus(HubConfig::default().lease.lease),
        }
    );

    // The same session retries, as the host does, and gets its own lease.
    let reg: Registered = serde_json::from_value(
        hub.handle(register(session))
            .await
            .into_result()
            .expect("the retry registers"),
    )
    .expect("a registration");
    assert_eq!(reg.agent, agent);
    assert_eq!(reg.token, FencingToken(1), "the retry keeps the token");
    let leased = lease_of(&state, &agent).await;
    assert_eq!(leased.token, reg.token, "the registration names its lease");
    assert_eq!(
        leased.state,
        LeaseState::Held {
            session,
            host: name.clone(),
            expires: Timestamp(1_000).plus(HubConfig::default().lease.lease),
        }
    );

    // The token of the reply renews the lease: the harness and the hub agree.
    let reply: borant_core::proto::HeartbeatReply = serde_json::from_value(
        hub.handle(CtlRequest::HostHeartbeat {
            host: name.clone(),
            sessions: vec![SessionLeaseRef {
                session,
                agent: agent.clone(),
                token: reg.token,
                idle_since: None,
                last_event_at: None,
                pinned: false,
            }],
            stats: None,
        })
        .await
        .into_result()
        .expect("the heartbeat"),
    )
    .expect("a heartbeat reply");
    assert!(reply.lost.is_empty(), "the lease is still the session's");

    // Another session on the same host does not take the live agent.
    let err = hub
        .handle(register(SessionId::new()))
        .await
        .into_result()
        .expect_err("another session cannot take the lease");
    assert_eq!(
        err.message,
        "the lease of the agent is held by a session on host host-a"
    );
    assert_eq!(lease_of(&state, &agent).await.state, leased.state);
}
