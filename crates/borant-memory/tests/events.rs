//! The in-memory event log, and the inbox stats of the in-memory bus.

use borant_core::AgentId;
use borant_core::ports::MessageBus;
use borant_core::testing::event_log_contract;
use borant_memory::{MemoryBus, MemoryEventLog};

#[tokio::test]
async fn events_contract_holds_for_the_memory_log() {
    let log = MemoryEventLog::new();
    event_log_contract(&log).await;
}

#[tokio::test]
async fn events_a_refused_backend_reports_unavailable() {
    let log = MemoryEventLog::new();
    log.set_available(false);
    let event = borant_core::events::SwarmEvent::hub(
        borant_core::Timestamp(1),
        borant_core::events::Severity::Info,
        borant_core::events::SwarmEventBody::HostDraining { agents: 0 },
    );
    let err = borant_core::ports::EventLog::publish(&log, &event)
        .await
        .expect_err("an unavailable backend");
    assert_eq!(err.kind, borant_core::ports::PortErrorKind::Unavailable);
}

#[tokio::test]
async fn inbox_stats_report_pending_and_inflight() {
    let bus = MemoryBus::new();
    let agent: AgentId = "w1".parse().unwrap();
    let subject = borant_core::subjects::agent(&agent);
    bus.ensure_inbox(&agent, std::slice::from_ref(&subject))
        .await
        .unwrap();
    let envelope =
        borant_core::testing::envelope(borant_core::model::Target::Agent(agent.clone()), "hello");
    bus.publish(&subject, &envelope).await.unwrap();

    let stats = bus.inbox_stats(&agent).await.unwrap();
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.ack_pending, 0);
    assert_eq!(stats.redelivered, 0);

    // A delivery that is not acked stays in flight.
    let delivered = bus
        .pull(&agent, 1, std::time::Duration::from_millis(10))
        .await
        .unwrap();
    assert_eq!(delivered.len(), 1);
    let stats = bus.inbox_stats(&agent).await.unwrap();
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.ack_pending, 1);
    delivered
        .into_iter()
        .next()
        .unwrap()
        .acker
        .ack()
        .await
        .unwrap();
    let stats = bus.inbox_stats(&agent).await.unwrap();
    assert_eq!(stats.ack_pending, 0);
}
