//! A lost inbox consumer comes back with the messages that arrived while it
//! was gone. The tests skip when `BORANT_NATS_URL` is unset. Start a server
//! with `just nats-up`.

use std::time::Duration;

use async_nats::jetstream;
use borant_core::keys::subjects;
use borant_core::model::Target;
use borant_core::ports::MessageBus;
use borant_core::{AgentId, testing};
use borant_nats::{MSG_STREAM, Nats, NatsConfig, consumer_name};

async fn nats() -> Option<(Nats, String)> {
    let Ok(url) = std::env::var("BORANT_NATS_URL") else {
        eprintln!("skip: BORANT_NATS_URL is not set");
        return None;
    };
    let cfg = NatsConfig {
        urls: vec![url.clone()],
        name: "borant-inbox-test".into(),
        ..Default::default()
    };
    Some((borant_nats::connect(&cfg).await.expect("connect"), url))
}

/// Deletes the durable consumer of `agent`, as a broker that lost its data
/// does.
async fn delete_consumer(url: &str, agent: &AgentId) {
    let client = async_nats::connect(url).await.expect("a second client");
    let js = jetstream::new(client);
    let stream = js.get_stream(MSG_STREAM).await.expect("the message stream");
    stream
        .delete_consumer(&consumer_name(agent))
        .await
        .expect("delete the consumer");
}

#[tokio::test]
async fn a_recovered_inbox_keeps_the_backlog_once() {
    let Some((n, url)) = nats().await else {
        return;
    };
    let bus = n.bus();
    let agent: AgentId = testing::unique("a").parse().expect("an agent id");
    let inbox = subjects::inbox(&agent, &[]);
    bus.ensure_inbox(&agent, &inbox)
        .await
        .expect("ensure the inbox");

    let one = testing::envelope(Target::Agent(agent.clone()), "one");
    bus.publish(&inbox[0], &one).await.expect("publish one");

    // The broker loses the consumer. The message that waits is not acked, and
    // two more arrive while there is no consumer.
    delete_consumer(&url, &agent).await;
    let two = testing::envelope(Target::Agent(agent.clone()), "two");
    let three = testing::envelope(Target::Agent(agent.clone()), "three");
    bus.publish(&inbox[0], &two).await.expect("publish two");
    bus.publish(&inbox[0], &three).await.expect("publish three");

    // The host sees the pull fail with `not found` and recovers the inbox.
    bus.recover_inbox(&agent, &inbox)
        .await
        .expect("recover the inbox");

    let mut texts = Vec::new();
    for _ in 0..10 {
        for d in bus
            .pull(&agent, 16, Duration::from_millis(500))
            .await
            .expect("pull")
        {
            texts.push(d.envelope.text.clone());
            d.acker.ack().await.expect("ack");
        }
        if texts.len() >= 3 {
            break;
        }
    }
    texts.sort();
    assert_eq!(
        texts,
        ["one", "three", "two"],
        "the waiting messages come back once each"
    );
}
