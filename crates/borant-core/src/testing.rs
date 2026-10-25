//! Contract suites for port adapters. Each adapter crate runs these against
//! its own implementation.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::events::{EventFilter, Severity, SwarmEvent, SwarmEventBody};

use crate::ids::{AgentId, EventId, MessageId, TaskId};
use crate::model::{
    Envelope, EnvelopeKind, Harness, Sender, SessionEvent, SessionEventBody, Target, TaskEvent,
    TaskEventKind, TaskStatus,
};
use crate::ports::provision::{HostSpec, PoolName, ProviderHostId, Provisioner, StopMode};
use crate::ports::secrets::{SecretRef, SecretStore};
use crate::ports::{
    Change, ChangeRequests, ChangeStatus, EventLog, MessageBus, PortErrorKind, SessionStore,
    StateStore,
};
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

/// The key rules of the real KV (spec section 6): a key it refuses cannot be
/// written, and reading it finds nothing. A store that keeps a colon in a key
/// in memory but loses it in production fails here.
pub async fn state_store_key_rules(store: &dyn StateStore) {
    // The KV takes `.`-separated tokens of letters, digits, `-`, `_`, `/`, and
    // `=`; an empty token, `:`, a space, or a wildcard is refused.
    let refused = ["a:b", ".a", "a.", "a..b", "a b", "", "a*", "a>"];
    let accepted = ["agent.a1", "alert.members_limited.fake-auth", "x/y=z_-"];

    for key in refused {
        if let Ok(rev) = store.create(key, Bytes::from_static(b"refused")).await {
            panic!("create of refused key {key:?} must fail, wrote revision {rev}");
        }
        assert!(
            store
                .get(key)
                .await
                .unwrap_or_else(|e| panic!("get of refused key {key:?} must not fail: {e:?}"))
                .is_none(),
            "refused key {key:?} must be absent"
        );
    }

    for key in accepted {
        // A store that shares one backend between runs finds the key of the
        // last run; a delete of it is not an error.
        store
            .delete(key, None)
            .await
            .unwrap_or_else(|e| panic!("clear accepted key {key:?}: {e:?}"));
        let value = Bytes::copy_from_slice(key.as_bytes());
        let rev = store
            .create(key, value.clone())
            .await
            .unwrap_or_else(|e| panic!("create of accepted key {key:?}: {e:?}"));
        let got = store
            .get(key)
            .await
            .unwrap_or_else(|e| panic!("get of accepted key {key:?}: {e:?}"))
            .unwrap_or_else(|| panic!("accepted key {key:?} must be present"));
        assert_eq!(got.value, value, "value of accepted key {key:?}");
        assert_eq!(got.revision, rev, "revision of accepted key {key:?}");
        store
            .delete(key, Some(rev))
            .await
            .unwrap_or_else(|e| panic!("delete of accepted key {key:?}: {e:?}"));
    }
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

    // A message published just after a pull returns arrives at the next pull,
    // not at a redelivery after the ack wait.
    for text in ["three", "four"] {
        let env = envelope(Target::Agent(agent.clone()), text);
        bus.publish(&subjects[0], &env).await.expect("publish");
        let got = bus
            .pull(&agent, 10, Duration::from_secs(2))
            .await
            .expect("pull");
        let texts: Vec<&str> = got.iter().map(|d| d.envelope.text.as_str()).collect();
        assert_eq!(texts, vec![text]);
        for d in got {
            d.acker.ack().await.expect("ack");
        }
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

/// The provisioner contract: the six points of the elastic doc ("Tests") as
/// phase 4 keeps them.
///
/// `foreign` is a host of another pool that the backend holds and that `p`
/// did not create, like a server without the ownership label. The adapter
/// plants it before the call, so a delete of it must be `Denied` and not
/// `Ok`; the id of a host that does not exist at all is `Ok`.
pub async fn provisioner_contract(
    p: &dyn Provisioner,
    pool: &PoolName,
    spec: HostSpec,
    foreign: ProviderHostId,
) {
    assert_eq!(&spec.pool, pool, "the spec belongs to the pool under test");
    let caps = p.describe().await.expect("describe");

    // Point 1: a create returns the pool, the name, and the spec labels, and
    // a second create with the same name returns the same host.
    let first = p.create(&spec).await.expect("create");
    assert_eq!(&first.pool, pool, "create returns the pool");
    assert_eq!(first.name, spec.name, "create returns the name");
    assert_eq!(
        first.provider_labels, spec.provider_labels,
        "create returns the spec labels"
    );
    let again = p.create(&spec).await.expect("a second create");
    assert_eq!(
        again.id, first.id,
        "a create is idempotent by pool and name"
    );

    // Point 2: list shows the hosts of that pool only.
    let other_pool: PoolName = unique("p").parse().expect("a pool name");
    let other_spec = HostSpec {
        pool: other_pool.clone(),
        name: unique("h").parse().expect("a host name"),
        ..spec.clone()
    };
    let elsewhere = p
        .create(&other_spec)
        .await
        .expect("a create in another pool");
    let ours = p.list(pool).await.expect("list");
    let ids: Vec<&ProviderHostId> = ours.iter().map(|r| &r.id).collect();
    assert!(
        ids.contains(&&first.id),
        "list shows the host of that pool: {ids:?}"
    );
    assert!(
        !ids.contains(&&elsewhere.id),
        "list hides a host of another pool: {ids:?}"
    );
    let theirs = p.list(&other_pool).await.expect("list of the other pool");
    assert!(
        theirs.iter().any(|r| r.id == elsewhere.id),
        "list of the other pool shows its own host"
    );
    assert!(
        !theirs.iter().any(|r| r.id == first.id),
        "list of the other pool hides the first pool's host"
    );

    // Point 3: a delete of a host that does not exist is Ok.
    let missing = ProviderHostId(unique("missing"));
    p.delete(&missing)
        .await
        .expect("a delete of a missing host is Ok");

    // Point 4: a delete of a host of another pool that this provisioner did
    // not create is Denied.
    let err = p
        .delete(&foreign)
        .await
        .expect_err("a delete of a foreign host must fail");
    assert_eq!(
        err.kind,
        PortErrorKind::Denied,
        "a delete of a foreign host is Denied: {err}"
    );

    // Point 5: a failed create returns an error and leaves no host. An
    // unlisted size is the failure that every backend can give.
    let mut unlisted = unique("size");
    while caps.sizes.iter().any(|s| s.id == unlisted) {
        unlisted = unique("size");
    }
    let bad_name: crate::ids::HostName = unique("h").parse().expect("a host name");
    let bad = HostSpec {
        name: bad_name.clone(),
        size: unlisted,
        ..spec.clone()
    };
    let err = p
        .create(&bad)
        .await
        .expect_err("a create with an unlisted size must fail");
    assert_eq!(
        err.kind,
        PortErrorKind::Invalid,
        "an unlisted size is Invalid: {err}"
    );
    let after = p.list(pool).await.expect("list");
    assert!(
        !after.iter().any(|r| r.name == bad_name),
        "a failed create leaves no host: {after:?}"
    );

    // Point 6: stop and start return Invalid, because delete is the only
    // mode that every backend offers.
    if caps.stop_modes.is_empty() {
        let err = p
            .stop(&first.id, StopMode::Stop)
            .await
            .expect_err("stop must fail by default");
        assert_eq!(
            err.kind,
            PortErrorKind::Invalid,
            "stop is Invalid by default: {err}"
        );
        let err = p
            .start(&first.id)
            .await
            .expect_err("start must fail by default");
        assert_eq!(
            err.kind,
            PortErrorKind::Invalid,
            "start is Invalid by default: {err}"
        );
    }

    // A repeat delete of an own host is Ok, and the host is gone.
    p.delete(&first.id).await.expect("delete");
    p.delete(&first.id).await.expect("a repeat delete is Ok");
    let left = p.list(pool).await.expect("list");
    assert!(
        !left.iter().any(|r| r.id == first.id),
        "delete removes the host: {left:?}"
    );
    p.delete(&elsewhere.id)
        .await
        .expect("delete the other pool host");
}

/// Rules 1 to 4 of the secrets doc, section 2.3. `known` must not be empty.
pub async fn secret_store_contract(
    store: &dyn SecretStore,
    known: &[(SecretRef, &str)],
    missing: SecretRef,
) {
    assert!(!known.is_empty(), "the contract needs one known ref");
    assert!(
        store.capabilities().resolve,
        "a secrets store must support resolve"
    );
    let refs: Vec<SecretRef> = known.iter().map(|(r, _)| r.clone()).collect();

    // Rule 1: every known ref gives its exact value, twice in a row.
    for round in 0..2 {
        let resolved = store
            .resolve(&refs)
            .await
            .unwrap_or_else(|e| panic!("resolve, round {round}: {e}"));
        assert_eq!(resolved.values.len(), refs.len(), "round {round}");
        for (r, value) in known {
            let got = resolved
                .values
                .get(r)
                .unwrap_or_else(|| panic!("{r:?} is missing, round {round}"));
            assert_eq!(got.expose(), *value, "the value of {r:?}, round {round}");
        }
        // Rule 4: the Debug output holds no value.
        assert_no_value(&format!("{resolved:?}"), known, "the Debug output");
    }

    // Rule 2: an empty list and a duplicate ref.
    let empty = store.resolve(&[]).await.expect("an empty list resolves");
    assert!(empty.values.is_empty(), "an empty list gives an empty map");
    let twice = store
        .resolve(&[refs[0].clone(), refs[0].clone()])
        .await
        .expect("a duplicate ref resolves");
    assert_eq!(twice.values.len(), 1, "a duplicate ref gives one entry");

    // Rule 3: one missing ref fails the whole call and returns no values.
    let mut asked = refs.clone();
    asked.push(missing.clone());
    let err = store
        .resolve(&asked)
        .await
        .expect_err("one missing ref must fail the call");
    assert_eq!(
        err.kind,
        PortErrorKind::NotFound,
        "a missing ref is NotFound: {err}"
    );
    assert_no_value(&format!("{err:?}"), known, "the error");
    assert_no_value(&err.message, known, "the error message");
}

fn assert_no_value(text: &str, known: &[(SecretRef, &str)], what: &str) {
    for (r, value) in known {
        assert!(
            !text.contains(value),
            "{what} holds the value of {r:?}: {text}"
        );
    }
}

/// The change-request contract: a known URL reports the seeded status, and an
/// unknown URL is `NotFound`. The adapter seeds the URL before the call, so
/// the suite needs no write path.
pub async fn change_requests_contract(
    changes: &dyn ChangeRequests,
    url: &str,
    expected: ChangeStatus,
) {
    let got = changes.status(url).await.expect("a known change");
    assert_eq!(got, expected, "a known change reports its status");
    let err = changes
        .status("https://example.invalid/no-such-change/1")
        .await
        .expect_err("an unknown change is NotFound");
    assert_eq!(err.kind, PortErrorKind::NotFound, "{err}");
}

/// The event log contract: one event per id, oldest first, filters, and follow.
pub async fn event_log_contract(log: &dyn EventLog) {
    let host: crate::HostName = unique("h").parse().unwrap();
    let agent: crate::AgentId = unique("a").parse().unwrap();
    let other: crate::AgentId = unique("b").parse().unwrap();
    // A channel names this test's events in a backend that other tests share.
    let channel: crate::ChannelName = unique("c").parse().unwrap();
    let t = Timestamp(1_000);

    let first = SwarmEvent::host(
        t,
        &host,
        Severity::Info,
        SwarmEventBody::HostOnline {
            last_seen: t,
            always_on: true,
            agents: 1,
        },
    )
    .about_agent(agent.clone())
    .about_channel(channel.clone());
    let second = SwarmEvent::hub(
        Timestamp(1_010),
        Severity::Warn,
        SwarmEventBody::LeaseExpired {
            session: crate::SessionId::new(),
            token: crate::FencingToken(3),
        },
    )
    .about_agent(agent.clone())
    .about_channel(channel.clone());
    let third = SwarmEvent::host(
        Timestamp(1_020),
        &host,
        Severity::Error,
        SwarmEventBody::DeliveryAckFailed {
            message: crate::MessageId::new(),
            error: "ack".into(),
            count: 2,
        },
    )
    .about_agent(other.clone())
    .about_channel(channel.clone());

    for event in [&first, &second, &third] {
        log.publish(event).await.expect("publish");
    }
    // Publishing the same id again stores one event.
    log.publish(&first).await.expect("publish twice");

    let mine = EventFilter {
        channel: Some(channel.clone()),
        ..EventFilter::default()
    };
    let all: Vec<SwarmEvent> = collect(log.read(&mine, false).await.expect("read")).await;
    assert_eq!(
        all.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![first.id, second.id, third.id],
        "every event once, oldest first"
    );

    // A reader that follows sees the events already there, then a new one.
    let mut live = log.read(&mine, true).await.expect("follow");
    let mut seen = Vec::new();
    while seen.len() < 3 {
        seen.push(next(&mut live).await);
    }
    let later = SwarmEvent::hub(
        Timestamp(1_030),
        Severity::Info,
        SwarmEventBody::ClaimsReleased {
            tasks: 1,
            after_secs: 30,
        },
    )
    .about_channel(channel.clone());
    log.publish(&later).await.expect("publish while following");
    assert_eq!(next(&mut live).await.id, later.id);

    // Filters.
    let by_kind: Vec<SwarmEvent> = collect(
        log.read(
            &EventFilter {
                channel: Some(channel.clone()),
                kinds: vec!["lease.*".into()],
                ..EventFilter::default()
            },
            false,
        )
        .await
        .expect("read"),
    )
    .await;
    assert_eq!(
        by_kind.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![second.id]
    );
    let by_agent: Vec<SwarmEvent> = collect(
        log.read(
            &EventFilter {
                channel: Some(channel.clone()),
                agent: Some(agent.clone()),
                ..EventFilter::default()
            },
            false,
        )
        .await
        .expect("read"),
    )
    .await;
    assert_eq!(
        by_agent.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    let warns: Vec<SwarmEvent> = collect(
        log.read(
            &EventFilter {
                channel: Some(channel.clone()),
                min_severity: Some(Severity::Warn),
                ..EventFilter::default()
            },
            false,
        )
        .await
        .expect("read"),
    )
    .await;
    assert_eq!(
        warns.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![second.id, third.id]
    );
    let recent: Vec<SwarmEvent> = collect(
        log.read(
            &EventFilter {
                channel: Some(channel.clone()),
                since: Some(Timestamp(1_015)),
                ..EventFilter::default()
            },
            false,
        )
        .await
        .expect("read"),
    )
    .await;
    assert_eq!(
        recent.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![third.id, later.id]
    );
}

async fn collect(stream: BoxStream<'static, SwarmEvent>) -> Vec<SwarmEvent> {
    use futures::StreamExt;
    let mut stream = stream;
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event);
    }
    out
}

async fn next(stream: &mut BoxStream<'static, SwarmEvent>) -> SwarmEvent {
    use futures::StreamExt;
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("an event before the timeout")
        .expect("the stream ended")
}
