mod common;

use std::time::Duration;

use borant_core::model::{ChannelKind, Harness, SessionEventBody, Target};
use borant_core::ports::{MessageBus, SessionStore};
use borant_core::proto::{ApiErrorCode, LocalEvent, LocalRegister, LocalState, ToolCall};
use borant_core::{AgentId, ChannelName, EventId, SessionId, Timestamp, subjects, testing};
use common::{Fx, eventually, serve_hub};

fn agent(s: &str) -> AgentId {
    s.parse().unwrap()
}

fn sid(s: &borant_core::proto::LocalSession) -> SessionId {
    s.session
}

async fn send(fx: &Fx, from: &borant_core::proto::LocalSession, to: Target, text: &str) {
    fx.host
        .call(
            Some(sid(from)),
            ToolCall::Send {
                to,
                text: text.into(),
                urgent: false,
            },
        )
        .await
        .unwrap();
}

const HUB_BACK: &str = "Borant is back. Your tools work again. Continue your task.";

/// Counts the hub-is-back messages in the session log of `agent`.
async fn wake_messages(fx: &Fx, agent: &AgentId) -> usize {
    fx.sessions
        .read(agent)
        .await
        .unwrap()
        .iter()
        .filter(|(_, e)| {
            matches!(
                &e.body,
                SessionEventBody::SwarmMessage { envelope } if envelope.text == HUB_BACK
            )
        })
        .count()
}

/// The reason the tools of a session are blocked, if any.
async fn fence(fx: &Fx, session: SessionId) -> Option<String> {
    fx.host
        .event(session, LocalEvent::TurnStart)
        .await
        .unwrap()
        .fenced
}

#[tokio::test]
async fn register_when_hub_down_detaches_then_attaches() {
    let mut fx = Fx::new(false, |_| {}).await;
    let cwd = fx.dir.path().to_string_lossy().into_owned();
    let s = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd,
            agent: Some(agent("a")),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(s.state, LocalState::Detached);
    let err = fx
        .host
        .call(Some(s.session), ToolCall::Whoami)
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Unavailable);
    fx.hub_stop = Some(serve_hub(&fx.bus, fx.hub.clone()).await);
    let host = fx.host.clone();
    eventually(
        || {
            let host = host.clone();
            async move { host.local_view(s.session).unwrap().state == LocalState::Attached }
        },
        "attach",
    )
    .await;
    assert!(
        fx.host
            .call(Some(s.session), ToolCall::Whoami)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn direct_message_wakes_waiter() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let b = fx.session("b", "wb").await;
    let waiter = {
        let host = fx.host.clone();
        tokio::spawn(async move { host.wait(b.session, Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(&fx, &a, Target::Agent(agent("b")), "ping").await;
    let got = waiter.await.unwrap().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "ping");
}

#[tokio::test]
async fn broadcast_is_delivered_on_next_event_not_wake() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let b = fx.session("b", "wb").await;
    let api: ChannelName = "api".parse().unwrap();
    fx.host
        .call(
            Some(a.session),
            ToolCall::ChannelCreate {
                name: api.clone(),
                schema: None,
                project: None,
                branch: None,
                kind: ChannelKind::Project,
                title: None,
                issue: None,
            },
        )
        .await
        .unwrap();
    fx.host
        .call(Some(b.session), ToolCall::ChannelJoin { name: api.clone() })
        .await
        .unwrap();
    send(&fx, &a, Target::Channel(api), "status update").await;
    let none = fx
        .host
        .wait(b.session, Duration::from_millis(700))
        .await
        .unwrap();
    assert!(none.is_empty(), "a broadcast must not wake");
    let reply = fx
        .host
        .event(
            b.session,
            LocalEvent::ToolResult {
                name: "bash".into(),
                output: "ok".into(),
                is_error: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.messages.len(), 1);
    assert_eq!(reply.messages[0].text, "status update");
    // The sender does not get its own broadcast back.
    let mine = fx
        .host
        .event(a.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert!(mine.messages.iter().all(|m| m.text != "status update"));
}

#[tokio::test]
async fn received_message_is_logged_before_ack() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let _b = fx.session("b", "wb").await;
    send(&fx, &a, Target::Agent(agent("b")), "logged").await;
    let sessions = fx.sessions.clone();
    eventually(
        || {
            let sessions = sessions.clone();
            async move {
                sessions.read(&agent("b")).await.unwrap().iter().any(|(_, e)| {
                    matches!(&e.body, SessionEventBody::SwarmMessage { envelope } if envelope.text == "logged")
                })
            }
        },
        "the message in the log",
    )
    .await;
    assert!(fx.bus.unacked(&agent("b")).await.unwrap().is_empty());
}

#[tokio::test]
async fn message_already_in_log_is_dropped() {
    let fx = Fx::new(true, |_| {}).await;
    let env = testing::envelope(Target::Agent(agent("b")), "old");
    // A previous session of b already received this message.
    fx.sessions
        .append(
            &agent("b"),
            &borant_core::model::SessionEvent {
                id: EventId::new(),
                agent: agent("b"),
                session: None,
                harness: Harness::Fake,
                host: "h0".parse().unwrap(),
                turn: 1,
                at: Timestamp(0),
                body: SessionEventBody::SwarmMessage {
                    envelope: env.clone(),
                },
            },
        )
        .await
        .unwrap();
    fx.sessions
        .append(
            &agent("b"),
            &borant_core::model::SessionEvent {
                id: EventId::new(),
                agent: agent("b"),
                session: None,
                harness: Harness::Fake,
                host: "h0".parse().unwrap(),
                turn: 1,
                at: Timestamp(0),
                body: SessionEventBody::Delivered { message: env.id },
            },
        )
        .await
        .unwrap();
    let b = fx.session("b", "wb").await;
    fx.bus
        .publish(&subjects::agent(&agent("b")), &env)
        .await
        .unwrap();
    let got = fx
        .host
        .wait(b.session, Duration::from_millis(700))
        .await
        .unwrap();
    assert!(got.is_empty(), "{got:?}");
}

#[tokio::test]
async fn oversized_message_is_rejected() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let err = fx
        .host
        .call(
            Some(a.session),
            ToolCall::Send {
                to: Target::Agent(agent("b")),
                text: "x".repeat(64 * 1024 + 1),
                urgent: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid);
    assert!(err.message.contains("65537"), "{}", err.message);
}

#[tokio::test]
async fn tool_output_is_cut_and_redacted() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let output = format!("token=abc123secret {}", "y".repeat(10_000));
    fx.host
        .event(
            a.session,
            LocalEvent::ToolResult {
                name: "bash".into(),
                output,
                is_error: false,
            },
        )
        .await
        .unwrap();
    let events = fx.sessions.read(&agent("a")).await.unwrap();
    let out = events
        .iter()
        .find_map(|(_, e)| match &e.body {
            SessionEventBody::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!out.contains("abc123secret"));
    assert!(out.contains("bytes removed]"), "{}", &out[out.len() - 40..]);
    assert!(out.len() < 8300);
}

#[tokio::test]
async fn lost_lease_fences_session() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    fx.hub_clock.advance(Duration::from_secs(31));
    fx.hub.tick().await.unwrap();
    // A new session takes the expired agent.
    fx.session("a", "wb").await;
    fx.host.heartbeat_once().await;
    let reply = fx
        .host
        .event(
            a.session,
            LocalEvent::ToolCall {
                name: "bash".into(),
                input: "ls".into(),
            },
        )
        .await
        .unwrap();
    let reason = reply.fenced.expect("fenced");
    assert!(reason.contains("lost its lease"), "{reason}");
}

#[tokio::test]
async fn expired_lease_with_no_takeover_is_reclaimed() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let token = |fx: &Fx| -> u64 {
        let table: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fx.dir.path().join("sessions.json")).unwrap())
                .unwrap();
        table[0]["reg"]["token"].as_u64().unwrap()
    };
    let before = token(&fx);
    fx.hub_clock.advance(Duration::from_secs(31));
    fx.hub.tick().await.unwrap();
    fx.host.heartbeat_once().await;
    assert_eq!(token(&fx), before + 1, "the new token is saved");
    let reply = fx
        .host
        .event(a.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(reply.fenced, None);
    // A second expiry works only if the host sends the new token.
    fx.hub_clock.advance(Duration::from_secs(31));
    fx.hub.tick().await.unwrap();
    fx.host.heartbeat_once().await;
    assert_eq!(token(&fx), before + 2);
    let reply = fx
        .host
        .event(a.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(reply.fenced, None);
}

#[tokio::test]
async fn no_heartbeat_for_lease_pauses_then_resumes() {
    let mut fx = Fx::new(true, |c| c.lease = Duration::from_millis(300)).await;
    let a = fx.session("a", "wa").await;
    fx.host.heartbeat_once().await;
    fx.hub_stop.take().unwrap().cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    let paused = fx
        .host
        .event(a.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert!(paused.fenced.unwrap().contains("unreachable"));
    fx.hub_stop = Some(serve_hub(&fx.bus, fx.hub.clone()).await);
    fx.host.heartbeat_once().await;
    let back = fx
        .host
        .event(a.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(back.fenced, None);
}

#[tokio::test]
async fn hub_return_wakes_paused_sessions_once() {
    let mut fx = Fx::new(true, |c| c.lease = Duration::from_millis(300)).await;
    let a = fx.session("a", "wa").await;
    fx.host.heartbeat_once().await;

    // The hub is gone for longer than the lease: the session pauses.
    fx.hub_stop.take().unwrap().cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    assert!(fence(&fx, a.session).await.unwrap().contains("unreachable"));

    // A brief success is not a stable recovery: the tools work again, but
    // the session is not woken into a hub that may go away again.
    fx.hub_stop = Some(serve_hub(&fx.bus, fx.hub.clone()).await);
    fx.host.heartbeat_once().await;
    assert_eq!(fence(&fx, a.session).await, None);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(wake_messages(&fx, &agent("a")).await, 0);

    // The hub flaps: down past the lease again, then up.
    fx.hub_stop.take().unwrap().cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    assert!(fence(&fx, a.session).await.unwrap().contains("unreachable"));
    fx.hub_stop = Some(serve_hub(&fx.bus, fx.hub.clone()).await);
    fx.host.heartbeat_once().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(wake_messages(&fx, &agent("a")).await, 0);

    // One wake per session, once the hub stays reachable for a lease. The
    // message goes through the normal delivery path.
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    let woken = fx
        .host
        .wait(a.session, Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        woken.iter().any(|e| e.text == HUB_BACK),
        "the session did not wake: {woken:?}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(wake_messages(&fx, &agent("a")).await, 1);

    // A session that the pause did not catch gets nothing.
    let b = fx.session("b", "wb").await;
    fx.host.heartbeat_once().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wake_messages(&fx, &agent("b")).await, 0);

    // A new outage after the full recovery wakes every session once.
    fx.hub_stop.take().unwrap().cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    fx.hub_stop = Some(serve_hub(&fx.bus, fx.hub.clone()).await);
    fx.host.heartbeat_once().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(wake_messages(&fx, &agent("a")).await, 2);
    assert_eq!(wake_messages(&fx, &agent("b")).await, 1);
    assert_eq!(fence(&fx, b.session).await, None);
}

#[tokio::test]
async fn dead_pid_ends_session() {
    let fx = Fx::new(true, |_| {}).await;
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let s = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            pid: Some(pid),
            cwd: fx.dir.path().to_string_lossy().into_owned(),
            agent: Some(agent("a")),
            ..Default::default()
        })
        .await
        .unwrap();
    fx.host.check_liveness().await;
    assert!(fx.host.local_view(s.session).is_err());
}

#[tokio::test]
async fn outbox_holds_appends_while_store_down() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    fx.sessions.set_available(false);
    fx.host
        .event(
            a.session,
            LocalEvent::UserPrompt {
                text: "while down".into(),
            },
        )
        .await
        .unwrap();
    fx.sessions.set_available(true);
    let before = fx.sessions.read(&agent("a")).await.unwrap().len();
    fx.host.flush_outbox().await;
    let events = fx.sessions.read(&agent("a")).await.unwrap();
    assert!(events.len() > before);
    assert!(events.iter().any(
        |(_, e)| matches!(&e.body, SessionEventBody::UserPrompt { text } if text == "while down")
    ));
}

#[tokio::test]
async fn wake_limit_notifies_sender_once() {
    let fx = Fx::new(true, |c| c.wakes_per_hour = 2).await;
    let a = fx.session("a", "wa").await;
    let b = fx.session("b", "wb").await;
    for i in 0..4 {
        send(&fx, &a, Target::Agent(agent("b")), &format!("m{i}")).await;
    }
    let host = fx.host.clone();
    eventually(
        || {
            let host = host.clone();
            async move {
                let r = host
                    .event(
                        a.session,
                        LocalEvent::ToolCall {
                            name: "x".into(),
                            input: String::new(),
                        },
                    )
                    .await
                    .unwrap();
                r.messages
                    .iter()
                    .any(|m| m.text.contains("reached its wake limit"))
            }
        },
        "the wake-limit notice",
    )
    .await;
    let got = fx
        .host
        .event(b.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(got.messages.len(), 4, "all messages still arrive");
}

#[tokio::test]
async fn restart_restores_live_sessions() {
    let fx = Fx::new(true, |_| {}).await;
    let cwd = fx.dir.path().join("wr");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            harness_session_id: Some("native-r".into()),
            // This test process stands in for the harness process.
            pid: Some(std::process::id()),
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent("r")),
            ..Default::default()
        })
        .await
        .unwrap();
    // A second daemon on the same home, as after a restart.
    let again = borant_host::Host::new(
        fx.host.config().clone(),
        std::sync::Arc::new(fx.bus.clone()),
        std::sync::Arc::new(fx.sessions.clone()),
        std::sync::Arc::new(borant_core::SystemClock),
        Box::new(borant_launch::ProcessLauncher::new(
            fx.dir.path().join("logs"),
        )),
        None,
        None,
        borant_host::outbox::Outbox::in_memory().unwrap(),
    );
    again.restore().await;
    let view = again.local_view(s.session).unwrap();
    assert_eq!(view.state, LocalState::Attached);
    again.heartbeat_once().await;
    let reply = again.event(s.session, LocalEvent::TurnStart).await.unwrap();
    assert_eq!(reply.fenced, None, "the lease must still be valid");
}

#[tokio::test]
async fn turn_end_events_do_not_take_messages() {
    let fx = Fx::new(true, |_| {}).await;
    let a = fx.session("a", "wa").await;
    let b = fx.session("b", "wb").await;
    let api: ChannelName = "api".parse().unwrap();
    fx.host
        .call(
            Some(a.session),
            ToolCall::ChannelCreate {
                name: api.clone(),
                schema: None,
                project: None,
                branch: None,
                kind: ChannelKind::Project,
                title: None,
                issue: None,
            },
        )
        .await
        .unwrap();
    fx.host
        .call(Some(b.session), ToolCall::ChannelJoin { name: api.clone() })
        .await
        .unwrap();
    send(&fx, &a, Target::Channel(api), "for the next turn").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = fx
        .host
        .event(
            b.session,
            LocalEvent::AssistantMessage {
                text: "done".into(),
            },
        )
        .await
        .unwrap();
    assert!(r.messages.is_empty(), "the Stop hook cannot show messages");
    let r = fx.host.event(b.session, LocalEvent::TurnEnd).await.unwrap();
    assert!(r.messages.is_empty());
    let r = fx
        .host
        .event(b.session, LocalEvent::TurnStart)
        .await
        .unwrap();
    assert_eq!(r.messages.len(), 1);
}

#[tokio::test]
async fn paused_session_does_not_take_messages() {
    let mut fx = Fx::new(true, |c| c.lease = Duration::from_millis(300)).await;
    let b = fx.session("b", "wb").await;
    fx.host.heartbeat_once().await;
    fx.hub_stop.take().unwrap().cancel();
    tokio::time::sleep(Duration::from_millis(400)).await;
    fx.host.heartbeat_once().await;
    assert!(fx.host.local_view(b.session).unwrap().fenced.is_some());
    let env = testing::envelope(Target::Agent(agent("b")), "not for a paused session");
    fx.bus
        .publish(&subjects::agent(&agent("b")), &env)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let left = fx.bus.unacked(&agent("b")).await.unwrap();
    assert_eq!(
        left.len(),
        1,
        "the message stays in the inbox for the next holder"
    );
}

#[tokio::test]
async fn unknown_session_is_unavailable_until_the_restore_ends() {
    let fx = Fx::new(true, |_| {}).await;
    // Before the restore, the session can still come back, so callers such
    // as the Claude Code idle waiter must retry instead of giving up.
    let err = fx.host.local_view(SessionId::new()).unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Unavailable, "{err}");
    fx.host.restore().await;
    let err = fx.host.local_view(SessionId::new()).unwrap_err();
    assert_eq!(err.code, ApiErrorCode::NotFound, "{err}");
}
