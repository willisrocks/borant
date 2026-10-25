//! The read-only HTTP listener: the page, the JSON API, and `/metrics`.
//!
//! Every route reads what `borant.ctl` already reads (`Hub::view`, the task
//! list, the event log) and writes nothing. The listener is off unless
//! `[http] listen` is set; `crates/borant/src/roles.rs` starts it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use borant_core::events::EventFilter;
use borant_core::model::{
    HostStatus, HubLease, InboxStatus, LeaseState, SwarmView, Task, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::{ChannelName, Timestamp, keys};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::Hub;

/// The web page, embedded into the binary.
const PAGE: &str = include_str!("web/index.html");

/// One page and the read-only API over the hub's state.
#[derive(Clone)]
struct App {
    hub: Arc<Hub>,
    /// When the hub started serving, for `borant_hub_start_time_seconds`.
    started: Timestamp,
}

/// The router of the listener. `started` is when the hub started serving.
pub fn router(hub: Arc<Hub>, started: Timestamp) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/healthz", get(healthz))
        .route("/api/snapshot", get(snapshot))
        .route("/api/channels", get(channels))
        .route("/api/events", get(events))
        .route("/metrics", get(metrics))
        .with_state(App { hub, started })
}

/// Serves the listener on `listen` until `shutdown`.
pub async fn serve(
    hub: Arc<Hub>,
    listen: SocketAddr,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let started = hub.serving_since().await;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "hub HTTP listener");
    axum::serve(listener, router(hub, started))
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

/// A failed route, as the JSON error and status that the API returns.
struct Fail(ApiError);

impl From<ApiError> for Fail {
    fn from(error: ApiError) -> Self {
        Self(error)
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ApiErrorCode::Invalid => StatusCode::BAD_REQUEST,
            ApiErrorCode::NotFound => StatusCode::NOT_FOUND,
            ApiErrorCode::Denied => StatusCode::FORBIDDEN,
            ApiErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::Conflict | ApiErrorCode::StaleToken | ApiErrorCode::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = serde_json::json!({ "error": self.0.message });
        (status, Json(body)).into_response()
    }
}

async fn page() -> Html<&'static str> {
    Html(PAGE)
}

/// 200 when this hub holds `lease.hub` and serves `borant.ctl`.
async fn healthz(State(app): State<App>) -> Response {
    let serving = match app.hub.store.get::<HubLease>(&keys::lease_hub()).await {
        Ok(Some(doc)) => {
            doc.value.holder == app.hub.cfg.instance && doc.value.expires > app.hub.now()
        }
        Ok(None) => false,
        Err(e) => return Fail(e).into_response(),
    };
    if serving {
        (StatusCode::OK, "ok").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "this hub does not hold borant.ctl",
        )
            .into_response()
    }
}

/// The `SwarmView` that `borant top` and `ToolCall::View` show.
async fn snapshot(State(app): State<App>) -> Result<Json<SwarmView>, Fail> {
    Ok(Json(app.hub.view().await?))
}

/// The channels of the swarm, by name.
async fn channels(State(app): State<App>) -> Result<Json<Vec<ChannelName>>, Fail> {
    Ok(Json(channel_names(&app.hub).await?))
}

/// The channels of the store, in name order.
async fn channel_names(hub: &Hub) -> Result<Vec<ChannelName>, ApiError> {
    let mut names: Vec<ChannelName> = hub
        .store
        .list::<serde_json::Value>(keys::CHANNEL_PREFIX)
        .await?
        .into_iter()
        // `chan.<name>.task.<id>` is under the same prefix.
        .filter(|doc| doc.key.matches('.').count() == 1)
        .filter_map(|doc| {
            doc.key
                .strip_prefix(keys::CHANNEL_PREFIX)
                .and_then(|name| name.parse().ok())
        })
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// The query of `GET /api/events`: `since` in Unix seconds.
#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<i64>,
}

/// The swarm event log as server-sent events, one JSON event per `data:`
/// line, following new events until the reader goes away.
async fn events(
    State(app): State<App>,
    Query(q): Query<EventsQuery>,
) -> Result<impl IntoResponse, Fail> {
    let filter = EventFilter {
        since: q.since.map(|secs| Timestamp(secs.saturating_mul(1000))),
        ..EventFilter::default()
    };
    let stream: BoxStream<'static, Result<Event, std::convert::Infallible>> = match &app.hub.events
    {
        Some(log) => log
            .read(&filter, true)
            .await
            .map_err(ApiError::from)?
            .map(|event| {
                Ok(Event::default()
                    .event("swarm")
                    .data(serde_json::to_string(&event).unwrap_or_default()))
            })
            .boxed(),
        None => futures::stream::empty().boxed(),
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// The gauges of the spec in the Prometheus text format.
async fn metrics(State(app): State<App>) -> Result<Response, Fail> {
    let view = app.hub.view().await?;
    let mut tasks = Vec::new();
    for name in channel_names(&app.hub).await? {
        for task in app.hub.tasks(&name).await? {
            tasks.push((name.clone(), task));
        }
    }
    let queued = app.hub.spawn_queue_length().await?;
    let text = render_metrics(&view, &tasks, queued, app.started);
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response())
}

/// The lease states of the `borant_agent_lease_state` gauge, in a fixed
/// order.
const LEASE_STATES: [&str; 4] = ["held", "reserved", "expired", "released"];

/// The task statuses of `borant_tasks`, in a fixed order.
const TASK_STATUSES: [&str; 5] = ["open", "in_progress", "needs_human", "stuck", "closed"];

/// Renders the gauges of the phase 8 spec from a `SwarmView`, the tasks of
/// every channel, and the length of the spawn queue.
///
/// `started` is when the hub started serving: the value of
/// `borant_hub_start_time_seconds`. The ages are measured from the view's
/// `at`, the hub clock when it built the view, or from `started` for a view
/// without one. A host or an agent with no value for a gauge gets no series
/// for it, never a made-up zero.
pub fn render_metrics(
    view: &SwarmView,
    tasks: &[(ChannelName, Task)],
    queued: usize,
    started: Timestamp,
) -> String {
    let now = view.at.unwrap_or(started);
    let mut out = String::new();

    header(
        &mut out,
        "borant_hub_start_time_seconds",
        "Unix time when the hub started serving, in seconds.",
    );
    line(
        &mut out,
        "borant_hub_start_time_seconds",
        &[],
        started.0 as f64 / 1000.0,
    );

    header(
        &mut out,
        "borant_host_online",
        "1 when the hub saw the host within the lease time.",
    );
    let mut hosts: Vec<&HostStatus> = view.hosts.iter().collect();
    hosts.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    for host in &hosts {
        line(
            &mut out,
            "borant_host_online",
            &[("host", host.info.name.as_str())],
            if host.online { 1.0 } else { 0.0 },
        );
    }

    header(
        &mut out,
        "borant_host_heartbeat_age_seconds",
        "Seconds since the last heartbeat of the host.",
    );
    for host in &hosts {
        if let Some(seen) = host.last_seen {
            line(
                &mut out,
                "borant_host_heartbeat_age_seconds",
                &[("host", host.info.name.as_str())],
                now.since(seen).as_secs_f64(),
            );
        }
    }

    header(
        &mut out,
        "borant_host_load1",
        "The one-minute load average of the host.",
    );
    for host in &hosts {
        if let Some(stats) = &host.stats {
            line(
                &mut out,
                "borant_host_load1",
                &[("host", host.info.name.as_str())],
                stats.load1,
            );
        }
    }

    header(
        &mut out,
        "borant_host_mem_avail_bytes",
        "The memory available on the host, in bytes.",
    );
    for host in &hosts {
        if let Some(mb) = host.stats.as_ref().and_then(|stats| stats.mem_avail_mb) {
            line(
                &mut out,
                "borant_host_mem_avail_bytes",
                &[("host", host.info.name.as_str())],
                mb as f64 * 1024.0 * 1024.0,
            );
        }
    }

    header(
        &mut out,
        "borant_host_disk_used_ratio",
        "The used part of the hub disk of the host, from 0 to 1.",
    );
    for host in &hosts {
        if let Some(pct) = host.stats.as_ref().and_then(|stats| stats.disk_used_pct) {
            line(
                &mut out,
                "borant_host_disk_used_ratio",
                &[("host", host.info.name.as_str())],
                pct / 100.0,
            );
        }
    }

    header(
        &mut out,
        "borant_host_agents",
        "The agents that the hub placed on the host.",
    );
    for host in &hosts {
        let agents = view
            .agents
            .iter()
            .filter(|agent| agent.host.as_ref() == Some(&host.info.name))
            .count();
        line(
            &mut out,
            "borant_host_agents",
            &[("host", host.info.name.as_str())],
            agents as f64,
        );
    }

    header(
        &mut out,
        "borant_host_max_agents",
        "The agent slots of the host.",
    );
    for host in &hosts {
        line(
            &mut out,
            "borant_host_max_agents",
            &[("host", host.info.name.as_str())],
            host.info.max_agents as f64,
        );
    }

    header(
        &mut out,
        "borant_agent_lease_state",
        "1 for the state of the agent's lease, 0 for the others.",
    );
    let mut agents: Vec<&borant_core::model::AgentStatus> = view.agents.iter().collect();
    agents.sort_by(|a, b| a.agent.cmp(&b.agent));
    for agent in &agents {
        let state = agent.lease.as_ref().map(lease_state);
        for name in LEASE_STATES {
            line(
                &mut out,
                "borant_agent_lease_state",
                &[("agent", agent.agent.as_str()), ("state", name)],
                if state == Some(name) { 1.0 } else { 0.0 },
            );
        }
    }

    header(
        &mut out,
        "borant_inbox_pending",
        "The messages that the inbox of the agent has not delivered yet.",
    );
    for inbox in sorted_inboxes(view) {
        line(
            &mut out,
            "borant_inbox_pending",
            &[("agent", inbox.agent.as_str())],
            inbox.stats.pending as f64,
        );
    }

    header(
        &mut out,
        "borant_inbox_redelivered",
        "The messages that the inbox of the agent delivered more than once.",
    );
    for inbox in sorted_inboxes(view) {
        line(
            &mut out,
            "borant_inbox_redelivered",
            &[("agent", inbox.agent.as_str())],
            inbox.stats.redelivered as f64,
        );
    }

    header(
        &mut out,
        "borant_tasks",
        "The tasks of each channel, by status.",
    );
    let counts = task_counts(tasks);
    for (channel, status, count) in &counts {
        line(
            &mut out,
            "borant_tasks",
            &[("channel", channel.as_str()), ("status", status)],
            *count as f64,
        );
    }

    header(
        &mut out,
        "borant_spawn_queue_length",
        "The spawns that wait for a placement or a launch.",
    );
    line(&mut out, "borant_spawn_queue_length", &[], queued as f64);

    out
}

/// The state label of a lease.
fn lease_state(state: &LeaseState) -> &'static str {
    match state {
        LeaseState::Held { .. } => "held",
        LeaseState::Reserved { .. } => "reserved",
        LeaseState::Expired { .. } => "expired",
        LeaseState::Released { .. } => "released",
    }
}

/// The status label of a task.
fn task_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Open => "open",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::NeedsHuman => "needs_human",
        TaskStatus::Stuck => "stuck",
        TaskStatus::Closed => "closed",
    }
}

fn sorted_inboxes(view: &SwarmView) -> Vec<&InboxStatus> {
    let mut inboxes: Vec<&InboxStatus> = view.inboxes.iter().collect();
    inboxes.sort_by(|a, b| a.agent.cmp(&b.agent));
    inboxes
}

/// The count of each status of each channel that the tasks name, every
/// status present even at zero, so a scrape sees a task close.
fn task_counts(tasks: &[(ChannelName, Task)]) -> Vec<(ChannelName, &'static str, usize)> {
    let mut channels: Vec<ChannelName> = tasks.iter().map(|(ch, _)| ch.clone()).collect();
    channels.sort();
    channels.dedup();
    let mut out = Vec::new();
    for channel in channels {
        for status in TASK_STATUSES {
            let count = tasks
                .iter()
                .filter(|(ch, task)| ch == &channel && task_status(task.status) == status)
                .count();
            out.push((channel.clone(), status, count));
        }
    }
    out
}

/// One `# HELP` and `# TYPE gauge` pair.
fn header(out: &mut String, name: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n"));
}

/// One sample line. No labels makes a gauge without a label set.
fn line(out: &mut String, name: &str, labels: &[(&str, &str)], value: f64) {
    let value = if value.is_finite() { value } else { 0.0 };
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (key, label)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{key}=\"{}\"", escape(label)));
        }
        out.push('}');
    }
    out.push_str(&format!(" {value}\n"));
}

/// Escapes a label value: `\`, `"`, and the newline.
fn escape(labels: &str) -> String {
    labels
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// A route with a channel name in its path, for the graph of that channel.
#[derive(Debug, Serialize)]
pub struct ChannelGraph {
    pub channel: ChannelName,
    pub tasks: Vec<GraphTask>,
}

/// One task of `ChannelGraph`, with the stage of its `blocked_by` path.
#[derive(Debug, Serialize)]
pub struct GraphTask {
    pub id: borant_core::TaskId,
    pub title: String,
    pub status: TaskStatus,
    pub claimed_by: Option<borant_core::AgentId>,
    pub blocked_by: Vec<borant_core::TaskId>,
    pub stage: u32,
}

#[cfg(test)]
mod tests;
