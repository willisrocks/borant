//! The host fills its stats in each heartbeat: the load, the counters, and one
//! entry for each session with its last event.

mod common;

use borant_core::model::SwarmView;
use borant_core::proto::{CtlRequest, LocalState, ToolCall};
use common::Fx;

#[tokio::test]
async fn stats_heartbeat_carries_the_host_and_its_sessions() {
    let fx = Fx::new(true, |_| {}).await;
    let session = fx.session("a1", "work").await;
    assert_eq!(session.state, LocalState::Attached);

    // The stats of one heartbeat carry the round trip of the previous one.
    fx.host.heartbeat_once().await;
    fx.host.heartbeat_once().await;

    let v = fx
        .hub
        .handle(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::View,
        })
        .await
        .into_result()
        .expect("the view");
    let view: SwarmView = serde_json::from_value(v).expect("the view parses");
    assert_eq!(view.hosts.len(), 1);
    let stats = view.hosts[0].stats.as_ref().expect("the host sent stats");
    assert!(!stats.version.is_empty());
    assert!(stats.cpus >= 1, "the host reports its processors");
    assert!(stats.started.is_some());
    assert!(stats.mem_used_pct >= 0.0);
    assert!(
        stats.last_heartbeat_rtt_ms.is_some(),
        "the previous heartbeat measured a round trip"
    );

    // One entry for the registered session, with its last event.
    assert_eq!(stats.sessions.len(), 1);
    let s = &stats.sessions[0];
    assert_eq!(s.agent, "a1".parse::<borant_core::AgentId>().unwrap());
    assert_eq!(s.turn, 0);
    assert!(!s.busy);
    assert!(!s.paused);
    assert!(
        s.last_event_at.is_some(),
        "the registration appends a SessionStart event"
    );
}
