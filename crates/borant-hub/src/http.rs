//! The HTTP listener: the page, the JSON API, and `/metrics`.
//!
//! Every read route serves what `borant.ctl` already reads (`Hub::view`, the
//! task list, the event log) and writes nothing. The one write route is
//! `POST /v1/join`, where a pool host that the hub created trades its
//! single-use token for the vars of bundle `host-join`. The listener is off
//! unless `[http] listen` is set; `crates/borant/src/roles.rs` starts it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use borant_core::board::BoardView;
use borant_core::events::EventFilter;
use borant_core::model::{
    HostStatus, HubLease, InboxStatus, LeaseState, SwarmView, Task, TaskStatus,
};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::{ChannelName, CollectionName, HostName, Timestamp, keys};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::Hub;
use crate::board::Caller;
use crate::metrics::{self, Histogram, Metrics};

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
        .route("/api/board", get(board))
        .route("/api/channels", get(channels))
        .route("/api/channels/{channel}/graph", get(graph))
        .route("/api/events", get(events))
        .route("/metrics", get(metrics))
        // The one write route: a pool host trades its join token for its
        // first secrets.
        .route("/v1/join", post(join))
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
        let status = match &self.0.code {
            ApiErrorCode::Invalid => StatusCode::BAD_REQUEST,
            ApiErrorCode::NotFound => StatusCode::NOT_FOUND,
            ApiErrorCode::Denied => StatusCode::FORBIDDEN,
            ApiErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::Conflict
            | ApiErrorCode::StaleToken
            | ApiErrorCode::Internal
            | ApiErrorCode::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::SagaNotApproved => StatusCode::CONFLICT,
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

/// The query of `GET /api/board`.
#[derive(Debug, Deserialize)]
struct BoardQuery {
    collection: Option<CollectionName>,
}

/// The board that a person reads: every collection, or the one that the query
/// names. It serves the person's view, as `/api/snapshot` does.
async fn board(
    State(app): State<App>,
    Query(query): Query<BoardQuery>,
) -> Result<Json<BoardView>, Fail> {
    Ok(Json(
        app.hub
            .board(Caller::Person, query.collection, false, None)
            .await?,
    ))
}

/// The body of `POST /v1/join`: the host name and its single-use token.
#[derive(Debug, Deserialize)]
struct JoinRequest {
    host: String,
    token: String,
}

/// The reply of every refusal: one status and one body, so a caller learns
/// nothing about the token that it sent.
fn join_refused() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "join refused" })),
    )
        .into_response()
}

/// `POST /v1/join`: trades the single-use join token of a pool host for the
/// vars of bundle `host-join`. This is the only route that returns a secret,
/// and the reply goes only to the caller.
async fn join(State(app): State<App>, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<JoinRequest>(&body) else {
        return join_refused();
    };
    let Ok(host) = req.host.parse::<HostName>() else {
        return join_refused();
    };
    match app.hub.redeem_join(&host, &req.token).await {
        Ok(vars) => {
            tracing::info!(%host, "join accepted");
            (StatusCode::OK, Json(serde_json::json!({ "vars": vars }))).into_response()
        }
        // `redeem_join` logs the reason, never the token.
        Err(_) => join_refused(),
    }
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

/// The task graph of one channel: every task with the stage of its
/// `blocked_by` path, so the page draws one column per stage.
async fn graph(
    State(app): State<App>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<ChannelGraph>, Fail> {
    let channel: ChannelName = name
        .parse()
        .map_err(|e: borant_core::CoreError| ApiError::invalid(e.to_string()))?;
    let tasks = app.hub.task_list(&channel, false).await?;
    let stages = borant_core::rules::graph::stages(&tasks);
    let mut rows: Vec<GraphTask> = tasks
        .into_iter()
        .map(|task| GraphTask {
            stage: stages.get(&task.id).copied().unwrap_or(0),
            id: task.id,
            title: task.title,
            status: task.status,
            claimed_by: task.claimed_by,
            blocked_by: task.blocked_by,
        })
        .collect();
    rows.sort_by(|a, b| a.stage.cmp(&b.stage).then_with(|| a.id.cmp(&b.id)));
    Ok(Json(ChannelGraph {
        channel,
        tasks: rows,
    }))
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
    let text = render_metrics(&view, &tasks, queued, app.started, &app.hub.metrics);
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

/// Renders the metrics of the phase 8 spec from a `SwarmView`, the tasks of
/// every channel, the length of the spawn queue, and the counters that the
/// request and tick paths record.
///
/// `started` is when the hub started serving: the value of
/// `borant_hub_start_time_seconds`. The ages are measured from the view's
/// `at`, the hub clock when it built the view, or from `started` for a view
/// without one. A host or an agent with no value for a gauge gets no series
/// for it, never a made-up zero.
pub(crate) fn render_metrics(
    view: &SwarmView,
    tasks: &[(ChannelName, Task)],
    queued: usize,
    started: Timestamp,
    counters: &Metrics,
) -> String {
    let now = view.at.unwrap_or(started);
    let mut out = String::new();

    header(
        &mut out,
        "borant_hub_start_time_seconds",
        "Unix time when the hub started serving, in seconds. A value that changes between two scrapes means the hub restarted.",
    );
    line(
        &mut out,
        "borant_hub_start_time_seconds",
        &[],
        started.0 as f64 / 1000.0,
    );

    header_kind(
        &mut out,
        "borant_hub_ctl_requests_total",
        "counter",
        "The requests of borant.ctl that the hub served, by kind.",
    );
    for (kind, count) in metrics::CTL_KINDS.iter().zip(counters.ctl_requests()) {
        line(
            &mut out,
            "borant_hub_ctl_requests_total",
            &[("kind", kind)],
            count as f64,
        );
    }

    histogram_header(
        &mut out,
        "borant_hub_ctl_latency_seconds",
        "The time a request of borant.ctl took, by kind.",
    );
    for (i, kind) in metrics::CTL_KINDS.iter().enumerate() {
        histogram_samples(
            &mut out,
            "borant_hub_ctl_latency_seconds",
            &[("kind", kind)],
            counters.ctl_latency(i),
        );
    }

    header_kind(
        &mut out,
        "borant_hub_ctl_errors_total",
        "counter",
        "The requests of borant.ctl that the hub answered with an error, by kind.",
    );
    for (kind, count) in metrics::CTL_KINDS.iter().zip(counters.ctl_errors()) {
        line(
            &mut out,
            "borant_hub_ctl_errors_total",
            &[("kind", kind)],
            count as f64,
        );
    }

    histogram_header(
        &mut out,
        "borant_hub_tick_seconds",
        "The time one pass of the hub tick took.",
    );
    histogram_samples(&mut out, "borant_hub_tick_seconds", &[], counters.tick());

    header(
        &mut out,
        "borant_hub_store_keys",
        "The keys under each prefix, from the last tick that listed them.",
    );
    for (prefix, count) in counters.store_keys() {
        line(
            &mut out,
            "borant_hub_store_keys",
            &[("prefix", prefix)],
            count as f64,
        );
    }

    header(
        &mut out,
        "borant_hub_inbox_pending_total",
        "The messages that every inbox has not delivered yet.",
    );
    let pending: u64 = view.inboxes.iter().map(|inbox| inbox.stats.pending).sum();
    line(
        &mut out,
        "borant_hub_inbox_pending_total",
        &[],
        pending as f64,
    );

    // Off Linux there is no `/proc/self/statm`, so the series is absent.
    if let Some(bytes) = metrics::resident_bytes() {
        header(
            &mut out,
            "borant_hub_process_resident_bytes",
            "The resident set of the hub process, in bytes.",
        );
        line(
            &mut out,
            "borant_hub_process_resident_bytes",
            &[],
            bytes as f64,
        );
    }

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
    header_kind(out, name, "gauge", help);
}

/// One `# HELP` and `# TYPE <kind>` pair.
fn header_kind(out: &mut String, name: &str, kind: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

/// The `# HELP` and `# TYPE histogram` pair of a family that several label
/// sets share. Prometheus takes one pair per family, so the samples of every
/// label set follow it from [`histogram_samples`].
fn histogram_header(out: &mut String, name: &str, help: &str) {
    header_kind(out, name, "histogram", help);
}

/// Every sample of one histogram with `labels`: the cumulative buckets, the
/// `+Inf` bucket, the sum, and the count.
fn histogram_samples(out: &mut String, name: &str, labels: &[(&str, &str)], histogram: &Histogram) {
    let (cumulative, count, sum) = histogram.read();
    for (bound, seen) in histogram.bounds().iter().zip(&cumulative) {
        let le = bound.to_string();
        let mut all = labels.to_vec();
        all.push(("le", le.as_str()));
        line(out, &format!("{name}_bucket"), &all, *seen as f64);
    }
    let mut all = labels.to_vec();
    all.push(("le", "+Inf"));
    line(out, &format!("{name}_bucket"), &all, count as f64);
    line(out, &format!("{name}_sum"), labels, sum);
    line(out, &format!("{name}_count"), labels, count as f64);
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
