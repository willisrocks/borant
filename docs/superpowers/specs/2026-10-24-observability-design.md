# Borant observability design

Date: 2026-10-24. Status: draft for review. Track 4 of five parallel designs.

Related designs from the same day: `2026-10-24-secrets-and-host-bootstrap-design.md`
(the ntfy token), `2026-10-24-elastic-compute-design.md` (VPS cost and scale to
zero, which read the host stats from this design),
`2026-10-24-orchestration-api-and-sandboxes-design.md` (spawn and placement,
which own the fields of the `placement.*` events), and
`2026-10-24-durable-orchestration-vision.md`.

## Summary

Recommendation: add one durable "swarm event" stream first, then a hub
snapshot that feeds `borant top`, a `/metrics` endpoint, and phone alerts.
The hub already knows almost everything a view needs, but today it keeps that
knowledge in memory or in debug logs. The host daemons add a small stats
block to the heartbeat that they already send every 10 seconds. Thus the hub
becomes the one place to read, and a laptop that sleeps never leaves a broken
scrape target. LLM cost accounting and a small web dashboard come next. A
Prometheus-compatible store with Grafana on borant-1 is the last and optional
step, for history over weeks. OpenTelemetry export from Borant itself is cut.

## Goals and non-goals

Goals:

1. One command shows the live swarm and updates itself: hosts, agents,
   leases, inboxes, channels, and recent events. No loop over `borant status`.
2. A delivery problem is visible without `natscli`. Pending, ack-pending, and
   redelivered counts for each inbox are part of the snapshot.
3. Every important change becomes an event with a reason, kept for 14 days.
4. Chris gets a phone notification for the few states that need him.
5. LLM tokens and cost are counted for each agent, channel, and day, with a
   daily budget alert.
6. Dead agents leave the registry, so the views show real agents only.

Non-goals:

- High availability for the views. If the hub is down, the views are down.
  An always-on host daemon sends the "hub unreachable" alert.
- Traces across hub and hosts. A request rarely crosses more than two
  processes.
- Access control for several users. Tailscale membership is the access
  control.
- Exact cost for each task. Cost goes to an agent and a channel only.

## Current state

These facts come from the code at `v0.1.0-6-g4352933`.

| Area | Today |
|---|---|
| Hub events | Only task events on `borant.chan.{ch}.tasks`. Lease, host, and move changes publish nothing. A move writes one `move` row to the session log. |
| Logs | `tracing` to stderr, level from `BORANT_LOG`, default `info`. "ack failed", "cannot log message yet", "inbox pull failed", and "request failed" log at `debug`. |
| Host stats | `HostHeartbeat` carries only `{host, sessions}`. The hub keeps last-seen time in memory. |
| Inbox stats | The bus port reads `num_pending` only. `ack_wait` is 60 s. `max_deliver` is unlimited. |
| Views | `borant status` prints tables. `agents`, `hosts`, and `tasks` print JSON. No `--follow`. |
| HTTP | The host daemon serves axum on `host.sock` only. The hub has no HTTP server. |
| Notifier | `DesktopNotifier` on the host, for task status `needs_human` and `stuck` only. On by default on macOS only. |
| Usage | No token, cost, or model field in the model, the hook handler, or the Pi extension. |
| Registry | Nothing deletes an agent, session, lease, host, or inbox consumer. `BORANT_SESSION` has no age limit. The `seen` set in `delivery.rs` has no bound. |

## Options compared

| Option | Effort | Value now | Verdict |
|---|---|---|---|
| A. Event stream and `borant events --follow` | 2 to 3 days | High. It replaces the polling loop and the debug logs. | Phase 1 |
| B. Heartbeat stats, hub snapshot, `borant top` | 3 to 4 days | High. It replaces `natscli consumer info`. | Phase 2 |
| C. Web dashboard served by the hub over Tailscale | 2 to 3 days after B | Medium. It works from the phone and shows the task graph. | Phase 5 |
| D. VictoriaMetrics and Grafana on borant-1 through NixOS | 1 day, plus dashboards | Medium later. History and cost trends over weeks. | Phase 6, optional |
| E. OpenTelemetry export from Borant | 2 days, plus a collector | Low. Prometheus text covers the same metrics. | Cut |
| F. Sentry for the daemons | 1 day, plus an account | Low. Errors go to the event stream. | Cut |

The order puts the stress-test problems first: polling, hidden logs, and
inbox counters. Option D is cheap on NixOS, but it answers "what happened last
month", and nothing needs that answer yet.

## Recommended design

### Data flow

The hub is the single reader for the views, as it is the single writer for
the control plane.

```
harness adapter --LocalEvent--> host daemon --heartbeat + stats--> hub daemon
                                    |                                 | Snapshot (memory)
                                    +--events--> BORANT_EVENTS <------+ usage (KV)
                                                      |               |
                        borant events / top / web <---+---------------+--> /metrics
```

- Each host daemon adds `stats` to `HostHeartbeat`: host load and counter
  deltas since the last good reply. The host keeps a delta until the hub acks
  the heartbeat, so a missed heartbeat loses nothing.
- The hub reads inbox stats through the bus port every 15 seconds.
- The hub builds a `Snapshot` on its tick. `borant top`, the web page, and
  `/metrics` read the same snapshot.

### Swarm event stream

| Setting | Value |
|---|---|
| Stream | `BORANT_EVENTS`, subjects `borant.events.>` |
| Subject | `borant.events.{kind}`, for example `borant.events.lease.expired`. A reader filters with `borant.events.lease.>`. |
| Retention | Limits, `max_age` 14 days, `max_bytes` 512 MiB, discard old |
| Writers | The hub daemon, and host daemons for host-side kinds |
| Dedupe | `Nats-Msg-Id` is the event ID, as in `BORANT_SESSION` |

A host publishes events through its SQLite outbox. Thus the events of a hub
outage, such as `session.paused`, arrive when the hub returns. Events are data
plane, so the rule that only the hub writes the control plane still holds.

```rust
pub struct SwarmEvent {
    pub id: Ulid,
    pub at: Timestamp,
    pub severity: Severity,       // info, warn, error
    pub source: EventSource,      // { role: hub|host, host }
    pub agent: Option<AgentId>,
    pub host: Option<String>,     // the host that the event is about
    pub channel: Option<String>,
    pub task: Option<TaskId>,
    pub reason: Option<String>,   // one line for people
    #[serde(flatten)]
    pub body: SwarmEventBody,     // tagged "kind", typed fields
}
```

| Kind | Writer | Fields | Severity |
|---|---|---|---|
| `hub.started`, `hub.stepped_down`, `hub.maintenance` | hub | `instance`, `token`, `quiet_secs`, `on` | info, warn |
| `host.registered`, `host.online`, `host.offline`, `host.draining` | hub | `last_seen`, `always_on`, `agents` | info, warn when an always-on host goes offline |
| `hub.unreachable` | host | `since` | error |
| `nats.disconnected`, `nats.reconnected` | hub, host | `server`, `down_secs` | warn, info |
| `lease.acquired`, `lease.released`, `lease.reclaimed`, `lease.expired` | hub | `session`, `token` | info, warn for expired |
| `lease.reserved`, `lease.reservation_ended` | hub | `host`, `reason`, `until` | info, warn |
| `session.started`, `session.ended` | hub | `session`, `harness`, `reason` | info |
| `session.paused`, `session.fenced` | host | `why` | warn |
| `agent.spawned`, `agent.pruned` | hub | `by`, `harness`, `idle_days` | info |
| `placement.chosen`, `placement.refused` | hub | `host`, `harness`, `reason` (takeover, planned, spawn), `rejected: [{host, why}]` | info, warn |
| `move.requested`, `move.launched`, `move.ready`, `move.failed` | hub | `from_host`, `to_host`, `reason`, `error` | info, error |
| `move.stale_saved` | host | `git_ref` | warn |
| `claims.released` | hub | `tasks`, `after_secs` | warn |
| `task.status` | hub | `from`, `to`, `claimed_by`, `attention_msg` | info, warn for `stuck` and `needs_human` |
| `delivery.ack_failed`, `delivery.append_failed`, `delivery.redelivered`, `delivery.wake_limited`, `delivery.bad_envelope` | host | `message`, `deliveries`, `error`, `count` | warn |
| `usage.budget` | hub | `scope`, `spent_usd`, `limit_usd`, `level` | warn, error |
| `alert.fired`, `alert.resolved` | hub | `rule`, `subject` | warn |

Rules:

1. Lease renewals and successful deliveries make no events. The metrics count
   them.
2. A host emits at most one `delivery.*` event for each agent and kind in a
   minute. The `count` field carries the number in that minute.
3. `task.status` fires on a status or claim change, not on a property-only
   update. The task events on `borant.chan.{ch}.tasks` stay, because the
   inbox and wake rules use them.
4. The hub publishes after its write. A crash between the two loses one
   event, as section 6 of the main design accepts.
5. A reader ignores unknown fields, so the orchestration design can add
   fields to `placement.*`.

Two log changes ship with Phase 1. First, "ack failed", "cannot log message
yet", and "request failed" go from `debug` to `warn`, with the same
one-minute limit. "request failed" stays `debug` for the codes `Invalid`,
`NotFound`, and `Conflict`, because agents cause those in normal work. Second,
the `seen` set in `delivery.rs` keeps the last 10,000 IDs for each agent.

### Host stats and inbox stats

`HostStats` in the heartbeat holds `version`, `started`, `load1`, `cpus`,
`mem_used_pct` (from `sysinfo`, already a dependency), `outbox_depth`,
`last_heartbeat_rtt_ms` (measured on the previous heartbeat), and
`nats_reconnects` (a delta from the async-nats event callback). It also holds
one `SessionStats` for each session: `agent`, `busy`, `paused`, `turn`,
`last_event_at`, and deltas for `wakes`, `wake_limited`, delivery anomalies
by kind, and LLM usage.

The bus port gets one neutral function, with no NATS type:

```rust
async fn inbox_stats(&self, agent: &AgentId) -> PortResult<InboxStats>;
pub struct InboxStats { pub pending: u64, pub ack_pending: u64, pub redelivered: u64 }
```

The NATS adapter fills it from the consumer info of `inbox-{agent}`
(`num_pending`, `num_ack_pending`, `num_redelivered`).

### Metric catalog

The hub serves all metrics on `/metrics` in Prometheus text format, with the
`prometheus-client` crate (0.25.1). Host values arrive in the heartbeat, so
one scrape target covers the swarm. A host value carries its age, so a
sleeping laptop shows as old data, not as a gap.

| Metric | Type | Labels | Source |
|---|---|---|---|
| `borant_hub_start_time_seconds` | gauge | `instance` | hub. `changes()` counts restarts. |
| `borant_hub_requests_total` | counter | `op`, `code` | hub, `calls.rs` |
| `borant_hub_request_duration_seconds`, `borant_hub_tick_duration_seconds` | histogram | `op` | hub |
| `borant_events_total` | counter | `kind` | hub |
| `borant_host_online` | gauge | `host`, `always_on` | hub |
| `borant_host_heartbeat_age_seconds` | gauge | `host` | hub |
| `borant_host_heartbeat_rtt_seconds`, `borant_host_load1`, `borant_host_mem_used_ratio`, `borant_host_outbox_depth` | gauge | `host` | host stats |
| `borant_host_agents`, `borant_host_max_agents` | gauge | `host` | hub |
| `borant_host_nats_reconnects_total` | counter | `host` | host stats |
| `borant_agent_lease_state` | gauge (0 or 1) | `agent`, `state` (held, expired, reserved, released) | hub |
| `borant_agent_lease_state_age_seconds` | gauge | `agent` | hub |
| `borant_agent_busy`, `borant_agent_last_event_age_seconds` | gauge | `agent` | host stats |
| `borant_agent_turns_total` | counter | `agent`, `harness` | host stats |
| `borant_agent_wakes_total`, `borant_agent_wake_limited_total` | counter | `agent` | host stats |
| `borant_inbox_pending`, `borant_inbox_ack_pending`, `borant_inbox_redelivered` | gauge | `agent` | hub, `inbox_stats` |
| `borant_delivery_anomalies_total` | counter | `host`, `kind` | host stats |
| `borant_moves_total` | counter | `reason`, `outcome` (launched, failed, no_host) | hub |
| `borant_tasks`, `borant_tasks_ready` | gauge | `channel`, `status` | hub |
| `borant_llm_tokens_total` | counter | `agent`, `channel`, `model`, `type` (input, output, cache_read, cache_write) | host stats |
| `borant_llm_cost_usd_total` | counter | `agent`, `channel`, `model` | host stats |
| `borant_registry_agents` | gauge | `state` (live, parked, prunable) | hub |

Agent IDs as labels are safe at this scale. The swarm has tens of agents, and
pruning keeps the registry under a few hundred. Each agent has about 15
series, and the token metric multiplies by about two models and four types.
The total stays under 10,000 series. Labels never hold a task, session, or
message ID. Those go in events.

For NATS itself, turn on the monitoring port (`http_port: 8222`), bound to
`127.0.0.1` on borant-1. Phase 6 scrapes it with `prometheus-nats-exporter`
(`-varz -jsz`). Until then, `inbox_stats` covers the inbox counters.

### Views

`borant events` tails the stream:

```
borant events [--follow] [--since 1h] [--kind lease.*] [--agent X]
              [--channel C] [--host H] [--severity warn] [--json]
```

The host daemon gets `GET /v1/events?since=&follow=&filter=` on `host.sock`.
It reads `BORANT_EVENTS` with an ordered consumer and streams SSE, as
`/v1/sessions/{id}/stream` does. The bus port gets `read_events(filter,
since) -> BoxStream<SwarmEvent>`.

`borant top` is a ratatui (0.30.2) screen. It calls a new `stats` op through
`/v1/call` every 2 seconds and tails `/v1/events` in the bottom panel. The
same `Snapshot` JSON backs `borant status --json`.

```
borant top   hub-1 on borant-1  up 3d4h  token 12  events 14/min    09:43:21
HOSTS     ONLINE  HB AGE  RTT    LOAD  AGENTS  OUTBOX  RECONN
borant-1  yes     3s      4ms    0.42  3/6     0       0
borant-2  yes     7s      38ms   1.10  4/6     0       2
laptop-a  no      2h13m   -      -     0/4     -       -
AGENTS       HARNESS  HOST      LEASE     AGE   TURN  STATE  INBOX p/a/r  CLAIM       $TODAY
api-owner    claude   borant-1  held      41m   37    busy   0/0/0        -           4.12
pi-worker-3  omp      borant-2  held      12m   9     idle   2/0/0        #api T..XK  0.31
pi-worker-4  omp      borant-1  reserved  22s   -     -      1/1/3        #api T..9Q  0.18
CHANNELS  OWNER      OPEN  READY  RUN  STUCK  HUMAN  CLOSED  $TODAY
#api      api-owner  4     2      2    1      0      11      4.61
EVENTS
09:43:04 info  move.launched      pi-worker-4  borant-2 -> borant-1
09:42:59 info  placement.chosen   pi-worker-4  borant-1 (takeover, 2 candidates)
09:40:58 warn  lease.expired      pi-worker-4  borant-2 (no heartbeat for 30s)
q quit   tab panel   / filter   enter detail   e events only
```

The web dashboard (Phase 5) runs in the hub on axum (0.8.9, already a
workspace dependency). It binds to the Tailscale address only:

```toml
# hub.toml
[http]
listen = "100.101.102.103:7420"   # the Tailscale IP of borant-1
```

| Route | Content |
|---|---|
| `GET /` | One HTML page with inline JS, embedded in the binary. No build step. |
| `GET /api/snapshot` | The `Snapshot` JSON |
| `GET /api/events?since=&follow=1` | SSE of swarm events |
| `GET /api/channels/{ch}/graph` | Tasks with `blocked_by`, status, and a `stage` (the longest path from a root) |
| `GET /metrics` | Prometheus text |
| `GET /healthz` | 200 when the hub holds `lease.hub` |

The page is read-only, with four panels: hosts, agents, the channel graph,
and the timeline. The graph draws one column for each stage, with cards
colored by status, so it needs no graph library:

```
#api   ready 2   running 2   stuck 1
stage 0            stage 1                 stage 2
[closed] schema    [running] api handlers  [open] e2e tests
[closed] models    [stuck]   migrations    [open] docs
```

The NixOS configuration opens the port on `tailscale0` only:
`networking.firewall.interfaces.tailscale0.allowedTCPPorts = [ 7420 ]`.

History (Phase 6, optional) runs on borant-1 with modules from nixpkgs
26.05: `services.victoriametrics` scrapes the hub `/metrics` and
`services.prometheus.exporters.nats`, and `services.grafana` gets one
provisioned datasource and two dashboards from `deploy/grafana/` ("Swarm" and
"LLM spend"). The NATS Surveyor dashboard covers NATS. VictoriaMetrics over
Prometheus is a call: it is one binary with less memory and disk on a small
VPS. Its default retention is 31 days. Grafana alerting is not used, so
alerts work without Phase 6.

### Alerts and notifications

The hub evaluates a few fixed rules on its snapshot every 15 seconds, with
thresholds in `hub.toml`. A rule fires once for each subject and stays quiet
until it resolves. Each change publishes `alert.fired` or `alert.resolved`.

| Rule | Condition (defaults) | Priority |
|---|---|---|
| `task_attention` | A task enters `needs_human` or `stuck` | high |
| `agent_stalled` | Lease held, agent busy, and the last session event is older than 20 min | high |
| `agent_unplaced` | Lease expired, the agent has a claim, and no placement succeeded 5 min after the grace period | high |
| `host_offline` | An `always_on` host has no heartbeat for 2 min. Laptops are excluded, because they sleep. | high |
| `inbox_stuck` | `ack_pending` above 0 for 5 min, `redelivered` grows twice in 10 min, or `pending` above 50 | default |
| `budget` | Spend today reaches 80% or 100% of a daily limit | default, then high |
| `hub_restarted` | `hub.started` with a previous instance | low |

```toml
[alerts]
notifier = "ntfy"
ntfy_url = "https://ntfy.sh/borant-<random>"   # or a self-hosted server
ntfy_token_file = "/run/secrets/ntfy-token"    # optional, see the secrets design
stalled_after = "20m"

[budget]
daily_usd = 25.0
channel_daily_usd = { api = 10.0 }
```

The Notifier port stays `notify(title, body)`. Two changes:

1. Add an `NtfyNotifier` adapter: one HTTP POST with `X-Priority`, `X-Tags`,
   and a click link to the dashboard. ntfy is free, can be self-hosted, and
   has an iOS app. Pushover ($4.99 one time for each platform) and Slack
   webhooks are each one more small adapter later.
2. Give a notifier to the hub, which owns the rules. The macOS desktop
   notifier stays for `task_attention`.

The hub cannot report its own death. Therefore the host daemon on an
always-on host (borant-2) sends `hub.unreachable` through its own
`NtfyNotifier` after 2 minutes without a heartbeat reply. The host already
detects this state before it pauses sessions.

### Cost and usage accounting

Recommendation: each harness adapter reports usage to its host daemon, the
heartbeat carries the deltas, and the hub stores daily totals.

| Harness | Source | Cost | Status |
|---|---|---|---|
| Pi | `message.usage` on the `message_end` extension event: `input`, `output`, `cacheRead`, `cacheWrite`, `cost.total` | From the Pi model catalog | Documented |
| omp | The same usage shape in its session records | Same as Pi | The event payload is not verified |
| Claude Code | OpenTelemetry log event `claude_code.api_request`: `model`, `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `cost_usd`, with `session.id` | An estimate at Anthropic list price | Documented |
| DeepSeek API | `usage` in each response: `prompt_cache_hit_tokens`, `prompt_cache_miss_tokens`, `completion_tokens` | None. Use the price table. | Documented |
| LunaRoute | `github.com/erans/lunaroute` returns 404, and a listing marks it discontinued. A hosted LunaRoute has a `get_usage` MCP tool. | Unknown | Not verified |

Claude Code hooks carry no token or cost field. The statusline input has
`cost.total_cost_usd`, but it conflicts with Chris's own statusline. Parsing
`transcript_path` breaks the anti-goal against native transcript formats.
Therefore Claude Code usage comes from OpenTelemetry, which is a documented
standard:

1. The host daemon listens on `127.0.0.1:4318` and accepts only `POST
   /v1/logs` in OTLP `http/json`. It keeps `claude_code.api_request` records
   and drops the rest. This is about 150 lines of `serde_json`, with no
   OpenTelemetry crate.
2. The launcher sets the variables below for each Claude Code session that it
   starts. For sessions that Chris starts, he sets them in the `env` block of
   his Claude Code settings.
3. The daemon maps a record to an agent by `session.id`, which is the harness
   session ID that Borant already stores. The resource attribute is a
   fallback.

```sh
CLAUDE_CODE_ENABLE_TELEMETRY=1
OTEL_LOGS_EXPORTER=otlp
OTEL_EXPORTER_OTLP_PROTOCOL=http/json
OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://127.0.0.1:4318/v1/logs
OTEL_RESOURCE_ATTRIBUTES=borant.agent=<agent_id>
```

The Pi and omp extension sends a new `LocalEvent::Usage { model, input,
output, cache_read, cache_write, cost_usd }` on `message_end`. A `usage` row
also goes into the session log.

Prices: the hub uses the harness cost if it is present, and a price table in
`hub.toml` overrides it for each model. DeepSeek on 2026-10-24, per million
tokens, off-peak (peak hours double each price):

| Model | Cache hit | Cache miss | Output |
|---|---|---|---|
| `deepseek-flash` | $0.003 | $0.15 | $0.60 |
| `deepseek-v4-pro` | $0.022 | $0.66 | $1.98 |

The default table uses peak prices, so the budget alert errs high.

Storage: the hub writes one KV document for each day and agent,
`usage.{yyyy-mm-dd}.{agent}`, shaped `{channel: {model: {input, output,
cache_read, cache_write, cost_usd}}}`. The hub is the only writer, so it adds
deltas without races. The channel is the channel of the task that the agent
claims at that time, or else the first channel of the agent. Usage documents
stay for 400 days and outlive a pruned agent.

### Registry hygiene

The hub prunes on its tick:

| State | Definition | Action |
|---|---|---|
| live | The lease is held or reserved | Keep |
| parked | Lease expired or released, and the agent has a claim, undelivered messages, an open channel that it owns, or a session in the last 7 days | Keep |
| prunable | None of the above | Delete |

A delete removes `agent.{id}`, its `session.*` records, `lease.agent.{id}`,
its `chan.*.member.{id}` keys, the `inbox-{id}` consumer, and its session log.
It publishes `agent.pruned`. The hub also deletes ended sessions older than 7
days, except the last session of each agent. For test runs:

```
borant agents prune [--older 1h] [--dry-run]
borant agents rm <agent> [--force]
```

| Data | Retention |
|---|---|
| `BORANT_EVENTS` | 14 days, 512 MiB |
| `BORANT_MSG` | 30 days (no change) |
| `BORANT_SESSION` | Last 50 turns for each agent (no change), deleted with a pruned agent, plus a 2 GiB `max_bytes` backstop |
| Usage KV | 400 days |
| VictoriaMetrics (Phase 6) | 31 days |

## Phased plan

Each phase ships alone and is useful alone.

| Phase | Content | Done when |
|---|---|---|
| 1 | `BORANT_EVENTS`, `SwarmEvent`, hub events (lease, host, placement, move, task status), host events (`delivery.*`, `session.paused`), `borant events --follow`, the log changes, agent pruning | A two-host takeover shows `lease.expired`, `placement.chosen`, and `move.launched` in `borant events --follow`, and `borant agents prune --dry-run` lists the dead test agents |
| 2 | `HostStats`, `inbox_stats`, the hub `Snapshot`, the `stats` op, `borant top`, `borant status --json` | The redelivery bug from the stress test shows in the INBOX column without `natscli` |
| 3 | Alert rules, `NtfyNotifier` on the hub and on always-on hosts, `hub.unreachable` | Stopping the host daemon on borant-2 sends one phone notification in under 3 minutes |
| 4 | Usage from Pi, omp, and Claude Code, the usage KV, the price table, `$TODAY` columns, budget alerts | One day of real work gives a cost for each agent within 10% of the provider console |
| 5 | The hub HTTP listener on Tailscale, the web page, `/metrics` | The phone on Tailscale shows the dashboard and the task graph of a live channel |
| 6 | VictoriaMetrics, the NATS exporter, and Grafana on borant-1 | The "LLM spend" dashboard shows 7 days of history |

Tests: focused tests for the rule code (alert rules, prune rules, the stage
computation, OTLP parsing) and one Docker multi-host scenario for Phase 1.
The full suite runs in CI only.

What this design cuts:

- OpenTelemetry export from Borant, traces, and Sentry
- Grafana alerting, alert rules in a file, quiet hours, escalation, and acks
  from the phone
- Actions in the web dashboard, such as move, drain, or send
- Exact cost for each task, and exact DeepSeek peak and off-peak pricing
- A metrics endpoint on each host daemon. The heartbeat carries host stats.
- Provider billing APIs from Anthropic or DeepSeek. Not researched.

## Risks

| Risk | Effect | Mitigation |
|---|---|---|
| `inbox_stats` load | One JetStream API call for each agent every 15 s | Fine at tens of agents. Batch or slow down above 200. |
| Claude Code renames its OTel events | Claude Code cost drops to zero | The parser logs `warn` once for each unknown shape. A busy agent with no usage for 1 hour raises `budget`. |
| Harness cost is wrong for a proxied model | Claude Code prices a non-Anthropic model at Anthropic list price | The price table overrides the harness cost for each model. |
| The hub is the only reader | The views die with the hub | The always-on host alert, and 14 days of events after the restart |
| Prune deletes a needed agent | The agent loses its log and inbox | The parked rules, 7 days, and `--dry-run` |

## Open questions for Chris

1. Does Claude Code run on a Max subscription or on API keys? With a
   subscription, its cost is notional, and the budget counts only DeepSeek.
2. Which LunaRoute do you run, and does it record usage or expose metrics?
   The open-source repo is gone. Also, does your omp model configuration set
   a cost for DeepSeek, so that `message.usage.cost` is real?
3. For the phone: hosted ntfy.sh with a secret topic, ntfy self-hosted on
   borant-1, or Pushover for $4.99? A self-hosted ntfy needs the phone on
   Tailscale, and iOS push needs the `upstream-base-url` setting.
4. Over budget, is an alert enough, or must the hub also stop wakes for that
   channel?
5. Is 7 days the right prune window? Must a spawned worker be deleted at once
   when it releases its lease with no claims?
6. Do you want history over weeks (Phase 6), or are the snapshot and 14 days
   of events enough for now?

## Sources

Code facts come from the Borant repository at `v0.1.0-6-g4352933`. Web facts
were read on 2026-10-24.

- Claude Code OpenTelemetry: https://code.claude.com/docs/en/monitoring-usage
- Claude Code hooks (read in part, Stop payload not verified):
  https://code.claude.com/docs/en/hooks
- Claude Code statusline: https://code.claude.com/docs/en/statusline
- Pi session format and usage:
  https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md,
  https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/message-types.md
- omp sessions: https://github.com/can1357/oh-my-pi/blob/main/docs/session.md
- LunaRoute: https://github.com/erans/lunaroute (404),
  https://www.libhunt.com/r/lunaroute,
  https://github.com/lunaroute/lunaroute-pi-extension
- DeepSeek: https://api-docs.deepseek.com/api/create-chat-completion,
  https://api-docs.deepseek.com/quick_start/pricing
- NATS monitoring and advisories:
  https://docs.nats.io/running-a-nats-service/nats_admin/monitoring,
  https://docs.nats.io/running-a-nats-service/nats_admin/monitoring/monitoring_jetstream,
  https://docs.nats.io/reference/jetstream/advisory
- NATS exporters: https://github.com/nats-io/prometheus-nats-exporter,
  https://github.com/nats-io/nats-surveyor
- NixOS modules, release-26.05 branch: https://github.com/NixOS/nixpkgs
  (`services/monitoring/prometheus/`, `services/monitoring/grafana.nix`,
  `services/databases/victoriametrics.nix`, `services/misc/ntfy-sh.nix`)
- VictoriaMetrics:
  https://docs.victoriametrics.com/victoriametrics/single-server-victoriametrics/
- ntfy: https://docs.ntfy.sh/publish/, https://docs.ntfy.sh/config/
- Pushover: https://pushover.net/pricing
- Crates: https://crates.io/crates/ratatui,
  https://crates.io/crates/prometheus-client, https://crates.io/crates/axum

Not verified: the omp `message_end` payload, any LunaRoute usage API, the
price of hosted ntfy.sh, Slack webhook limits, and the async-nats reconnect
callback name.
