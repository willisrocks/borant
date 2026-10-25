# Phase 2: see the swarm

This spec builds roadmap phase 2: observability phases 1 to 3 of the
[observability design](2026-10-24-observability-design.md) ("the
observability doc"). The swarm gets an event stream, `borant events`, a live
view (`borant top` and `borant status --json`), and alerts to a phone. It also
picks up the alerts that phases 1 and 4 deferred.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Goals

1. Every lease, host, placement, move, task status, park, elastic, and member
   change is one `SwarmEvent` in a durable stream. `borant events --follow`
   shows them live, with filters.
2. `borant top` and `borant status --json` show one consistent view of the
   swarm: hosts, agents, inbox depth, and redeliveries.
3. Alerts reach a phone for: a task that needs a human, a stalled agent, an
   offline host, the hub down, every member of a bundle limited, a budget
   stop, and a provisioner that refuses (`denied` or `invalid`).
4. The core names no vendor. The phone path is an exec plugin.

## Cuts

- `usage.budget` events and dollar budgets for LLM use (observability phase
  4).
- The web page (observability phase 5).
- `nats.*` events. NATS health shows as `hub.*` and `host.*` events.
- Hub-side delivery stats beyond `inbox_stats`.

## Constraints

- Strict ports and adapters. The stream name, the dedupe header, and the
  consumer live only in `borant-nats`. ntfy, Pushover, and Slack live in
  `borant-infra` plugins.
- The hub never holds its `mem` mutex across a publish or a notify call.
- An event is published after the write it reports. Readers ignore unknown
  fields and unknown kinds.
- No event for a lease renewal. At most one `delivery.*` event per agent and
  kind per minute.
- `CtlRequest::Snapshot` already means the hub backup. The live view is named
  `SwarmView`, and its request is `ToolCall::View`.

## Interfaces

### Events (`crates/borant-core/src/events.rs`)

As in the observability doc ("The event stream" and its kind table), with
these names:

- `SwarmEvent { id: EventId, at: Timestamp, source: EventSource, severity: Severity, body: SwarmEventBody }`.
- `EventSource { Hub, Host(HostName) }`. `Severity { Info, Warn, Error }`.
- `SwarmEventBody`, a serde enum tagged `kind`, with the kinds of the
  observability doc table except `nats.*` and `usage.budget`, plus
  `agent.parked`, `elastic.created`, `elastic.deleted`, `member.limited`,
  `member.reset`, and `alert.fired` / `alert.resolved`.
- `EventFilter { since: Option<Timestamp>, kinds: Vec<String>, agent, channel, host, min_severity }`.
- `pub fn subjects::events(kind: &str) -> String` in `keys.rs`.

### Ports

- A new `EventLog` port in `crates/borant-core/src/ports/events.rs`:
  `async fn publish(&self, e: &SwarmEvent) -> PortResult<()>` (idempotent by
  `id`) and `async fn read(&self, filter: &EventFilter, follow: bool) -> PortResult<BoxStream<'static, SwarmEvent>>`.
  `borant-core::testing` gets `event_log_contract`. `borant-memory` and
  `borant-nats` implement it.
- `MessageBus::inbox_stats(&self, agent) -> PortResult<InboxStats>` with
  `InboxStats { pending, ack_pending, redelivered }`.
- `Notifier` changes to `async fn notify(&self, n: &Notice) -> PortResult<()>`
  with `Notice { title, body, priority: Priority, tags: Vec<String>, link: Option<String> }`
  and `Priority { Low, Default, High, Urgent }`. The desktop adapter maps it.

### Records and protocol

- `HostStats` and `SessionStats` of the observability doc, sent in the
  heartbeat (`#[serde(default)]`).
- `SessionLeaseRef.last_event_at: Option<Timestamp>`: the time of the last
  session event, for the stall rule.
- `ToolCall::View` returns `SwarmView { hosts, agents, inboxes, at }`.
  `borant status --json` prints it. `borant top` polls it every 2 s.
- `[alerts]` in `hub.toml` and `host.toml`:

```toml
[alerts]
notifier = "phone"          # a name of [notifiers.<name>]
watch_hub = false           # the laptop sets true: it sends hub.unreachable
needs_human = true
stall_minutes = 30
host_offline_minutes = 5
hub_down_minutes = 2

[notifiers.phone]
adapter = "exec"
command = ["borant-notify-ntfy"]
credential = "notify-token"
```

## Rules

### Alerts (`crates/borant-core/src/rules/alerts.rs`)

A pure function from the view, the open alerts, and the time to a list of
`Fire { key, notice }` and `Resolve { key }`. One key per subject, so a rule
fires once and resolves once. The rules:

| Rule | Fires when | Not for |
|---|---|---|
| needs_human | a task enters `needs_human` | nothing |
| agent_stalled | a busy session (`idle_since = None`) has no session event for `stall_minutes`. The hub also sets its claimed task to `stuck`, in the same pass | pinned agents, parked agents |
| host_offline | a host with `always_on` has no heartbeat for `host_offline_minutes` | pool hosts (`pool` set), the restart grace period of the hub |
| members_limited | every member of a bundle is limited | resolves when one member resets |
| budget_stop | a pool needs a host and a budget blocks it | nothing |
| provisioner_refused | a create or delete returns `denied` or `invalid` | other errors (a retry handles them) |

`hub.unreachable` is not a hub rule. The host with `watch_hub = true` sends it
through its own notifier after `hub_down_minutes` of failed heartbeats,
measured on a clock that counts sleep, so a laptop that just woke does not
alert at once.

## Tasks

| Task | Who | Blocked by | Files |
|---|---|---|---|
| Core interfaces for events, stats, and notices | worker | none | `crates/borant-core` (events.rs, ports, proto, keys, testing, rules/alerts.rs types), `crates/borant-memory` (EventLog, inbox_stats) |
| NATS event log and inbox stats | worker | Core interfaces | `crates/borant-nats` |
| Hub events | worker | Core interfaces | `crates/borant-hub/src/{leases,moves,tasks,park,elastic,members}.rs` and one event helper module |
| Host events and the events route | worker | Core interfaces | `crates/borant-host/src/{outbox,delivery,api,sessions}.rs` |
| `borant events` | worker | Core interfaces | `crates/borant/src/cli.rs`, a new `events.rs` |
| Swarm view and heartbeat stats | worker | Core interfaces | `crates/borant-host/src/lib.rs` (heartbeat), a new `crates/borant-hub/src/view.rs`, `calls.rs` (one arm) |
| `borant top` and `status --json` | worker | Swarm view | a new `crates/borant/src/top.rs`, `cli.rs` |
| Exec notifier | worker | Core interfaces | `crates/borant-exec/src/notify.rs`, the desktop adapter in `crates/borant-host/src/commands.rs` |
| Alert rules and the hub alert pass | worker | Core interfaces, Swarm view | `crates/borant-core/src/rules/alerts.rs`, a new `crates/borant-hub/src/alerts.rs`, the hub tick |
| Hub-down alert from the laptop | worker | Exec notifier | `crates/borant-host/src/lib.rs` (heartbeat failure path) |
| ntfy plugin | helper | Core interfaces (the `Notice` shape) | `borant-infra` |
| Release and deploy | helper, orchestrator | every code task | none |

Each code task: one test for each rule or behavior it owns, run with
`just test-crate <crate> <filter>`, then `cargo check --workspace --all-targets`,
`just fmt`, and `just lint`. The observability doc's "done when" lines are the
phase checks: a two-host takeover shows `lease.expired`, `placement.chosen`,
and `move.launched`; the INBOX column of `borant top` shows redeliveries; a
stopped host daemon on borant-2 sends one phone notification in under 3
minutes.

## Questions for Chris

1. The phone path (roadmap question 8). Default: hosted ntfy.sh with a long
   random topic name, through the exec plugin. You install the ntfy app and
   subscribe to the topic. Slack threads can plug in later as another exec
   plugin.

## Decision records

Decision: alerts go through a neutral `Notice` and an exec notifier, and the
ntfy client is a plugin in `borant-infra`. Evidence: the strict ports rule,
and the observability doc's `notifier = "ntfy"` and `ntfy_url` keys name a
vendor. Reversible: yes. Friend: none, follows the rule. Blocker: none.

Decision: the live view is `SwarmView` and `ToolCall::View`. Evidence:
`CtlRequest::Snapshot` already exists for backups. Reversible: yes. Friend:
none. Blocker: none.

Decision: pinned agents, parked agents, and pool hosts are excluded from the
stall, unplaced, and offline rules. Evidence: a pinned laptop chief keeps an
expired lease on purpose; a parked agent is released and idle; a deleted pool
host keeps its `host.{name}` key. Reversible: yes. Friend: none. Blocker: none.
