//! The local API on a Unix socket with mode 0600.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use borant_core::events::{EventFilter, Severity};
use borant_core::proto::{ApiError, ApiErrorCode, LocalEvent, LocalRegister, Reply, ToolCall};
use borant_core::{AgentId, ChannelName, HostName, SessionId, Timestamp};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::Host;

struct ApiFail(ApiError);

impl IntoResponse for ApiFail {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ApiErrorCode::Invalid => StatusCode::BAD_REQUEST,
            ApiErrorCode::NotFound => StatusCode::NOT_FOUND,
            ApiErrorCode::Conflict | ApiErrorCode::StaleToken => StatusCode::CONFLICT,
            ApiErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::Denied => StatusCode::FORBIDDEN,
        };
        (status, Json(Reply::Error(self.0))).into_response()
    }
}

impl From<ApiError> for ApiFail {
    fn from(e: ApiError) -> Self {
        Self(e)
    }
}

fn session_id(s: &str) -> Result<SessionId, ApiFail> {
    s.parse()
        .map_err(|_| ApiFail(ApiError::invalid(format!("bad session ID '{s}'"))))
}

#[derive(Deserialize)]
struct WaitQuery {
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct ResolveBody {
    #[serde(default)]
    ancestors: Vec<u32>,
    #[serde(default)]
    harness_session_id: Option<String>,
}

#[derive(Deserialize)]
struct CallBody {
    #[serde(default)]
    session: Option<SessionId>,
    call: ToolCall,
}

/// The query of `GET /v1/events`. Every field is optional; the names match
/// `EventFilter`.
#[derive(Deserialize)]
struct EventsQuery {
    /// Only events at or after this time, in unix milliseconds.
    since: Option<Timestamp>,
    /// `1` or `true` keeps the stream open for the events published from now
    /// on. Without it the reply holds the events of the filter and closes.
    follow: Option<String>,
    /// Kinds, comma separated or repeated. An entry may end in `.*`.
    #[serde(default)]
    kinds: Vec<String>,
    agent: Option<AgentId>,
    channel: Option<ChannelName>,
    host: Option<HostName>,
    /// `info`, `warn`, or `error`.
    min_severity: Option<Severity>,
}

pub fn router(host: Host) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/sessions", post(register))
        .route("/v1/resolve", post(resolve))
        .route("/v1/sessions/{id}", get(session_info))
        .route("/v1/sessions/{id}/events", post(event))
        .route("/v1/sessions/{id}/wait", get(wait))
        .route("/v1/sessions/{id}/stream", get(stream))
        .route("/v1/events", get(events))
        .route("/v1/call", post(call))
        .with_state(host)
}

async fn health(State(h): State<Host>) -> Json<Reply> {
    let sessions = h.state().sessions.len();
    Json(Reply::ok(
        &json!({ "host": h.config().name, "sessions": sessions }),
    ))
}

async fn register(
    State(h): State<Host>,
    Json(req): Json<LocalRegister>,
) -> Result<Json<Reply>, ApiFail> {
    Ok(Json(Reply::ok(&h.register(req).await?)))
}

async fn resolve(
    State(h): State<Host>,
    Json(b): Json<ResolveBody>,
) -> Result<Json<Reply>, ApiFail> {
    let found = h.resolve_pid(&b.ancestors).or_else(|| {
        b.harness_session_id
            .as_deref()
            .and_then(|s| h.resolve_harness_session(s))
    });
    match found {
        Some(id) => Ok(Json(Reply::ok(&h.local_view(id)?))),
        None => Err(ApiFail(crate::persist::no_session(
            h.restored(),
            "no local session for these processes".into(),
        ))),
    }
}

async fn session_info(
    State(h): State<Host>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Reply>, ApiFail> {
    Ok(Json(Reply::ok(&h.local_view(session_id(&id)?)?)))
}

async fn event(
    State(h): State<Host>,
    UrlPath(id): UrlPath<String>,
    Json(ev): Json<LocalEvent>,
) -> Result<Json<Reply>, ApiFail> {
    Ok(Json(Reply::ok(&h.event(session_id(&id)?, ev).await?)))
}

async fn wait(
    State(h): State<Host>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<WaitQuery>,
) -> Result<Json<Reply>, ApiFail> {
    let timeout = Duration::from_millis(q.timeout_ms.unwrap_or(300_000).min(3_600_000));
    let messages = h.wait(session_id(&id)?, timeout).await?;
    Ok(Json(Reply::ok(&json!({ "messages": messages }))))
}

async fn stream(
    State(h): State<Host>,
    UrlPath(id): UrlPath<String>,
) -> Result<impl IntoResponse, ApiFail> {
    let id = session_id(&id)?;
    h.local_view(id)?;
    let events = h.stream(id).map(|env| {
        Ok::<_, std::convert::Infallible>(
            Event::default()
                .event("message")
                .data(serde_json::to_string(&env).unwrap_or_default()),
        )
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

async fn call(State(h): State<Host>, Json(b): Json<CallBody>) -> Result<Json<Reply>, ApiFail> {
    Ok(Json(Reply::ok(&h.call(b.session, b.call).await?)))
}

/// Streams the swarm events of the filter as server-sent events, one JSON
/// event per `data:` line. With `follow` the stream stays open.
async fn events(
    State(h): State<Host>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, ApiFail> {
    let kinds: Vec<String> = q
        .kinds
        .iter()
        .flat_map(|k| k.split(','))
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect();
    let filter = EventFilter {
        since: q.since,
        kinds,
        agent: q.agent,
        channel: q.channel,
        host: q.host,
        min_severity: q.min_severity,
    };
    let follow = q
        .follow
        .as_deref()
        .is_some_and(|v| matches!(v, "1" | "true" | "yes"));
    let stream = h.event_stream(&filter, follow).await?;
    let events = stream.map(|e| {
        Ok::<_, std::convert::Infallible>(
            Event::default()
                .event("swarm")
                .data(serde_json::to_string(&e).unwrap_or_default()),
        )
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Serves the local API on `socket` until `shutdown`. Removes a stale
/// socket file first.
pub async fn serve(host: Host, socket: &Path, shutdown: CancellationToken) -> anyhow::Result<()> {
    if let Some(dir) = socket.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    if socket.exists() {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            anyhow::bail!(
                "another host daemon is running on {}; stop it first",
                socket.display()
            );
        }
        tokio::fs::remove_file(socket).await?;
    }
    let listener = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(socket = %socket.display(), "local API listening");
    axum::serve(listener, router(host))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use borant_core::events::{SwarmEvent, SwarmEventBody};
    use borant_core::ports::EventLog;
    use borant_core::{Clock, ManualClock, Timestamp};
    use borant_memory::MemoryEventLog;

    /// The body of an events reply, as text.
    async fn body_of(q: EventsQuery, host: Host) -> String {
        let reply = match events(State(host), Query(q)).await {
            Ok(reply) => reply.into_response(),
            Err(_) => panic!("the test host has an event log"),
        };
        let bytes = http_body_util::BodyExt::collect(reply.into_body())
            .await
            .unwrap()
            .to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn query(kinds: &[&str]) -> EventsQuery {
        EventsQuery {
            since: None,
            follow: None,
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
            agent: Some("w1".parse().unwrap()),
            channel: None,
            host: None,
            min_severity: None,
        }
    }

    #[tokio::test]
    async fn the_events_route_streams_a_published_event() {
        let log = Arc::new(MemoryEventLog::new());
        let clock = Arc::new(ManualClock::new(Timestamp(1_000_000)));
        let host = crate::outbox::test_host(log.clone(), clock.clone());
        let event = SwarmEvent::host(
            clock.now(),
            &"h1".parse().unwrap(),
            Severity::Warn,
            SwarmEventBody::SessionPaused {
                why: "the hub is down".into(),
            },
        )
        .about_agent("w1".parse().unwrap());
        log.publish(&event).await.unwrap();

        let text = body_of(query(&["session.*"]), host.clone()).await;
        assert!(text.contains("session.paused"), "{text}");
        assert!(text.contains("\"agent\":\"w1\""), "{text}");

        // The kinds of the query filter the stream.
        let other = body_of(query(&["delivery.*"]), host).await;
        assert!(!other.contains("session.paused"), "{other}");
    }
}
