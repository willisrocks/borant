//! The event log against a real NATS server. The tests skip when
//! `BORANT_NATS_URL` is unset. Start a server with `just nats-up`.

use std::time::Duration;

use borant_core::events::{EventFilter, Severity, SwarmEvent, SwarmEventBody};
use borant_core::keys::subjects;
use borant_core::ports::EventLog;
use borant_core::testing;
use borant_core::{ChannelName, EventId, Timestamp};
use borant_nats::{Nats, NatsConfig};
use futures::StreamExt;
use futures::stream::BoxStream;

async fn nats() -> Option<Nats> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url],
        name: "borant-events-test".into(),
        ..Default::default()
    };
    Some(borant_nats::connect(&cfg).await.expect("connect"))
}

/// A channel that names the events of one test in a stream that the tests
/// share.
fn channel() -> ChannelName {
    testing::unique("c").parse().expect("a channel name")
}

fn claims(channel: &ChannelName, at: i64) -> SwarmEvent {
    SwarmEvent::hub(
        Timestamp(at),
        Severity::Info,
        SwarmEventBody::ClaimsReleased {
            tasks: 1,
            after_secs: 30,
        },
    )
    .about_channel(channel.clone())
}

/// The next event of a reader, or a failure after 5 seconds.
async fn next(reader: &mut BoxStream<'static, SwarmEvent>) -> SwarmEvent {
    tokio::time::timeout(Duration::from_secs(5), reader.next())
        .await
        .expect("an event within 5 s")
        .expect("a live reader")
}

/// Every event of a reader that does not follow.
async fn all(mut reader: BoxStream<'static, SwarmEvent>) -> Vec<SwarmEvent> {
    let mut out = Vec::new();
    while let Some(event) = reader.next().await {
        out.push(event);
    }
    out
}

fn mine(channel: &ChannelName) -> EventFilter {
    EventFilter {
        channel: Some(channel.clone()),
        ..EventFilter::default()
    }
}

#[tokio::test]
async fn events_contract() {
    let Some(n) = nats().await else {
        return;
    };
    testing::event_log_contract(&n.events()).await;
}

#[tokio::test]
async fn events_store_a_duplicate_id_once() {
    let Some(n) = nats().await else {
        return;
    };
    let log = n.events();
    let channel = channel();
    let event = claims(&channel, 2_000);
    log.publish(&event).await.expect("publish");
    log.publish(&event).await.expect("publish twice");
    let seen = all(log.read(&mine(&channel), false).await.expect("read")).await;
    assert_eq!(
        seen.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![event.id],
        "the event ID stores one event"
    );
}

#[tokio::test]
async fn events_follow_sees_a_new_event() {
    let Some(n) = nats().await else {
        return;
    };
    let log = n.events();
    let channel = channel();
    let mut reader = log.read(&mine(&channel), true).await.expect("follow");
    let published = claims(&channel, 2_100);
    log.publish(&published).await.expect("publish");
    assert_eq!(next(&mut reader).await.id, published.id);
}

#[tokio::test]
async fn events_skip_an_event_of_an_unknown_kind() {
    let Some(n) = nats().await else {
        return;
    };
    let log = n.events();
    let channel = channel();
    // An event of a newer release: its JSON decodes as JSON, but this core
    // knows no such kind. It must not end the reader or fail it.
    let future = serde_json::json!({
        "id": EventId::new().to_string(),
        "at": 2_200,
        "source": "hub",
        "severity": "info",
        "channel": channel.to_string(),
        "kind": "test.from_the_future",
    });
    let subject = subjects::events("test.from_the_future");
    n.client()
        .publish(
            subject,
            serde_json::to_vec(&future).expect("the raw event").into(),
        )
        .await
        .expect("publish the raw event");
    n.client().flush().await.expect("flush");
    // A real event on the same channel, after the one that does not decode.
    let real = claims(&channel, 2_210);
    log.publish(&real).await.expect("publish");
    let seen = all(log.read(&mine(&channel), false).await.expect("read")).await;
    assert_eq!(
        seen.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![real.id],
        "a reader skips a kind that it does not know"
    );
}

#[tokio::test]
async fn events_reject_a_kind_that_is_not_a_subject() {
    let Some(n) = nats().await else {
        return;
    };
    let err = match n
        .events()
        .read(
            &EventFilter {
                kinds: vec!["lease.*.expired".into()],
                ..EventFilter::default()
            },
            false,
        )
        .await
    {
        Ok(_) => panic!("a bad kind is a bad request"),
        Err(e) => e,
    };
    assert_eq!(
        err.kind,
        borant_core::ports::PortErrorKind::Invalid,
        "{err}"
    );
}
