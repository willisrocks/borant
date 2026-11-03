//! The activity of a session that the host reports with the lease renewal:
//! the last tool call, the tool that runs now, and the last text. The text is
//! redacted on the host, and the renewal carries it at most once each 30 s.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{Harness, SwarmView};
use borant_core::proto::{CtlRequest, LocalRegister, LocalState, ToolCall};
use borant_core::{ManualClock, Timestamp};
use borant_host::client::Outcome;
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig, LocalClient};
use borant_hub::{Hub, HubConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A host and a hub on the same memory bus, served on the bus, and the local
/// API of the host. The host runs on `clock`, so a test moves its time; the
/// hub keeps its own clock, so a lease still renews.
async fn fixture(
    dir: &Path,
    clock: &Arc<ManualClock>,
    bus: &MemoryBus,
) -> (Host, Arc<Hub>, LocalClient) {
    let sessions = Arc::new(MemorySession::new());
    let hub = Arc::new(Hub::new(
        Arc::new(MemoryState::new()),
        Arc::new(bus.clone()),
        sessions.clone(),
        Arc::new(ManualClock::new(Timestamp(1_000_000))),
        HubConfig::default(),
    ));
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.hub_timeout = Duration::from_millis(500);
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        sessions,
        clock.clone(),
        Box::new(ProcessLauncher::new(dir.join("logs"))),
        None,
        None,
        Outbox::in_memory().expect("an in-memory outbox"),
    );
    common::serve_hub(bus, hub.clone()).await;
    // The local API, as a harness reaches it. The server runs in its own task
    // for the life of the test.
    let socket = dir.join("host.sock");
    let stop = CancellationToken::new();
    let serving = host.clone();
    let s = socket.clone();
    let st = stop.clone();
    tokio::spawn(async move {
        let _ = borant_host::api::serve(serving, &s, st).await;
    });
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (host, hub, LocalClient::new(socket))
}

/// One event as a hook posts it to the local API, in the wire form of
/// `LocalEvent`.
async fn event(client: &LocalClient, session: borant_core::SessionId, ev: Value) {
    let r: Value = match client
        .post(
            &format!("/v1/sessions/{session}/events"),
            &ev,
            Duration::from_secs(5),
        )
        .await
    {
        Outcome::Reply(Ok(v)) => v,
        Outcome::Reply(Err(e)) => panic!("api error: {e}"),
        Outcome::Down(e) => panic!("daemon down: {e}"),
    };
    assert!(r["fenced"].is_null());
}

async fn register(host: &Host, dir: &Path, agent: &str) -> borant_core::proto::LocalSession {
    let cwd = dir.join(agent);
    std::fs::create_dir_all(&cwd).unwrap();
    let s = host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent.parse().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(s.state, LocalState::Attached, "{s:?}");
    s
}

async fn view(hub: &Hub) -> SwarmView {
    let v = hub
        .handle(CtlRequest::Call {
            actor: None,
            key: None,
            call: ToolCall::View,
        })
        .await
        .into_result()
        .unwrap();
    serde_json::from_value(v).expect("the view parses")
}

/// The status of one agent in the view.
fn status_of(
    view: &SwarmView,
    session: borant_core::SessionId,
) -> &borant_core::model::AgentStatus {
    view.agents
        .iter()
        .find(|a| a.session == Some(session))
        .expect("the agent is in the view")
}

#[tokio::test]
async fn activity_from_the_hooks_reaches_the_hub_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let bus = MemoryBus::new();
    let (host, hub, client) = fixture(dir.path(), &clock, &bus).await;
    let w1 = register(&host, dir.path(), "w1").await;
    // A second session that no hook of the activity has ever seen.
    let w2 = register(&host, dir.path(), "w2").await;

    // PreToolUse: a Bash call that names a token. Its result has not arrived,
    // so the tool runs now.
    event(
        &client,
        w1.session,
        json!({
            "event": "tool_start",
            "name": "Bash",
            "input": r#"{"command":"curl -H 'Authorization: Bearer sk-live-0123456789abcdef' https://api"}"#,
        }),
    )
    .await;
    // The Stop hook: the last assistant text, which names another token.
    event(
        &client,
        w1.session,
        json!({
            "event": "assistant_message",
            "text": format!(
                "{} the token ghp_0123456789abcdefghij is in .env",
                "x".repeat(200)
            ),
        }),
    )
    .await;
    host.heartbeat_once().await;

    let v = view(&hub).await;
    let a = status_of(&v, w1.session);
    assert_eq!(a.session, Some(w1.session));
    assert_eq!(a.last_tool_at, Some(Timestamp(1_000_000)));
    let tool = a.last_tool.as_deref().expect("the tool of the last call");
    assert!(tool.starts_with("Bash "), "{tool}");
    assert!(!tool.contains("sk-live-"), "{tool}");
    assert!(tool.contains("[redacted]"), "{tool}");
    assert_eq!(a.tool_running_since, Some(Timestamp(1_000_000)));
    assert_eq!(a.last_text_at, Some(Timestamp(1_000_000)));
    let text = a.last_text.as_deref().expect("the last text");
    assert_eq!(text.chars().count(), 120, "{text}");
    assert!(!text.contains("ghp_"), "{text}");

    // No report leaves the host with a token in it.
    let json = serde_json::to_string(&v).unwrap();
    assert!(!json.contains("sk-live-"), "{json}");
    assert!(!json.contains("ghp_0123456789"), "{json}");

    // A session without a hook has no data, which the mod reads as no data
    // rather than as silence.
    let b = status_of(&v, w2.session);
    assert_eq!(b.session, Some(w2.session));
    assert_eq!(b.last_tool_at, None);
    assert_eq!(b.last_tool, None);
    assert_eq!(b.last_text_at, None);
    assert_eq!(b.last_text, None);
    assert_eq!(b.tool_running_since, None);
}

#[tokio::test]
async fn activity_goes_out_once_each_thirty_seconds_and_a_result_ends_the_tool() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
    let bus = MemoryBus::new();
    let (host, hub, client) = fixture(dir.path(), &clock, &bus).await;
    let w1 = register(&host, dir.path(), "w1").await;

    event(
        &client,
        w1.session,
        json!({
            "event": "tool_start",
            "name": "Read",
            "input": r#"{"file_path":"/work/src/lib.rs"}"#,
        }),
    )
    .await;
    host.heartbeat_once().await;
    let v = view(&hub).await;
    assert_eq!(
        status_of(&v, w1.session).tool_running_since,
        Some(Timestamp(1_000_000))
    );

    // PostToolUse: the tool ended, but the renewal sent its report under 30 s
    // ago, so the next beat carries nothing.
    event(
        &client,
        w1.session,
        json!({
            "event": "tool_result",
            "name": "Read",
            "output": "fn main() {}",
            "is_error": false,
        }),
    )
    .await;
    host.heartbeat_once().await;
    let v = view(&hub).await;
    assert_eq!(
        status_of(&v, w1.session).tool_running_since,
        Some(Timestamp(1_000_000)),
        "the report goes out at most once each 30 s"
    );

    // Past the window, the next renewal says that nothing runs now, and the
    // last call keeps its time.
    clock.advance(Duration::from_secs(31));
    host.heartbeat_once().await;
    let v = view(&hub).await;
    let a = status_of(&v, w1.session);
    assert_eq!(a.tool_running_since, None);
    assert_eq!(a.last_tool_at, Some(Timestamp(1_000_000)));
    assert_eq!(a.last_tool.as_deref(), Some("Read /work/src/lib.rs"));
}
