//! The long-poll routes `GET /v1/wait/agent` and `GET /v1/wait/task`, against
//! the memory adapters: a host daemon on a Unix socket, and a hub that serves
//! its calls.

mod common;

use std::time::Duration;

use borant_core::model::{ChannelKind, CloseReason, NewTask};
use borant_core::proto::ApiErrorCode;
use borant_core::proto::ToolCall;
use borant_core::{ChannelName, SessionId, TaskId};
use borant_host::LocalClient;
use borant_host::client::Outcome;
use common::Fx;
use serde_json::{Value, json};

/// Starts the local API on a fresh socket, and returns a client for it.
async fn api(fx: &Fx) -> (LocalClient, tokio_util::sync::CancellationToken) {
    let socket = fx.dir.path().join("host.sock");
    let stop = tokio_util::sync::CancellationToken::new();
    let shutdown = stop.clone();
    let host = fx.host.clone();
    let path = socket.clone();
    tokio::spawn(async move {
        borant_host::api::serve(host, &path, shutdown)
            .await
            .expect("the test host serves the local API");
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (LocalClient::new(socket), stop)
}

/// A channel, and a task in it, created by a session.
async fn channel_and_task(fx: &Fx, channel: &str) -> (SessionId, TaskId) {
    let channel: ChannelName = channel.parse().unwrap();
    let session = fx.session("owner", "work").await;
    fx.host
        .call(
            Some(session.session),
            ToolCall::ChannelCreate {
                name: channel.clone(),
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
    let task = fx
        .host
        .call(
            Some(session.session),
            ToolCall::TaskCreate {
                channel,
                task: NewTask {
                    title: "ship it".into(),
                    ..NewTask::default()
                },
            },
        )
        .await
        .unwrap();
    (
        session.session,
        task["id"].as_str().unwrap().parse().unwrap(),
    )
}

async fn close_task(fx: &Fx, session: SessionId, channel: &str, id: TaskId, reason: CloseReason) {
    fx.host
        .call(
            Some(session),
            ToolCall::TaskClose {
                channel: channel.parse().unwrap(),
                id,
                reason,
                message: String::new(),
            },
        )
        .await
        .unwrap();
}

fn body(outcome: Outcome<Value>) -> Value {
    match outcome {
        Outcome::Reply(Ok(v)) => v,
        other => panic!("the route replied {other:?}"),
    }
}

#[tokio::test]
async fn wait_task_long_polls_until_the_close() {
    let fx = Fx::new(true, |_| {}).await;
    let (session, id) = channel_and_task(&fx, "api").await;
    let (client, _stop) = api(&fx).await;
    let path = format!("/v1/wait/task?channel=api&id={id}&timeout_ms=10000");

    let waiting =
        tokio::spawn(async move { client.get::<Value>(&path, Duration::from_secs(20)).await });
    // Let the route poll an open task before it closes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    close_task(&fx, session, "api", id, CloseReason::Done).await;

    let outcome = body(waiting.await.unwrap());
    assert_eq!(outcome["outcome"], "done", "{outcome}");
    assert_eq!(outcome["task"]["status"], "closed", "{outcome}");
    assert_eq!(outcome["task"]["close_reason"], "done", "{outcome}");
}

#[tokio::test]
async fn wait_task_reports_a_close_reason_other_than_done() {
    let fx = Fx::new(true, |_| {}).await;
    let (session, id) = channel_and_task(&fx, "api").await;
    close_task(&fx, session, "api", id, CloseReason::Wontfix).await;
    let (client, _stop) = api(&fx).await;

    let v = body(
        client
            .get::<Value>(
                &format!("/v1/wait/task?channel=api&id={id}&timeout_ms=1000"),
                Duration::from_secs(10),
            )
            .await,
    );
    assert_eq!(v["outcome"], "failed", "{v}");
    assert_eq!(v["task"]["close_reason"], "wontfix", "{v}");
}

#[tokio::test]
async fn wait_task_times_out_while_the_task_is_open() {
    let fx = Fx::new(true, |_| {}).await;
    let (_session, id) = channel_and_task(&fx, "api").await;
    let (client, _stop) = api(&fx).await;

    let started = tokio::time::Instant::now();
    let v = body(
        client
            .get::<Value>(
                &format!("/v1/wait/task?channel=api&id={id}&timeout_ms=150"),
                Duration::from_secs(10),
            )
            .await,
    );
    assert_eq!(v["outcome"], "timeout", "{v}");
    assert_eq!(v["task"]["status"], "open", "{v}");
    assert!(started.elapsed() >= Duration::from_millis(150), "it waited");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "and not too long"
    );
}

#[tokio::test]
async fn wait_task_reports_an_unknown_task() {
    let fx = Fx::new(true, |_| {}).await;
    let (_session, _id) = channel_and_task(&fx, "api").await;
    let (client, _stop) = api(&fx).await;
    let missing = TaskId::new();

    match client
        .get::<Value>(
            &format!("/v1/wait/task?channel=api&id={missing}&timeout_ms=1000"),
            Duration::from_secs(10),
        )
        .await
    {
        Outcome::Reply(Err(e)) => assert_eq!(e.code, ApiErrorCode::NotFound, "{e:?}"),
        other => panic!("the route replied {other:?} for a missing task"),
    }
}

#[tokio::test]
async fn wait_agent_times_out_while_the_lease_is_held() {
    let fx = Fx::new(true, |_| {}).await;
    fx.session("w1", "work").await;
    let (client, _stop) = api(&fx).await;

    let v = body(
        client
            .get::<Value>(
                "/v1/wait/agent?agent=w1&timeout_ms=150",
                Duration::from_secs(10),
            )
            .await,
    );
    assert_eq!(v["outcome"], "timeout", "{v}");
    assert_eq!(v["agent"], "w1", "{v}");
}

#[tokio::test]
async fn wait_agent_reports_an_agent_the_swarm_does_not_have() {
    let fx = Fx::new(true, |_| {}).await;
    fx.session("w1", "work").await;
    let (client, _stop) = api(&fx).await;

    let v = body(
        client
            .get::<Value>(
                "/v1/wait/agent?agent=nobody&timeout_ms=1000",
                Duration::from_secs(10),
            )
            .await,
    );
    assert_eq!(
        v,
        json!({"outcome": "done", "agent": "nobody", "why": "gone"})
    );
}

#[tokio::test]
async fn wait_agent_history_serves_the_local_log() {
    // `borant logs` reads the session log of the agents this host ran, from
    // the host itself: a human caller needs no session for it.
    let fx = Fx::new(true, |_| {}).await;
    fx.session("w1", "work").await;
    let v = fx
        .host
        .call(
            None,
            ToolCall::AgentHistory {
                agent: "w1".parse().unwrap(),
                page: 0,
                page_size: 50,
            },
        )
        .await
        .unwrap();
    assert_eq!(v["agent"], "w1", "{v}");
    let events = v["events"].as_array().unwrap();
    assert!(!events.is_empty(), "{v}");
    assert!(
        events.iter().any(|e| e["kind"] == json!("session_start")),
        "{v}"
    );
}
