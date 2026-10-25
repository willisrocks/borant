mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use borant_core::model::{Envelope, Harness, HostInfo, Target};
use borant_core::ports::MessageBus;
use borant_core::proto::{CtlRequest, HostCommand, LocalRegister, LocalSession, ToolCall};
use borant_core::subjects;
use borant_host::LocalClient;
use borant_host::client::Outcome;
use common::Fx;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn serve(fx: &Fx) -> (LocalClient, CancellationToken) {
    let socket = fx.dir.path().join("host.sock");
    let stop = CancellationToken::new();
    let host = fx.host.clone();
    let s = socket.clone();
    let st = stop.clone();
    tokio::spawn(async move { borant_host::api::serve(host, &s, st).await.unwrap() });
    for _ in 0..50 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (LocalClient::new(socket), stop)
}

fn ok<T>(o: Outcome<T>) -> T {
    match o {
        Outcome::Reply(Ok(v)) => v,
        Outcome::Reply(Err(e)) => panic!("api error: {e}"),
        Outcome::Down(e) => panic!("daemon down: {e}"),
    }
}

async fn register(c: &LocalClient, fx: &Fx, agent: &str, pid: Option<u32>) -> LocalSession {
    let cwd = fx.dir.path().join(agent);
    std::fs::create_dir_all(&cwd).unwrap();
    ok(c.post(
        "/v1/sessions",
        &LocalRegister {
            harness: Harness::Fake,
            harness_session_id: Some(format!("native-{agent}")),
            pid,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent.parse().unwrap()),
            channel: None,
            role: None,
            collection: None,
        },
        Duration::from_secs(5),
    )
    .await)
}

#[tokio::test]
async fn health_ok_and_socket_mode_is_0600() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let socket = c.socket().clone();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let health: Value = ok(c.get("/v1/health", Duration::from_secs(2)).await);
    assert_eq!(health["host"], "h1");
}

#[tokio::test]
async fn daemon_down_is_reported_fast() {
    let c = LocalClient::new("/tmp/borant-no-such.sock".into());
    let start = std::time::Instant::now();
    let r = c.get::<Value>("/v1/health", Duration::from_secs(2)).await;
    assert!(matches!(r, Outcome::Down(_)));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn register_then_wait_returns_direct_message() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let a = register(&c, &fx, "a", None).await;
    let b = register(&c, &fx, "b", None).await;
    let waiter = {
        let c = c.clone();
        let path = format!("/v1/sessions/{}/wait?timeout_ms=5000", b.session);
        tokio::spawn(async move { c.get::<Value>(&path, Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _: Value = ok(c
        .post(
            "/v1/call",
            &json!({"session": a.session, "call": {"tool": "send", "to": {"type": "agent", "id": "b"}, "text": "hello b"}}),
            Duration::from_secs(5),
        )
        .await);
    let got = ok(waiter.await.unwrap());
    let msgs: Vec<Envelope> = serde_json::from_value(got["messages"].clone()).unwrap();
    assert_eq!(msgs[0].text, "hello b");
}

#[tokio::test]
async fn resolve_finds_session_by_pid_or_native_id() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let a = register(&c, &fx, "a", Some(std::process::id())).await;
    let by_pid: LocalSession = ok(c
        .post(
            "/v1/resolve",
            &json!({"ancestors": [1, std::process::id()]}),
            Duration::from_secs(2),
        )
        .await);
    assert_eq!(by_pid.session, a.session);
    let by_native: LocalSession = ok(c
        .post(
            "/v1/resolve",
            &json!({"harness_session_id": "native-a"}),
            Duration::from_secs(2),
        )
        .await);
    assert_eq!(by_native.session, a.session);
}

#[tokio::test]
async fn stream_emits_sse_message() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let b = register(&c, &fx, "b", None).await;
    let socket = c.socket().clone();
    let path = format!("/v1/sessions/{}/stream", b.session);
    let reader = tokio::spawn(async move {
        use http_body_util::BodyExt;
        let stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let (mut tx, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(conn);
        let req = hyper::Request::get(path)
            .header("host", "borant")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();
        let mut body = tx.send_request(req).await.unwrap().into_body();
        let mut text = String::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.unwrap().into_data() {
                text.push_str(&String::from_utf8_lossy(&data));
                if text.contains("via sse") {
                    return text;
                }
            }
        }
        text
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let env = borant_core::testing::envelope(Target::Agent("b".parse().unwrap()), "via sse");
    fx.bus
        .publish(&subjects::agent(&"b".parse().unwrap()), &env)
        .await
        .unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), reader)
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("event: message"), "{text}");
}

#[tokio::test]
async fn prepare_move_on_idle_session_sends_move_ready() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let a = register(&c, &fx, "a", None).await;
    // The hub refuses a move that no host can take, so h1 must be known.
    fx.hub
        .handle(CtlRequest::RegisterHost {
            info: HostInfo {
                name: "h1".parse().unwrap(),
                harnesses: vec![Harness::Fake],
                launcher: "process".into(),
                max_agents: 4,
                always_on: false,
                draining: false,
                ssh: None,
                members: Default::default(),
                pool: None,
                tags: Vec::new(),
                bundles: Vec::new(),
            },
        })
        .await
        .into_result()
        .unwrap();
    // The hub sends PrepareMove to h1 and waits for MoveReady.
    let _: Value = ok(c
        .post(
            "/v1/call",
            &json!({"call": {"tool": "move", "selector": {"type": "agent", "value": "a"}, "to": {"host": null, "harness": null}}}),
            Duration::from_secs(5),
        )
        .await);
    fx.host
        .handle_command(HostCommand::PrepareMove {
            agent: "a".parse().unwrap(),
            session: a.session,
        })
        .await;
    // The session is gone locally, and the hub released the lease.
    assert!(fx.host.local_view(a.session).is_err());
    let agents = fx
        .hub
        .handle(CtlRequest::Call {
            actor: None,
            call: ToolCall::ListAgents,
        })
        .await
        .into_result()
        .unwrap();
    let lease = &agents[0]["lease"]["state"];
    assert_ne!(lease, "held", "{agents}");
}

#[tokio::test]
async fn second_daemon_on_same_socket_is_refused() {
    let fx = Fx::new(true, |_| {}).await;
    let (c, _stop) = serve(&fx).await;
    let err = borant_host::api::serve(fx.host.clone(), c.socket(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("another host daemon"), "{err}");
    // The first daemon still answers.
    let health: Value = ok(c.get("/v1/health", Duration::from_secs(2)).await);
    assert_eq!(health["host"], "h1");
}
