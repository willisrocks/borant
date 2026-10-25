//! The live view: the hosts with their heartbeat stats, the agents with their
//! lease, and the counts of each inbox.

mod common;

use borant_core::model::{AgentStatus, Harness, HostStats, LeaseState, SessionStats, SwarmView};
use borant_core::proto::{CtlRequest, Registered, ToolCall};
use borant_core::{AgentId, Timestamp};
use common::{Fx, name};

fn session_stats(agent: &AgentId, busy: bool, turn: u64) -> SessionStats {
    SessionStats {
        agent: agent.clone(),
        busy,
        paused: false,
        turn,
        last_event_at: Some(Timestamp(1_000)),
        wakes: 0,
        wake_limited: 0,
        ack_failed: 0,
        append_failed: 0,
        redelivered: 0,
        bad_envelope: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
    }
}

async fn heartbeat_with_stats(fx: &Fx, host: &str, load1: f64, session: SessionStats) {
    let stats = HostStats {
        version: "test".into(),
        started: Some(Timestamp(1_000)),
        load1,
        cpus: 4,
        mem_used_pct: 12.5,
        mem_avail_mb: None,
        disk_avail_mb: None,
        disk_used_pct: None,
        outbox_depth: 0,
        last_heartbeat_rtt_ms: Some(3),
        nats_reconnects: 1,
        sessions: vec![session],
    };
    fx.req(CtlRequest::HostHeartbeat {
        host: name(host),
        sessions: vec![],
        stats: Some(stats),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn view_after_two_heartbeats_has_both_hosts_and_the_inbox_stats() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    fx.host("h2", &[Harness::Fake]).await;
    let w1: Registered = fx.register("h1", Some("w1"), None).await;
    let w2: Registered = fx.register("h2", Some("w2"), None).await;

    heartbeat_with_stats(&fx, "h1", 0.5, session_stats(&w1.agent, true, 7)).await;
    heartbeat_with_stats(&fx, "h2", 1.5, session_stats(&w2.agent, false, 2)).await;

    let v = fx
        .req(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::View,
        })
        .await
        .unwrap();
    let view: SwarmView = serde_json::from_value(v).expect("the view parses");

    // Both hosts are there with their own stats, and both are online.
    assert_eq!(view.hosts.len(), 2);
    let h1 = view
        .hosts
        .iter()
        .find(|h| h.info.name == name("h1"))
        .expect("h1");
    assert!(h1.online);
    let stats = h1.stats.as_ref().expect("h1 sent its stats");
    assert_eq!(stats.load1, 0.5);
    assert_eq!(stats.cpus, 4);
    assert_eq!(stats.sessions.len(), 1);
    assert_eq!(stats.sessions[0].turn, 7);
    let h2 = view
        .hosts
        .iter()
        .find(|h| h.info.name == name("h2"))
        .expect("h2");
    assert_eq!(h2.stats.as_ref().map(|s| s.load1), Some(1.5));

    // The agents carry their lease, host, session, and busy state.
    assert_eq!(view.agents.len(), 2);
    let a1: &AgentStatus = view
        .agents
        .iter()
        .find(|a| a.agent == w1.agent)
        .expect("w1");
    assert_eq!(a1.host, Some(name("h1")));
    assert_eq!(a1.session, Some(w1.session));
    assert!(a1.busy);
    assert!(matches!(a1.lease, Some(LeaseState::Held { .. })));
    let a2 = view
        .agents
        .iter()
        .find(|a| a.agent == w2.agent)
        .expect("w2");
    assert!(!a2.busy);

    // Each agent's inbox counts come from the bus.
    assert_eq!(view.inboxes.len(), 2);
    let inbox = view
        .inboxes
        .iter()
        .find(|i| i.agent == w1.agent)
        .expect("w1's inbox");
    assert_eq!(inbox.stats.pending, 0);
    assert_eq!(inbox.stats.ack_pending, 0);
    assert!(view.at.is_some());
}

#[tokio::test]
async fn view_shows_a_host_without_stats() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let v = fx
        .req(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::View,
        })
        .await
        .unwrap();
    let view: SwarmView = serde_json::from_value(v).unwrap();
    assert_eq!(view.hosts.len(), 1);
    assert!(view.hosts[0].stats.is_none());
    assert!(view.hosts[0].last_seen.is_some(), "registering sets it");
    assert!(view.agents.is_empty());
}

/// The view carries the same facts as `borant agents`: the role, the
/// collection, the pin, the model, and the retirement state.
#[tokio::test]
async fn view_carries_the_role_the_collection_the_pin_and_the_model() {
    use borant_core::keys;
    use borant_core::model::Agent;
    use borant_core::ports::StateStore;
    use borant_core::proto::RegisterSession;

    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let chief: AgentId = "chief".parse().unwrap();
    fx.req(CtlRequest::RegisterSession(RegisterSession {
        session: borant_core::SessionId::new(),
        host: name("h1"),
        harness: Harness::Fake,
        pid: Some(1),
        cwd: "/work".into(),
        agent: Some(chief.clone()),
        role: Some("chief".parse().unwrap()),
        collection: Some("personal".parse().unwrap()),
        ..Default::default()
    }))
    .await
    .unwrap();
    // The spawn sets the model; the test writes it into the hub's record.
    let key = keys::agent(&chief);
    let entry = fx.state.get(&key).await.unwrap().expect("the agent record");
    let mut record: Agent = serde_json::from_slice(&entry.value).unwrap();
    record.model = Some("opus".into());
    fx.state
        .update(
            &key,
            serde_json::to_vec(&record).unwrap().into(),
            entry.revision,
        )
        .await
        .unwrap();
    fx.call(
        None,
        ToolCall::Pin {
            agent: chief.clone(),
            on: true,
        },
    )
    .await
    .unwrap();

    let v = fx
        .req(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::View,
        })
        .await
        .unwrap();
    let view: SwarmView = serde_json::from_value(v).unwrap();
    let a: &AgentStatus = view
        .agents
        .iter()
        .find(|a| a.agent == chief)
        .expect("the chief is in the view");
    assert_eq!(a.role.as_ref().map(|r| r.as_str()), Some("chief"));
    assert_eq!(a.collection.as_ref().map(|c| c.as_str()), Some("personal"));
    assert!(a.pinned);
    assert!(!a.retired);
    assert_eq!(a.model.as_deref(), Some("opus"));
}

/// A view from an older hub has no such fields; the parse keeps them at their
/// defaults.
#[test]
fn view_parses_without_the_new_agent_fields() {
    let old = serde_json::json!({
        "hosts": [],
        "agents": [{
            "agent": "w1",
            "harness": "fake",
            "host": "h1",
            "busy": false
        }],
        "inboxes": [],
        "at": 1000
    });
    let view: SwarmView = serde_json::from_value(old).unwrap();
    assert_eq!(view.agents[0].role, None);
    assert_eq!(view.agents[0].model, None);
    assert!(!view.agents[0].pinned);
    assert!(!view.agents[0].retired);
}
