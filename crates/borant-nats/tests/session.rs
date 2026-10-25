//! The session log reads in batches: a log of 2000 events reads in well under
//! a second on localhost, oldest first (perf follow-up to PR #79).

use std::time::{Duration, Instant};

use borant_core::model::{Harness, SessionEvent, SessionEventBody};
use borant_core::ports::SessionStore;
use borant_core::{AgentId, EventId, Timestamp, testing};
use borant_nats::{Nats, NatsConfig};

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-session-read-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

fn event(agent: &AgentId, turn: usize) -> SessionEvent {
    SessionEvent {
        id: EventId::new(),
        agent: agent.clone(),
        session: None,
        harness: Harness::Fake,
        host: "h".parse().expect("host"),
        turn: u64::try_from(turn).unwrap_or(u64::MAX),
        at: Timestamp(i64::try_from(turn).unwrap_or(0)),
        body: SessionEventBody::UserPrompt {
            text: format!("event {turn}"),
        },
    }
}

#[tokio::test]
async fn session_read_of_2000_events_is_quick() {
    let Some(n) = nats().await else {
        return;
    };
    let store = n.session();
    let agent: AgentId = testing::unique("r").parse().expect("agent id");
    for i in 0..2000 {
        store
            .append(&agent, &event(&agent, i))
            .await
            .expect("append");
    }

    let start = Instant::now();
    let events = store.read(&agent).await.expect("read the log");
    let read = start.elapsed();
    eprintln!("read {} events in {read:?}", events.len());

    assert_eq!(events.len(), 2000, "every event of the log");
    assert!(
        events.windows(2).all(|w| w[0].0 < w[1].0),
        "oldest first, with rising sequence numbers"
    );
    assert_eq!(events[0].1.turn, 0);
    assert_eq!(events[1999].1.turn, 1999);
    // One direct get per event took 719 ms for this log on localhost; the
    // batched read takes about 63 ms, and a 30 ms link costs 2000 round trips
    // instead of a few.
    assert!(
        read < Duration::from_secs(1),
        "2000 events took {read:?} to read"
    );
    store.delete(&agent).await.expect("delete");
}
