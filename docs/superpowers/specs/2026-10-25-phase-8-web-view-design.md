# Phase 8: the read-only web view

This builds phase 5 of the [observability design](2026-10-24-observability-design.md)
("Views" and "Phased plan"): a read-only HTTP listener in the hub, one web
page with the swarm snapshot and the task graph of a channel, and
`/metrics`. Observability phase 4 (usage and cost) waits for Chris's open
questions.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Goals

1. From a phone or a laptop on the tailnet, a person opens one page and sees
   the hosts, the agents, and the task graph of a chosen channel, refreshed
   every few seconds.
2. A Prometheus-compatible scraper reads `/metrics` from the hub.
3. Nothing on the page or the API changes state.

## Cuts

- Actions in the page (the design cuts them).
- Auth. The listener binds to one configured address; borant-infra binds it
  to the tailnet IP of borant-1, so the tailnet decides who reaches it.
- Request counters and histograms (`borant_hub_requests_total`, the
  durations), token and cost metrics. They need instrumentation or phase 4.
- A metrics crate. The text format of the gauges below is a few lines.
- A JS build step or a graph library.

## Constraints

- Strict ports and adapters: no core type or config key names Tailscale. The
  config is a bind address.
- Read-only: every route calls existing read paths (`Hub::view`, the task
  list, the event log). No route takes the hub request lock longer than those
  reads do today.
- The listener is off unless `[http] listen` is set.

## Interfaces

- `hub.toml`:

  ```toml
  [http]
  listen = "100.101.102.103:7420"   # any socket address; unset = off
  ```

- Routes (axum, already a workspace dependency):

  | Route | Content |
  |---|---|
  | `GET /` | One HTML page with inline JS and CSS, embedded with `include_str!` |
  | `GET /api/snapshot` | The `SwarmView` JSON of `ToolCall::View` |
  | `GET /api/channels` | The channel names |
  | `GET /api/channels/{ch}/graph` | `{channel, tasks: [{id, title, status, claimed_by, blocked_by, stage}]}` |
  | `GET /api/events?since=<secs>` | Server-sent events of the swarm event log (follow) |
  | `GET /metrics` | Prometheus text format |
  | `GET /healthz` | 200 with `ok` when the hub serves `borant.ctl` |

- `borant-core::rules::graph::stages(tasks: &[Task]) -> BTreeMap<TaskId, u32>`:
  the length of the longest `blocked_by` path from a task with no open
  prerequisite. A cycle or a missing prerequisite never loops: such a task
  gets the stage of its known prerequisites plus one.
- Metrics (gauges only, from `SwarmView` and the task list):
  `borant_hub_start_time_seconds`, `borant_host_online{host}`,
  `borant_host_heartbeat_age_seconds{host}`, `borant_host_load1{host}`,
  `borant_host_mem_avail_bytes{host}`, `borant_host_disk_used_ratio{host}`,
  `borant_host_agents{host}`, `borant_host_max_agents{host}`,
  `borant_agent_lease_state{agent,state}`, `borant_inbox_pending{agent}`,
  `borant_inbox_redelivered{agent}`, `borant_tasks{channel,status}`,
  `borant_spawn_queue_length`.
  A pure `render_metrics(view: &SwarmView, tasks: &[(ChannelName, Task)], queued: usize, now) -> String`
  in the hub.

## The page

Four panels: hosts (online, agents/max, load, memory, disk), agents (lease
state, host, role, inbox), the graph of the channel chosen in a select box
(one column per stage, cards colored by status, the claimer under the
title), and a timeline of the last 50 events. It polls `/api/snapshot` and
the graph every 5 s and reads the timeline from `/api/events`. It works at
phone width. It shows no secret: `SwarmView` and tasks hold none.

## Tasks

| Task | Blocked by | Files | Proof |
|---|---|---|---|
| Core: graph stages and the `[http]` config type | none | `borant-core/src/rules/graph.rs`, the hub config type | `just test-crate borant-core graph` (a chain, a fan-in, a cycle, a missing prerequisite) |
| Hub HTTP listener, the API routes, and `/metrics` | core | new `crates/borant-hub/src/http.rs`, wiring in `crates/borant/src/roles.rs` and config | `just test-crate borant-hub http` (each route with tower `oneshot` on memory adapters; `/metrics` text has the names above; no route writes) |
| The web page | none (it reads the JSON shapes above) | new `crates/borant-hub/src/web/index.html` | `just test-crate borant-hub web` (the page is served at `/` and names each API route it reads); the owner opens it on the tailnet after the deploy |
| Infra: bind to the tailnet IP, firewall on `tailscale0` only | all, a release | `borant-infra` | `curl` from the laptop over the tailnet gets `/healthz`; from the internet the port is closed |

## Decision records

### No auth, a configured bind address

Decision: the listener has no auth; it binds only to `[http] listen`.
Evidence: the page is read-only and holds no secret; the design binds it to
the tailnet IP, and the tailnet policy already limits who reaches borant-1.
Reversible: yes. A token header can come later.

### Gauges in plain text, no metrics crate

Decision: `/metrics` renders the gauges above by hand in the Prometheus text
format.
Evidence: every value already exists in `SwarmView` and the task list;
counters and histograms need instrumentation that this phase cuts. A crate
adds a registry for a dozen lines of text.
Reversible: yes. `prometheus-client` can replace the renderer when counters
come.
