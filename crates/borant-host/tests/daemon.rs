mod common;

use std::time::Duration;

use borant_core::model::{Harness, SessionEventBody, Target};
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

#[tokio::test]
async fn register_when_hub_down_detaches_then_attaches() {
    let mut fx = Fx::new(false, |_| {}).await;
    let cwd = fx.dir.path().to_string_lossy().into_owned();
    let s = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            harness_session_id: None,
            pid: None,
            cwd,
            agent: Some(agent("a")),
            channel: None,
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
async fn dead_pid_ends_session() {
    let fx = Fx::new(true, |_| {}).await;
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let s = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            harness_session_id: None,
            pid: Some(pid),
            cwd: fx.dir.path().to_string_lossy().into_owned(),
            agent: Some(agent("a")),
            channel: None,
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
