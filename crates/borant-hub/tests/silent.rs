//! A host that stops beating and loses all its leases at once: one warn for
//! the host and not one per agent, and one line when it is back (seen on
//! 2026-11-10: the macbook in clamshell sleep expired 14 leases at once, and
//! nothing named the host, so it read as a Borant outage).

mod common;

use borant_core::events::{
    EventFilter, EventSource, Severity, SwarmEvent, SwarmEventBody, kind_of,
};
use borant_core::model::Harness;
use borant_core::proto::{CtlRequest, Registered, SessionLeaseRef};
use borant_core::{AgentId, EventId, HostName, Timestamp};
use common::{Fx, name};
use futures::StreamExt;
use serde::Deserialize;

/// The rule of the alert that a silent host fires.
const HOST_SILENT: &str = "host_silent";

/// A beat of `host` that reports `regs` and nothing else.
async fn beat(fx: &Fx, host: &str, regs: &[&Registered]) {
    fx.req(CtlRequest::HostHeartbeat {
        host: name(host),
        sessions: regs
            .iter()
            .map(|r| SessionLeaseRef {
                session: r.session,
                agent: r.agent.clone(),
                token: r.token,
                idle_since: None,
                last_event_at: None,
                pinned: false,
                activity: None,
            })
            .collect(),
        launching: Vec::new(),
        stats: None,
    })
    .await
    .expect("the beat");
}

/// An end that the host reports, as an ordinary session shutdown does.
async fn end(fx: &Fx, reg: &Registered, reason: &str) {
    fx.req(CtlRequest::EndSession {
        session: reg.session,
        agent: reg.agent.clone(),
        token: reg.token,
        reason: reason.into(),
    })
    .await
    .expect("the end");
}

async fn read_events(fx: &Fx) -> Vec<SwarmEvent> {
    let mut stream = fx
        .events
        .read(&EventFilter::default(), false)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event);
    }
    out
}

/// The `host_silent` alerts of `events`, fired or resolved.
fn silent_alerts(events: &[SwarmEvent]) -> Vec<&SwarmEvent> {
    events
        .iter()
        .filter(|e| {
            matches!(
                &e.body,
                SwarmEventBody::AlertFired { rule, .. }
                    | SwarmEventBody::AlertResolved { rule, .. }
                    if rule == HOST_SILENT
            )
        })
        .collect()
}

/// Three agents on one host, each with a beat, at `t0`.
async fn one_host_three_agents(fx: &Fx) -> (Registered, Registered, Registered) {
    fx.host("h1", &[Harness::Fake]).await;
    let w1 = fx.register("h1", Some("w1"), None).await;
    let w2 = fx.register("h1", Some("w2"), None).await;
    let w3 = fx.register("h1", Some("w3"), None).await;
    beat(fx, "h1", &[&w1, &w2, &w3]).await;
    (w1, w2, w3)
}

/// One host that stopped beating: one warn that names the host, the last time
/// it was seen, and its leases, and one notice to the person. The per-agent
/// expiry lines stay.
#[tokio::test]
async fn a_silent_host_fires_one_alert_and_not_one_per_agent() {
    let fx = Fx::new();
    let (_w1, _w2, _w3) = one_host_three_agents(&fx).await;

    // The lid closes: no beat for longer than the lease.
    fx.advance(31);
    fx.hub.tick().await.unwrap();

    let events = read_events(&fx).await;
    let fired = silent_alerts(&events);
    assert_eq!(
        fired.len(),
        1,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    let SwarmEventBody::AlertFired { rule, subject } = &fired[0].body else {
        panic!("not a fire: {:?}", fired[0].body);
    };
    assert_eq!(rule, HOST_SILENT);
    assert_eq!(subject, "h1");
    assert_eq!(fired[0].severity, Severity::Warn);
    assert_eq!(fired[0].host, Some(name("h1")));
    let note = fired[0].note.as_deref().unwrap_or_default();
    assert!(note.contains("host h1 is silent since"), "{note}");
    assert!(note.contains("3 leases expired"), "{note}");

    // The lease pass keeps its per-agent line.
    let expired = events
        .iter()
        .filter(|e| kind_of(e) == "lease.expired")
        .count();
    assert_eq!(
        expired,
        3,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );

    // The person hears the host once, and the notice names its agents.
    let notices = fx.recorder.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].title, "A host is silent");
    assert!(notices[0].body.contains("h1"), "{}", notices[0].body);
    assert!(
        notices[0].body.contains("w1, w2, w3"),
        "{}",
        notices[0].body
    );
}

/// The host beats again: one info line with how long it was silent and what it
/// took back, however many beats it queued while it was away.
#[tokio::test]
async fn host_back_fires_once_after_a_flush_of_queued_beats() {
    let fx = Fx::new();
    let (w1, w2, w3) = one_host_three_agents(&fx).await;
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    assert_eq!(
        silent_alerts(&read_events(&fx).await).len(),
        1,
        "the silence"
    );

    // The lid opens: three beats land at once, as a woken host flushes them.
    fx.advance(8);
    beat(&fx, "h1", &[&w1, &w2, &w3]).await;
    beat(&fx, "h1", &[&w1, &w2, &w3]).await;
    beat(&fx, "h1", &[&w1, &w2, &w3]).await;
    fx.hub.tick().await.unwrap();

    let events = read_events(&fx).await;
    let resolved: Vec<&SwarmEvent> = silent_alerts(&events)
        .into_iter()
        .filter(|e| matches!(&e.body, SwarmEventBody::AlertResolved { .. }))
        .collect();
    assert_eq!(
        resolved.len(),
        1,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    let SwarmEventBody::AlertResolved { rule, subject } = &resolved[0].body else {
        panic!("not a resolve: {:?}", resolved[0].body);
    };
    assert_eq!(rule, HOST_SILENT);
    assert_eq!(subject, "h1");
    assert_eq!(resolved[0].severity, Severity::Info);
    assert_eq!(resolved[0].host, Some(name("h1")));
    let note = resolved[0].note.as_deref().unwrap_or_default();
    assert!(note.contains("host h1 was silent for"), "{note}");
    assert!(note.contains("3 of 3 leases reclaimed"), "{note}");

    // One silence is one line: the next pass says nothing more.
    fx.hub.tick().await.unwrap();
    let after = silent_alerts(&read_events(&fx).await).len();
    assert_eq!(after, 2, "one fire and one resolve");
}

/// A lease that ends while the host still beats is no host silence: a session
/// that ends cleanly, and one that disappears while its host runs the rest.
#[tokio::test]
async fn a_lease_that_ends_on_a_live_host_is_no_silent_host() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let w1 = fx.register("h1", Some("w1"), None).await;
    let w2 = fx.register("h1", Some("w2"), None).await;
    let w3 = fx.register("h1", Some("w3"), None).await;
    beat(&fx, "h1", &[&w1, &w2, &w3]).await;

    // w1 ends its session: an ordinary release.
    end(&fx, &w1, "session shutdown").await;
    // w2's session goes without an end, while the host keeps beating for w3.
    fx.advance(31);
    beat(&fx, "h1", &[&w3]).await;
    fx.hub.tick().await.unwrap();

    let events = read_events(&fx).await;
    assert!(
        silent_alerts(&events).is_empty(),
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    assert!(
        fx.recorder.notices().is_empty(),
        "no notice for a live host"
    );
}

/// The reader of v0.23.x: the kinds that release knows. The new hub publishes
/// only these, so an old host or plugin keeps decoding the stream.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
#[allow(dead_code)]
enum OldBody {
    #[serde(rename = "alert.fired")]
    AlertFired { rule: String, subject: String },
    #[serde(rename = "alert.resolved")]
    AlertResolved { rule: String, subject: String },
}

/// The event envelope of v0.23.x, with the old body.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OldEvent {
    id: EventId,
    at: Timestamp,
    source: EventSource,
    severity: Severity,
    #[serde(default)]
    seq: u64,
    agent: Option<AgentId>,
    host: Option<HostName>,
    note: Option<String>,
    #[serde(flatten)]
    body: OldBody,
}

/// The hard rule: an old reader decodes the new event. A new body kind fails
/// its enum, which is why the silence rides on the alert kinds.
#[tokio::test]
async fn an_old_reader_decodes_the_new_event() {
    let fx = Fx::new();
    let _ = one_host_three_agents(&fx).await;
    fx.advance(31);
    fx.hub.tick().await.unwrap();

    let events = read_events(&fx).await;
    let fired = silent_alerts(&events);
    assert_eq!(fired.len(), 1, "a silence to decode");
    let json = serde_json::to_string(fired[0]).expect("the event as JSON");
    let old: OldEvent = serde_json::from_str(&json).expect("an old reader decodes it");
    assert_eq!(old.severity, Severity::Warn);
    assert_eq!(old.source, EventSource::Hub);
    assert!(old.at.0 > 0, "the event carries its time");
    assert_eq!(old.host, Some(name("h1")));
    assert!(old.note.is_some_and(|n| n.contains("3 leases expired")));
    let OldBody::AlertFired { rule, subject } = old.body else {
        panic!("an old reader sees the alert");
    };
    assert_eq!(rule, HOST_SILENT);
    assert_eq!(subject, "h1");

    // A new kind would not decode: the reason this reuses the alert kinds.
    let err = serde_json::from_str::<OldBody>(r#"{"kind":"host.silent","host":"h1"}"#);
    assert!(err.is_err(), "an old reader drops an unknown kind");
}

/// Silence is per host: two hosts that stop together are two lines, and a
/// host that beats every lease time is none.
#[tokio::test]
async fn each_silent_host_is_its_own_line() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let a = fx.register("h1", Some("a1"), None).await;
    let b = fx.register("h2", Some("b1"), None).await;
    beat(&fx, "h1", &[&a]).await;
    beat(&fx, "h2", &[&b]).await;
    fx.advance(31);
    fx.hub.tick().await.unwrap();

    let events = read_events(&fx).await;
    let fired = silent_alerts(&events);
    let subjects: Vec<&str> = fired
        .iter()
        .filter_map(|e| match &e.body {
            SwarmEventBody::AlertFired { subject, .. } => Some(subject.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        subjects,
        vec!["h1", "h2"],
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    // A beat of h2 alone takes h2 back and leaves h1 silent.
    beat(&fx, "h2", &[&b]).await;
    fx.hub.tick().await.unwrap();
    let events = read_events(&fx).await;
    let resolved: Vec<&str> = silent_alerts(&events)
        .iter()
        .filter_map(|e| match &e.body {
            SwarmEventBody::AlertResolved { subject, .. } => Some(subject.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(resolved, vec!["h2"]);
}
