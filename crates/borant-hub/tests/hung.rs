//! A hung session after a sleep: the hub flags the agent on the live view and
//! by the alert pass, and `borant stop` ends its session now instead of
//! waiting for a turn end that a frozen harness never reaches (seen on the Mac
//! on 2026-11-04: no tool call after 01:04Z, the lid closed at 01:05Z, and
//! `busy` stayed with the lease renewing).

mod common;

use borant_core::model::{Harness, HostStats, SessionStats};
use borant_core::ports::StateStore;
use borant_core::proto::{
    CtlRequest, HostCommand, PauseSelector, Registered, SessionLeaseRef, ToolCall,
};
use borant_core::{AgentId, Clock, Timestamp};
use common::{Fx, name, next_cmd};
use serde_json::Value;

fn agent() -> AgentId {
    "w1".parse().unwrap()
}

/// A heartbeat that reports `reg` in a turn whose last event was at `last`.
async fn beat(fx: &Fx, reg: &Registered, last: Timestamp) {
    fx.req(CtlRequest::HostHeartbeat {
        host: name("h1"),
        sessions: vec![SessionLeaseRef {
            session: reg.session,
            agent: reg.agent.clone(),
            token: reg.token,
            idle_since: None,
            last_event_at: Some(last),
            pinned: false,
            activity: None,
        }],
        launching: Vec::new(),
        stats: Some(HostStats {
            version: "test".into(),
            sessions: vec![SessionStats {
                agent: reg.agent.clone(),
                busy: true,
                paused: false,
                turn: 1,
                last_event_at: Some(last),
                wakes: 0,
                wake_limited: 0,
                ack_failed: 0,
                append_failed: 0,
                redelivered: 0,
                bad_envelope: 0,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
            }],
            ..Default::default()
        }),
    })
    .await
    .unwrap();
}

/// The `hung` flag of the agent on the live view, as `borant status` reads it.
async fn hung(fx: &Fx) -> Option<Value> {
    let v = fx.call(None, ToolCall::View).await.unwrap();
    let agents = v["agents"].as_array()?;
    agents
        .iter()
        .find(|a| a["agent"] == agent().as_str())
        .and_then(|a| a.get("hung"))
        .filter(|h| !h.is_null())
        .cloned()
}

async fn alert_keys(fx: &Fx) -> Vec<String> {
    fx.state
        .list("alert.")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect()
}

/// Rule: a host that comes back from a gap of `hung_minutes` and a session
/// that said nothing since is hung; the view flags it, the alert names the
/// restart, and both go when the session moves.
#[tokio::test]
async fn a_sleep_leaves_a_hung_agent_on_the_view_and_the_alert() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let reg = fx.register("h1", Some("w1"), None).await;
    let start = fx.clock.now();
    beat(&fx, &reg, start).await;
    assert!(hung(&fx).await.is_none(), "a busy turn that just spoke");

    // The lid closes: 25 minutes without a heartbeat, and the session says
    // nothing when the host comes back.
    fx.advance(25 * 60);
    beat(&fx, &reg, start).await;
    let flag = hung(&fx).await.expect("the agent is hung after the sleep");
    assert_eq!(flag["host_slept"], true);

    fx.hub.tick().await.unwrap();
    assert_eq!(alert_keys(&fx).await, vec!["alert.agent_hung.w1"]);
    let notices = fx.recorder.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].title, "An agent looks hung");
    assert!(
        notices[0].body.ends_with("run `borant restart w1`"),
        "{}",
        notices[0].body
    );

    // Ten minutes more: the quiet turn is past the stall threshold too, and
    // the stall rule leaves it to the hung alert.
    fx.advance(10 * 60);
    beat(&fx, &reg, start).await;
    fx.hub.tick().await.unwrap();
    assert_eq!(alert_keys(&fx).await, vec!["alert.agent_hung.w1"]);

    // The session moves again: the flag goes, and the alert resolves.
    beat(&fx, &reg, fx.clock.now()).await;
    assert!(hung(&fx).await.is_none());
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    assert!(alert_keys(&fx).await.is_empty());
}

/// Rule: a stop of a hung agent ends its session now. A stop of a healthy one
/// waits for its turn end, so the order here is the stop and not a fence.
#[tokio::test]
async fn a_stop_ends_a_hung_session_now() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let reg = fx.register("h1", Some("w1"), None).await;
    let mut cmds = fx.commands("h1").await;
    let start = fx.clock.now();
    beat(&fx, &reg, start).await;
    fx.advance(25 * 60);
    beat(&fx, &reg, start).await;
    assert!(hung(&fx).await.is_some(), "the session looks hung");

    fx.call(
        None,
        ToolCall::Stop {
            selector: PauseSelector::Agent(agent()),
            grace_secs: Some(120),
        },
    )
    .await
    .unwrap();
    match next_cmd(&mut cmds).await {
        Some(HostCommand::Stop { session }) => assert_eq!(session, reg.session),
        other => panic!("the hub sent {other:?} to the host"),
    }

    // No fence waits behind it: the turn has no end to wait for.
    fx.advance(121);
    fx.hub.tick().await.unwrap();
    assert!(next_cmd(&mut cmds).await.is_none());
}
