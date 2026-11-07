//! The inbox counters of the message bus against a real NATS server. The
//! tests skip when `BORANT_NATS_URL` is unset. Start a server with
//! `just nats-up`.

use std::time::Duration;

use async_nats::jetstream::{self, AckKind, consumer::pull};
use borant_core::keys::subjects;
use borant_core::model::Target;
use borant_core::ports::MessageBus;
use borant_core::{AgentId, testing};
use borant_nats::{MSG_STREAM, Nats, NatsBus, NatsConfig};
use futures::StreamExt;

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-inbox-stats-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

/// An inbox with one message for each text, and its agent.
async fn inbox_with(bus: &NatsBus, texts: &[&str]) -> AgentId {
    let agent: AgentId = testing::unique("a").parse().expect("an agent id");
    let subjects = subjects::inbox(&agent, &[]);
    bus.ensure_inbox(&agent, &subjects)
        .await
        .expect("ensure the inbox");
    for text in texts {
        let envelope = testing::envelope(Target::Agent(agent.clone()), text);
        bus.publish(&subjects[0], &envelope).await.expect("publish");
    }
    agent
}

#[tokio::test]
async fn inbox_stats_report_pending_and_ack_pending() {
    let Some(n) = nats().await else {
        return;
    };
    let bus = n.bus();
    let agent = inbox_with(&bus, &["one", "two"]).await;

    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!(
        (stats.pending, stats.ack_pending, stats.redelivered),
        (2, 0, 0),
        "{stats:?}"
    );

    // One delivered and not acked.
    let mut got = bus
        .pull(&agent, 1, Duration::from_secs(2))
        .await
        .expect("pull");
    let delivery = got.pop().expect("one delivery");
    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!((stats.pending, stats.ack_pending), (1, 1), "{stats:?}");

    // Acked, so nothing is pending on the agent's side any more.
    delivery.acker.ack().await.expect("ack");
    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!((stats.pending, stats.ack_pending), (1, 0), "{stats:?}");
}

#[tokio::test]
async fn inbox_stats_report_a_nak_as_redelivered() {
    let Some(n) = nats().await else {
        return;
    };
    let bus = n.bus();
    let agent = inbox_with(&bus, &["one"]).await;

    // Take the message from the durable consumer of the inbox and nack it.
    // The port acks only, so a nack goes through the server here: it means
    // "deliver this one again".
    let js = jetstream::new(n.client().clone());
    let stream = js.get_stream(MSG_STREAM).await.expect("the message stream");
    let consumer = stream
        .get_consumer::<pull::Config>(&format!("inbox-{agent}"))
        .await
        .expect("the inbox consumer");
    let mut messages = consumer
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(2))
        .messages()
        .await
        .expect("fetch");
    let message = messages
        .next()
        .await
        .expect("a message")
        .expect("a message");
    message.ack_with(AckKind::Nak(None)).await.expect("nak");
    // The nacked message is in flight again, and not delivered a second time
    // yet.
    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!(
        (stats.pending, stats.ack_pending, stats.redelivered),
        (0, 1, 0),
        "{stats:?}"
    );

    // The redelivery. Its counter counts the messages in flight that the
    // inbox delivered more than once, as the memory bus does.
    let got = bus
        .pull(&agent, 1, Duration::from_secs(2))
        .await
        .expect("pull");
    assert_eq!(got.len(), 1, "the nak delivers the message again");
    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!(
        (stats.pending, stats.ack_pending, stats.redelivered),
        (0, 1, 1),
        "{stats:?}"
    );

    // An acked message counts nowhere.
    for delivery in got {
        delivery.acker.ack().await.expect("ack");
    }
    let stats = bus.inbox_stats(&agent).await.expect("stats");
    assert_eq!(
        (stats.pending, stats.ack_pending, stats.redelivered),
        (0, 0, 0),
        "{stats:?}"
    );
}

/// The batched call reads the counters of every inbox in one consumer list,
/// and reports the same numbers as one call per inbox.
#[tokio::test]
async fn inbox_stats_many_reports_every_inbox_in_one_call() {
    let Some(n) = nats().await else {
        return;
    };
    let bus = n.bus();
    let one = inbox_with(&bus, &["one", "two"]).await;
    let two = inbox_with(&bus, &["three"]).await;
    let missing: AgentId = testing::unique("a").parse().expect("an agent id");

    let many = bus
        .inbox_stats_many(&[one.clone(), two.clone(), missing])
        .await
        .expect("stats");
    assert_eq!(
        many.len(),
        2,
        "an agent without an inbox is left out: {many:?}"
    );
    assert_eq!(many[0].0, one);
    assert_eq!((many[0].1.pending, many[0].1.ack_pending), (2, 0));
    assert_eq!(many[1].0, two);
    assert_eq!((many[1].1.pending, many[1].1.ack_pending), (1, 0));

    // The same numbers as the per-inbox call.
    for (agent, stats) in &many {
        assert_eq!(bus.inbox_stats(agent).await.expect("stats"), *stats);
    }

    // A pull changes the counters that the batched call reports.
    let _unacked = bus
        .pull(&one, 1, Duration::from_secs(2))
        .await
        .expect("pull");
    let many = bus
        .inbox_stats_many(std::slice::from_ref(&one))
        .await
        .expect("stats");
    assert_eq!(many.len(), 1);
    assert_eq!(
        (many[0].1.pending, many[0].1.ack_pending),
        (1, 1),
        "{many:?}"
    );
}
