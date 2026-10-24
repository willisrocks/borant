//! Contract suites for port adapters. Each adapter crate runs these against
//! its own implementation.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;

use crate::ids::{AgentId, EventId, MessageId, TaskId};
use crate::model::{
    Envelope, EnvelopeKind, Harness, Sender, SessionEvent, SessionEventBody, Target, TaskEvent,
    TaskEventKind, TaskStatus,
};
use crate::ports::{Change, MessageBus, PortErrorKind, SessionStore, StateStore};
use crate::time::Timestamp;

/// A unique name for one test run, so tests can share a backend.
pub fn unique(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    )
}

pub fn envelope(to: Target, text: &str) -> Envelope {
    let kind = match to {
        Target::Agent(_) => EnvelopeKind::Direct,
        Target::Channel(_) => EnvelopeKind::Broadcast,
    };
    Envelope {
        id: MessageId::new(),
        from: Sender::Human,
        to,
        kind,
        text: text.to_owned(),
        urgent: false,
        event: None,
        sent: Timestamp(0),
    }
}

pub async fn state_store_contract(store: &dyn StateStore) {
    let p = unique("t");
    let k = format!("{p}.a");
    let rev = store
        .create(&k, Bytes::from_static(b"1"))
        .await
        .expect("create");
    let err = store
        .create(&k, Bytes::from_static(b"2"))
        .await
        .expect_err("second create");
    assert_eq!(err.kind, PortErrorKind::Conflict);

    let got = store.get(&k).await.expect("get").expect("present");
    assert_eq!(got.value, Bytes::from_static(b"1"));
    assert_eq!(got.revision, rev);

    let err = store
        .update(&k, Bytes::from_static(b"3"), rev + 1000)
        .await
        .expect_err("stale update");
    assert_eq!(err.kind, PortErrorKind::Conflict);
    let rev2 = store
        .update(&k, Bytes::from_static(b"3"), rev)
        .await
        .expect("update");
    assert!(rev2 > rev);

    let mut watch = store.watch(&format!("{p}.")).await.expect("watch");
    store
        .create(&format!("{p}.b"), Bytes::from_static(b"b"))
        .await
        .expect("create b");
    store
        .create(&format!("{p}x.c"), Bytes::from_static(b"c"))
        .await
        .expect("create other prefix");
    let change = tokio::time::timeout(Duration::from_secs(5), watch.next())
        .await
        .expect("watch timeout")
        .expect("watch item");
    match change {
        Change::Put(e) => assert_eq!(e.key, format!("{p}.b")),
        Change::Delete { .. } => panic!("expected a put"),
    }

    let keys: Vec<String> = store
        .list(&format!("{p}."))
        .await
        .expect("list")
        .into_iter()
        .map(|e| e.key)
        .collect();
    assert_eq!(keys, vec![format!("{p}.a"), format!("{p}.b")]);

    let err = store.delete(&k, Some(rev)).await.expect_err("stale delete");
    assert_eq!(err.kind, PortErrorKind::Conflict);
    store.delete(&k, Some(rev2)).await.expect("delete");
    assert!(store.get(&k).await.expect("get").is_none());
    store.delete(&k, None).await.expect("delete missing");
    // A deleted key can be created again.
    store
        .create(&k, Bytes::from_static(b"again"))
        .await
        .expect("recreate");
}

pub async fn message_bus_contract(bus: &dyn MessageBus) {
    let agent: AgentId = unique("a").parse().expect("agent id");
    let chan: crate::ids::ChannelName = unique("c").parse().expect("channel");
    let subjects = crate::keys::subjects::inbox(&agent, std::slice::from_ref(&chan));

    // A message published before the inbox exists does not arrive.
    let early = envelope(Target::Agent(agent.clone()), "early");
    bus.publish(&subjects[0], &early)
        .await
        .expect("publish early");

    bus.ensure_inbox(&agent, &subjects)
        .await
        .expect("ensure inbox");
    bus.ensure_inbox(&agent, &subjects)
        .await
        .expect("ensure inbox twice");

    let direct = envelope(Target::Agent(agent.clone()), "one");
    let broadcast = envelope(Target::Channel(chan.clone()), "two");
    bus.publish(&subjects[0], &direct)
        .await
        .expect("publish direct");
    bus.publish(&subjects[1], &broadcast)
        .await
        .expect("publish broadcast");
    // The same envelope twice is stored once.
    bus.publish(&subjects[1], &broadcast)
        .await
        .expect("publish duplicate");

    assert_eq!(bus.inbox_pending(&agent).await.expect("pending"), 2);
    let got = bus
        .pull(&agent, 10, Duration::from_secs(2))
        .await
        .expect("pull");
    let texts: Vec<&str> = got.iter().map(|d| d.envelope.text.as_str()).collect();
    assert_eq!(texts, vec!["one", "two"]);

    let mut got = got.into_iter();
    got.next().expect("first").acker.ack().await.expect("ack");
    drop(got);
    let unacked = bus.unacked(&agent).await.expect("unacked");
    assert_eq!(unacked.len(), 1);
    assert_eq!(unacked[0].text, "two");

    // A pull with nothing new waits about `wait` and returns empty, or
    // returns the unacked message again.
    let again = bus
        .pull(&agent, 10, Duration::from_millis(300))
        .await
        .expect("pull again");
    assert!(again.iter().all(|d| d.envelope.text == "two"));
    for d in again {
        d.acker.ack().await.expect("ack");
    }

    // Requests and replies.
    let subject = unique("borant.test.");
    let mut served = bus.serve(&subject).await.expect("serve");
    tokio::spawn(async move {
        while let Some(req) = served.next().await {
            let mut reply = b"re:".to_vec();
            reply.extend_from_slice(&req.payload);
            let _ = req.responder.respond(Bytes::from(reply)).await;
        }
    });
    let reply = bus
        .request(
            &subject,
            Bytes::from_static(b"ping"),
            Duration::from_secs(5),
        )
        .await
        .expect("request");
    assert_eq!(reply, Bytes::from_static(b"re:ping"));

    // Subscriptions with wildcards.
    let mut sub = bus
        .subscribe("borant.chan.*.tasks")
        .await
        .expect("subscribe");
    let mut event = envelope(Target::Channel(chan.clone()), "task changed");
    event.kind = EnvelopeKind::Event;
    event.event = Some(TaskEvent {
        channel: chan.clone(),
        task: TaskId::new(),
        title: "t".into(),
        kind: TaskEventKind::Updated,
        status: TaskStatus::Stuck,
        attention_msg: None,
        owner: agent.clone(),
    });
    bus.publish(&subjects[2], &event)
        .await
        .expect("publish event");
    let seen = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("subscribe timeout")
        .expect("subscribe item");
    assert_eq!(seen.id, event.id);

    bus.delete_inbox(&agent).await.expect("delete inbox");
}

pub async fn session_store_contract(store: &dyn SessionStore) {
    let agent: AgentId = unique("s").parse().expect("agent id");
    let ev = |text: &str| SessionEvent {
        id: EventId::new(),
        agent: agent.clone(),
        session: None,
        harness: Harness::Fake,
        host: "h".parse().expect("host"),
        turn: 1,
        at: Timestamp(0),
        body: SessionEventBody::UserPrompt { text: text.into() },
    };
    let s1 = store.append(&agent, &ev("a")).await.expect("append");
    let s2 = store.append(&agent, &ev("b")).await.expect("append");
    let s3 = store.append(&agent, &ev("c")).await.expect("append");
    assert!(s1 < s2 && s2 < s3);

    let all = store.read(&agent).await.expect("read");
    assert_eq!(
        all.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![s1, s2, s3]
    );

    store.trim_before(&agent, s2).await.expect("trim");
    let rest = store.read(&agent).await.expect("read");
    assert_eq!(
        rest.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![s2, s3]
    );

    assert!(store.agents().await.expect("agents").contains(&agent));
    store.delete(&agent).await.expect("delete");
    assert!(store.read(&agent).await.expect("read").is_empty());
}
