//! The tests of the read-only listener: the router over the memory adapters,
//! the page, the JSON API, the event stream, and the gauges.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use borant_core::events::{Severity, SwarmEvent, SwarmEventBody};
use borant_core::model::{
    Agent, AgentStatus, Channel, ChannelKind, HostInfo, HostStats, HostStatus, HubLease,
    InboxStatus, Lease, LeaseState, SwarmView, Task, TaskKind, TaskStatus,
};
use borant_core::ports::{Change, Entry, EventLog, InboxStats, PortError, PortResult, StateStore};
use borant_core::{AgentId, ChannelName, ManualClock, SessionId, TaskId, Timestamp, keys};
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession, MemoryState};
use bytes::Bytes;
use futures::stream::BoxStream;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::*;
use crate::HubConfig;

const NOW: Timestamp = Timestamp(1_000_000_000);

/// A store that records any write. The listener must never make one.
#[derive(Debug)]
struct ReadOnly {
    inner: MemoryState,
    writes: Arc<AtomicUsize>,
}

#[async_trait]
impl StateStore for ReadOnly {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>> {
        self.inner.get(key).await
    }

    async fn create(&self, _key: &str, _value: Bytes) -> PortResult<u64> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(PortError::other("the HTTP listener wrote to the store"))
    }

    async fn update(&self, _key: &str, _value: Bytes, _expected: u64) -> PortResult<u64> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(PortError::other("the HTTP listener wrote to the store"))
    }

    async fn delete(&self, _key: &str, _expected: Option<u64>) -> PortResult<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Err(PortError::other("the HTTP listener wrote to the store"))
    }

    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>> {
        self.inner.list(prefix).await
    }

    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>> {
        self.inner.watch(prefix).await
    }
}

struct Fx {
    hub: Arc<Hub>,
    state: MemoryState,
    log: Arc<MemoryEventLog>,
    writes: Arc<AtomicUsize>,
    agent: AgentId,
    channel: ChannelName,
    task: TaskId,
}

/// A task of the fixture channel, with the fields that the API reads.
fn task(id: TaskId, status: TaskStatus, blocked_by: Vec<TaskId>) -> Task {
    Task {
        id,
        title: "the task".into(),
        body: String::new(),
        status,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by,
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        properties: Default::default(),
        created: NOW,
        updated: NOW,
        create_key: None,
    }
}

impl Fx {
    /// A hub with one host, one agent on it, one channel, one open task, and
    /// the hub lease of `hub-test`.
    fn new() -> Self {
        let state = MemoryState::new();
        let writes = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(MemoryEventLog::new());
        let clock = Arc::new(ManualClock::new(NOW));

        let host: borant_core::HostName = "h1".parse().unwrap();
        let agent: AgentId = "w1".parse().unwrap();
        let channel: ChannelName = "roadmap".parse().unwrap();
        let first = TaskId::new();

        let info = HostInfo {
            name: host.clone(),
            max_agents: 4,
            always_on: true,
            launcher: "tmux".into(),
            ..HostInfo::default()
        };
        state.seed(&keys::host(&host), bytes(&info));
        state.seed(
            &keys::agent(&agent),
            bytes(&Agent {
                id: agent.clone(),
                host: Some(host.clone()),
                bundles: Vec::new(),
                ..Agent::default()
            }),
        );
        state.seed(
            &keys::lease_agent(&agent),
            bytes(&Lease {
                agent: agent.clone(),
                token: borant_core::FencingToken(1),
                state: LeaseState::Held {
                    session: SessionId::new(),
                    host: host.clone(),
                    expires: NOW.plus(Duration::from_secs(60)),
                },
            }),
        );
        state.seed(
            &keys::channel(&channel),
            bytes(&Channel {
                name: channel.clone(),
                owner: Some(agent.clone()),
                schema: json!({}),
                created: NOW,
                closed: None,
                project: None,
                branch: None,
                collection: None,
                paused: false,
                kind: ChannelKind::Project,
                title: None,
                issue: None,
                gate: None,
                root: None,
            }),
        );
        state.seed(
            &keys::task(&channel, &first),
            bytes(&task(first, TaskStatus::Open, Vec::new())),
        );
        state.seed(
            &keys::lease_hub(),
            bytes(&HubLease {
                holder: "hub-test".into(),
                token: borant_core::FencingToken(1),
                expires: NOW.plus(Duration::from_secs(60)),
            }),
        );

        let hub = Hub::new(
            Arc::new(ReadOnly {
                inner: state.clone(),
                writes: writes.clone(),
            }),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            clock,
            HubConfig {
                instance: "hub-test".into(),
                ..HubConfig::default()
            },
        )
        .with_events(log.clone());
        Fx {
            hub: Arc::new(hub),
            state,
            log,
            writes,
            agent,
            channel,
            task: first,
        }
    }

    /// One GET, as a browser makes it, without reading the body.
    async fn response(&self, uri: &str) -> axum::response::Response {
        let request = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("a request");
        router(self.hub.clone(), NOW)
            .oneshot(request)
            .await
            .expect("the router answers")
    }

    /// One GET, with its body read to the end. Only for a route that ends
    /// its body: the event stream follows.
    async fn get(&self, uri: &str) -> (StatusCode, String) {
        let response = self.response(uri).await;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("a body")
            .to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }
}

fn bytes<T: serde::Serialize>(value: &T) -> Bytes {
    Bytes::from(serde_json::to_vec(value).expect("a record"))
}

/// The names of the gauges of the spec, and no other.
const GAUGES: [&str; 13] = [
    "borant_hub_start_time_seconds",
    "borant_host_online",
    "borant_host_heartbeat_age_seconds",
    "borant_host_load1",
    "borant_host_mem_avail_bytes",
    "borant_host_disk_used_ratio",
    "borant_host_agents",
    "borant_host_max_agents",
    "borant_agent_lease_state",
    "borant_inbox_pending",
    "borant_inbox_redelivered",
    "borant_tasks",
    "borant_spawn_queue_length",
];

/// A view with one online host, one agent that holds its lease, and one
/// inbox, at the scrape time `NOW`.
fn metrics_view() -> SwarmView {
    let info = HostInfo {
        name: "h1".parse().unwrap(),
        max_agents: 4,
        ..HostInfo::default()
    };
    let stats = HostStats {
        load1: 0.5,
        mem_avail_mb: Some(512),
        disk_used_pct: Some(25.0),
        ..HostStats::default()
    };
    SwarmView {
        hosts: vec![HostStatus {
            info,
            online: true,
            last_seen: Some(NOW.minus(Duration::from_secs(30))),
            stats: Some(stats),
        }],
        agents: vec![AgentStatus {
            agent: "w1".parse().unwrap(),
            harness: None,
            host: Some("h1".parse().unwrap()),
            lease: Some(LeaseState::Held {
                session: SessionId::new(),
                host: "h1".parse().unwrap(),
                expires: NOW.plus(Duration::from_secs(60)),
            }),
            session: None,
            busy: false,
            claim: None,
            role: None,
            collection: None,
            pinned: false,
            retired: false,
            model: None,
        }],
        inboxes: vec![InboxStatus {
            agent: "w1".parse().unwrap(),
            stats: InboxStats {
                pending: 3,
                ack_pending: 1,
                redelivered: 1,
            },
        }],
        at: Some(NOW),
    }
}

fn metrics_tasks() -> Vec<(ChannelName, Task)> {
    let channel: ChannelName = "roadmap".parse().unwrap();
    let task = |status: TaskStatus| Task {
        id: TaskId::new(),
        title: "a task".into(),
        body: String::new(),
        status,
        close_reason: None,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 0,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: TaskKind::Task,
        issue: None,
        properties: Default::default(),
        created: NOW,
        updated: NOW,
        create_key: None,
    };
    vec![
        (channel.clone(), task(TaskStatus::Open)),
        (channel.clone(), task(TaskStatus::Open)),
        (channel, task(TaskStatus::Closed)),
    ]
}

#[test]
fn http_metrics_has_every_gauge_name_and_a_value() {
    let text = render_metrics(&metrics_view(), &metrics_tasks(), 2, Timestamp(900_000_000));
    for gauge in GAUGES {
        assert!(
            text.contains(&format!("# TYPE {gauge} gauge")),
            "no TYPE line for {gauge}:\n{text}"
        );
        assert!(
            text.contains(&format!("# HELP {gauge} ")),
            "no HELP line for {gauge}:\n{text}"
        );
        let samples = text
            .lines()
            .filter(|line| {
                line.starts_with(&format!("{gauge} ")) || line.starts_with(&format!("{gauge}{{"))
            })
            .count();
        assert!(samples > 0, "no sample of {gauge}:\n{text}");
    }
    // No other gauge slips in under a name that the spec does not have.
    let names: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|rest| rest.split_once(' ').map(|(name, _)| name))
        .collect();
    assert_eq!(names, GAUGES.to_vec(), "the gauges of the spec, in order");
}

#[test]
fn http_metrics_renders_the_values_of_the_view_and_the_tasks() {
    let text = render_metrics(&metrics_view(), &metrics_tasks(), 2, Timestamp(900_000_000));
    for expected in [
        "borant_hub_start_time_seconds 900000",
        "borant_host_online{host=\"h1\"} 1",
        "borant_host_heartbeat_age_seconds{host=\"h1\"} 30",
        "borant_host_load1{host=\"h1\"} 0.5",
        "borant_host_mem_avail_bytes{host=\"h1\"} 536870912",
        "borant_host_disk_used_ratio{host=\"h1\"} 0.25",
        "borant_host_agents{host=\"h1\"} 1",
        "borant_host_max_agents{host=\"h1\"} 4",
        "borant_agent_lease_state{agent=\"w1\",state=\"held\"} 1",
        "borant_agent_lease_state{agent=\"w1\",state=\"reserved\"} 0",
        "borant_agent_lease_state{agent=\"w1\",state=\"expired\"} 0",
        "borant_agent_lease_state{agent=\"w1\",state=\"released\"} 0",
        "borant_inbox_pending{agent=\"w1\"} 3",
        "borant_inbox_redelivered{agent=\"w1\"} 1",
        "borant_tasks{channel=\"roadmap\",status=\"open\"} 2",
        "borant_tasks{channel=\"roadmap\",status=\"in_progress\"} 0",
        "borant_tasks{channel=\"roadmap\",status=\"closed\"} 1",
        "borant_spawn_queue_length 2",
    ] {
        assert!(text.contains(expected), "no `{expected}`:\n{text}");
    }
}

#[test]
fn http_metrics_skips_a_host_value_it_does_not_have() {
    let mut view = metrics_view();
    view.hosts[0].last_seen = None;
    view.hosts[0].stats = None;
    let text = render_metrics(&view, &[], 0, NOW);
    assert!(text.contains("borant_host_online{host=\"h1\"} 1"));
    assert!(!text.contains("borant_host_heartbeat_age_seconds{host=\"h1\"}"));
    assert!(!text.contains("borant_host_load1{host=\"h1\"}"));
    assert!(text.contains("borant_host_max_agents{host=\"h1\"} 4"));
    assert!(text.contains("borant_spawn_queue_length 0"));
}

#[tokio::test]
async fn http_healthz_reports_the_hub_lease() {
    let fx = Fx::new();
    let (status, body) = fx.get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn http_healthz_is_503_without_the_hub_lease() {
    let fx = Fx::new();
    fx.state.seed(
        &keys::lease_hub(),
        bytes(&HubLease {
            holder: "another-hub".into(),
            token: borant_core::FencingToken(2),
            expires: NOW.plus(Duration::from_secs(60)),
        }),
    );
    let (status, _) = fx.get("/healthz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn http_snapshot_is_the_swarm_view() {
    let fx = Fx::new();
    let (status, body) = fx.get("/api/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&body).expect("the view as JSON");
    assert_eq!(view["hosts"][0]["info"]["name"], json!("h1"));
    assert_eq!(view["agents"][0]["agent"], json!("w1"));
    assert_eq!(view["agents"][0]["lease"]["state"], json!("held"));
}

#[tokio::test]
async fn http_graph_lists_the_tasks_with_the_stage_of_their_path() {
    let fx = Fx::new();
    let later = TaskId::new();
    let claimed: AgentId = fx.agent.clone();
    let mut blocked = task(later, TaskStatus::InProgress, vec![fx.task]);
    blocked.claimed_by = Some(claimed.clone());
    fx.state
        .seed(&keys::task(&fx.channel, &later), bytes(&blocked));

    let (status, body) = fx.get("/api/channels/roadmap/graph").await;
    assert_eq!(status, StatusCode::OK);
    let graph: serde_json::Value = serde_json::from_str(&body).expect("the graph as JSON");
    assert_eq!(graph["channel"], json!("roadmap"));
    assert_eq!(
        graph["tasks"],
        json!([
            {
                "id": fx.task,
                "title": "the task",
                "status": "open",
                "claimed_by": null,
                "blocked_by": [],
                "stage": 0,
            },
            {
                "id": later,
                "title": "the task",
                "status": "in_progress",
                "claimed_by": claimed,
                "blocked_by": [fx.task],
                "stage": 1,
            },
        ])
    );
}

#[tokio::test]
async fn http_graph_of_an_unknown_channel_is_404() {
    let fx = Fx::new();
    let (status, body) = fx.get("/api/channels/ghost/graph").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the body:\n{body}");
    assert!(body.contains("no channel #ghost"), "the body:\n{body}");
}

#[tokio::test]
async fn http_graph_of_a_bad_channel_name_is_400() {
    let fx = Fx::new();
    let (status, _) = fx.get("/api/channels/Bad.Name/graph").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_channels_lists_the_channels_and_not_their_tasks() {
    let fx = Fx::new();
    let (status, body) = fx.get("/api/channels").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        json!(["roadmap"])
    );
}

#[tokio::test]
async fn http_metrics_route_serves_the_text_format() {
    let fx = Fx::new();
    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();
    let response = router(fx.hub.clone(), NOW).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/plain; version=0.0.4"));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    for gauge in GAUGES {
        assert!(text.contains(gauge), "no {gauge}:\n{text}");
    }
    assert!(text.contains("borant_tasks{channel=\"roadmap\",status=\"open\"} 1"));
}

#[tokio::test]
async fn http_page_is_the_embedded_file() {
    let fx = Fx::new();
    let (status, body) = fx.get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<html"), "the page:\n{body}");
    assert_eq!(body, PAGE);
}

#[tokio::test]
async fn http_events_streams_the_log_from_since() {
    let fx = Fx::new();
    fx.log
        .publish(&SwarmEvent::hub(
            NOW,
            Severity::Info,
            SwarmEventBody::HubStarted {
                instance: "hub-test".into(),
                token: 1,
                quiet_secs: 0,
            },
        ))
        .await
        .expect("the memory log takes the event");

    let request = Request::builder()
        .uri("/api/events?since=900")
        .body(Body::empty())
        .unwrap();
    let response = router(fx.hub.clone(), NOW).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .starts_with("text/event-stream")
    );
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .expect("the stream sends the stored event")
        .expect("a frame")
        .expect("a frame");
    let data = frame.into_data().expect("a data frame");
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("event: swarm"), "the frame:\n{text}");
    assert!(text.contains("hub.started"), "the frame:\n{text}");
}

#[tokio::test]
async fn http_serve_answers_over_a_tcp_socket() {
    let fx = Fx::new();
    // A free port: the listener takes it after this one lets go.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let addr = taken.local_addr().expect("the address");
    drop(taken);

    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(serve(fx.hub.clone(), addr, shutdown.clone()));
    // The listener binds on its own task, so the first connect may race it.
    let mut stream = None;
    for _ in 0..100 {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    let mut stream = stream.expect("the listener answers");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: hub.test\r\nConnection: close\r\n\r\n")
        .await
        .expect("the request");
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.expect("the reply");
    let reply = String::from_utf8_lossy(&reply);
    assert!(reply.starts_with("HTTP/1.1 200"), "the reply:\n{reply}");
    assert!(reply.ends_with("ok"), "the reply:\n{reply}");

    shutdown.cancel();
    server
        .await
        .expect("the listener task")
        .expect("a clean stop");
}

#[tokio::test]
async fn http_serve_reports_an_address_in_use() {
    let fx = Fx::new();
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a free port");
    let addr = taken.local_addr().expect("the address");
    let error = serve(
        fx.hub.clone(),
        addr,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect_err("the bind fails");
    assert!(
        error.downcast_ref::<std::io::Error>().is_some(),
        "{error:#}"
    );
}

#[tokio::test]
async fn http_routes_never_write_to_the_store() {
    let fx = Fx::new();
    for uri in [
        "/",
        "/healthz",
        "/api/snapshot",
        "/api/channels",
        "/api/channels/roadmap/graph",
        "/metrics",
    ] {
        let (status, _) = fx.get(uri).await;
        assert!(status.is_success(), "{uri} answered {status}");
    }
    // The event stream follows, so the test reads its status and drops it.
    let status = fx.response("/api/events?since=900").await.status();
    assert!(status.is_success(), "/api/events answered {status}");
    assert_eq!(
        fx.writes.load(Ordering::SeqCst),
        0,
        "a route wrote to the store"
    );
}

/// A hub with the join secrets and a store that the join may write, with the
/// clock at [`NOW`].
struct JoinFx {
    hub: Arc<Hub>,
    state: MemoryState,
}

impl JoinFx {
    fn new(secrets: Option<crate::HubSecrets>) -> Self {
        let state = MemoryState::new();
        let hub = Arc::new(Hub::new(
            Arc::new(state.clone()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            Arc::new(ManualClock::new(NOW)),
            HubConfig {
                secrets,
                ..HubConfig::default()
            },
        ));
        Self { hub, state }
    }

    /// Plants a join key, as a create of `host` would.
    fn seed_token(&self, host: &str, token: &str, expires: Timestamp) {
        let host: HostName = host.parse().expect("a host name");
        self.state.seed(
            &keys::join(&host),
            bytes(&crate::JoinToken {
                host,
                digest: crate::join::digest(token),
                expires,
            }),
        );
    }

    async fn key(&self, host: &str) -> bool {
        let host: HostName = host.parse().expect("a host name");
        self.state
            .get(&keys::join(&host))
            .await
            .expect("the state")
            .is_some()
    }

    async fn post(&self, body: serde_json::Value) -> (StatusCode, String) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/join")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("the request");
        let response = router(self.hub.clone(), NOW)
            .oneshot(request)
            .await
            .expect("the reply");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("the body")
            .to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }
}

/// The one body of every refusal, so a caller learns nothing.
const REFUSED: &str = r#"{"error":"join refused"}"#;

#[tokio::test]
async fn http_join_returns_the_vars_of_host_join_once() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    let (status, body) = fx.post(json!({ "host": "e1", "token": "the-token" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reply: serde_json::Value = serde_json::from_str(&body).expect("the reply as JSON");
    assert_eq!(
        reply["vars"]["STORE_TOKEN"],
        json!(crate::testkit::STORE_TOKEN_VALUE)
    );
    assert_eq!(
        reply["vars"]["NATS_PASSWORD"],
        json!(crate::testkit::NATS_VALUE)
    );
    assert!(!fx.key("e1").await, "the key goes at the join");

    // The second use is refused, with the one body.
    let (status, body) = fx.post(json!({ "host": "e1", "token": "the-token" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, REFUSED);
}

#[tokio::test]
async fn http_join_refuses_the_token_of_another_host() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    let (status, body) = fx.post(json!({ "host": "e2", "token": "the-token" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, REFUSED);
    assert!(fx.key("e1").await, "the token of e1 survives the attempt");
}

#[tokio::test]
async fn http_join_refuses_an_expired_token() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.minus(Duration::from_secs(1)));

    let (status, body) = fx.post(json!({ "host": "e1", "token": "the-token" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, REFUSED);
    assert!(!fx.key("e1").await, "the expired key goes at the attempt");
}

#[tokio::test]
async fn http_join_refuses_a_token_that_does_not_match() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    let (status, body) = fx
        .post(json!({ "host": "e1", "token": "another-token" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, REFUSED);
    assert!(fx.key("e1").await, "a wrong guess does not spend the token");
}

#[tokio::test]
async fn http_join_refuses_without_the_secrets_of_the_hub() {
    let fx = JoinFx::new(None);
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    let (status, body) = fx.post(json!({ "host": "e1", "token": "the-token" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, REFUSED);
    assert!(fx.key("e1").await);
}

#[tokio::test]
async fn http_join_refuses_a_malformed_body_with_the_same_reply() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    for body in [
        json!({ "host": "e1" }),
        json!({ "token": "the-token" }),
        json!({}),
        json!({ "host": "Bad.Name", "token": "the-token" }),
    ] {
        let (status, reply) = fx.post(body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(reply, REFUSED, "{body}");
    }
    assert!(fx.key("e1").await, "no bad body spends the token");
}

#[tokio::test]
async fn http_join_of_two_requests_lets_one_win() {
    let fx = JoinFx::new(Some(crate::testkit::hub_secrets()));
    fx.seed_token("e1", "the-token", NOW.plus(Duration::from_secs(60)));

    let ask = || fx.post(json!({ "host": "e1", "token": "the-token" }));
    let (first, second) = tokio::join!(ask(), ask());
    let wins = [first.0, second.0]
        .iter()
        .filter(|status| **status == StatusCode::OK)
        .count();
    assert_eq!(wins, 1, "exactly one 200: {first:?} {second:?}");
    let loser = if first.0 == StatusCode::OK {
        second
    } else {
        first
    };
    assert_eq!(loser.0, StatusCode::FORBIDDEN);
    assert_eq!(loser.1, REFUSED);
}
