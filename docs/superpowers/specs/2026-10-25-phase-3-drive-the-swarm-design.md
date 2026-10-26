# Phase 3, remaining items: drive the swarm

Roadmap phase 3 items 1, 2, and 6 (projects, `model` on spawn, the
`orchestrate` skill) shipped in v0.3.0. This spec builds the rest: items 3 to 5
of the roadmap, from phases 1 and 2 of the
[orchestration design](2026-10-24-orchestration-api-and-sandboxes-design.md)
("the orchestration doc"), adapted to tonight's roles, parking, elastic pools,
and events.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Goals

1. A person or a script spawns, waits for, cancels, and reads agents from the
   CLI: `borant spawn`, `borant wait`, `borant cancel`, `borant logs`, and
   `borant ps --queued`.
2. A retried spawn, task create, or task close with the same key does the work
   once.
3. A spawn that cannot be placed waits in a persisted queue that survives a
   hub restart, with its prompt.
4. A worker bound to a task stops and frees its slot when the task closes.
5. A launch that never registers (for example a Claude Code folder-trust
   screen) is stopped, its screen is reported, and the hub stops relaunching it
   after a few tries.

## Cuts

- A gateway for the CLI off a host (orchestration doc question 1). The CLI runs
  on a host and talks to its daemon, as today.
- A `human` channel owner (question 2). A person's spawn uses a channel that an
  agent owns.
- `limits.max_turns` and `max_minutes` (roadmap phase 6).
- `--repo` and `--branch` on spawn. A spawn uses the project of its channel.
- `logs --screen`. The watchdog report carries the screen of a stuck launch.

## Constraints

- Strict ports and adapters. `capture` on the `Launcher` port has a default
  body that returns `None`.
- New fields are `#[serde(default)]`. Adding `key` to `CtlRequest::Call`
  changes exhaustive patterns in `crates/borant-hub/src/lib.rs`.
- Names that already exist: `Hub::queue` (the event outbox), `roles::Resolved`,
  `schedule::PlacementReq`, `/v1/sessions/{id}/wait`. The new call is
  `ToolCall::SpawnQueue`, the module `crates/borant-hub/src/spawns.rs`, and the
  host route `GET /v1/wait/agent` and `GET /v1/wait/task`.

## Interfaces

- `ToolCall::Spawn` gets `key: Option<String>`, `task: Option<TaskBinding>`,
  `stop_on_close: Option<bool>`, and `priority: Option<i32>`.
  `TaskBinding { channel, id }` or `{ channel, title }` (create it).
  `allowed_for_human` allows `Spawn`. A spawn by a person takes the collection
  of its channel.
- `ToolCall::Cancel { agent, close_task: Option<CloseReason> }`,
  `ToolCall::SpawnQueue`, and `ToolCall::AgentHistory { agent, page }` (the
  session log of any agent, for `logs`; allowed for humans).
- `CtlRequest::Call` gets `key: Option<String>`.
- `SpawnRecord` at KV key `spawn.<agent>`: `spec`, `resolved` (the
  `roles::Resolved` at enqueue time), `by: Sender`, `queued_at`, `priority`,
  `state` (`Queued`, `Launching`, `Running`, `Failed`, `Cancelled`),
  `attempts`, `last_reject`, and `launch_failures` (the host and the timeout
  of each launch that did not register). Written only by the hub. A record
  goes when its agent retires or is cancelled.
- `Agent` gets `spawn_key`, `task: Option<TaskRef>`, `stop_on_close`, and
  `retired`. `Task` gets `create_key`.
- `events.rs`: `AgentSpawned.by` becomes a `Sender`, and new kinds
  `spawn.queued`, `spawn.cancelled`, `agent.retired`, and `launch.stuck`.
- `Launcher::capture(&self, handle, lines) -> Option<String>`, default `None`.
- Host config `launch_timeout_secs`, default 90.

## Rules

### Idempotency (`borant-core::rules`)

- A spawn with a `key` stores it as `Agent.spawn_key`. With no `name`, the
  agent name is a short hash of the key. A second spawn with the same key and
  the same spec returns the first agent; with a different spec it fails with
  `conflict`.
- A task create with a key stores `Task.create_key` the same way.
- `task_close` of a closed task with the same reason returns the task. A
  different reason fails with `conflict`.
- A repeated `cancel` succeeds.

### The persisted queue

`spawn.<agent>` is written before the first placement try. Every tick, the hub
places `Queued` records by priority, then `queued_at`. A record that does not
fit does not block the records after it. This replaces the spawn entries of
`Memory.pending_moves`. Planned moves keep their path. The elastic executor
takes its demand from `Queued` records plus planned moves. `role_taken` (the
one-per check) counts `Queued` and `Launching` records.

### Stop on close

A worker bound to a task gets `stop_on_close = true` unless the spawn says
false. When that task closes, the hub sets `Agent.retired`, then sends
`HostCommand::Stop` when the session is not busy, else at its next turn end.
The turn state comes from the `busy` of the session's `SessionStats` in the
heartbeat (set by `TurnStart`/`TurnEnd`) and from `idle_since`, never from
`idle_since` alone: it is `None` both for a session in a turn and for one that
never ended a turn, as an omp session that waits for its first prompt. A
session that the host reports no stats for has no known turn state, so the
stop waits a safety grace of 10 minutes and then goes out anyway. A cancel
reply says `stopped: true` only when the stop order went out; else it says
`stopping: "at the turn end"`. A retired agent is never placed again by a
takeover, a wake, a move, or the queue. Its claims on other tasks go back to
open, and its spawn record goes, so `borant ps` lists no retired agent. The
hub also drops the records of retired agents at start, for the ones that an
older hub left behind.

### The launch watchdog

The host records the start time of each launch. If the session does not
register within `launch_timeout_secs`, the host captures the last 40 screen
lines, redacts them, stops the launch, and sends `LaunchFailed` with the text
and the timeout it waited. The hub emits `launch.stuck` with the redacted
screen, adds the host to the agent's `failed_hosts`, and counts the try in
`SpawnRecord.attempts`, with the host of the try in `launch_failures`. After 3
failed tries, the record goes to `Failed`, the hub logs a `warn`, and the
spawner gets one direct message with the agent and the reason, for example
`3 launches did not register within 90s (borant-1, borant-2, borant-1)`. A
spawn by a person has no agent to tell. `borant ps` shows the reason in its
`REASON` column. A failure on a record that is `Failed` already counts
nothing and sends nothing. A host that still has a stale tmux session of the
agent kills it before a relaunch.

## Tasks

| Task | Who | Blocked by | Files |
|---|---|---|---|
| Core interfaces for spawn, queue, and watchdog | worker | none | `crates/borant-core` (proto, model, keys, events, ports/launch, rules) |
| Hub spawn queue and idempotent spawn | worker | Core interfaces | new `crates/borant-hub/src/spawns.rs`, `moves.rs`, `lib.rs`, `leases.rs`, `elastic.rs`, `prune.rs`, `calls.rs` |
| Stop on close and idempotent task calls | worker | Core interfaces | `crates/borant-hub/src/tasks.rs`, new `crates/borant-hub/src/retire.rs`, the `retired` guard in `place_agent` |
| Launch watchdog | worker | Core interfaces | `crates/borant-host/src/{commands,lib}.rs`, `crates/borant-launch/src/{tmux,orca,zellij}.rs`, the hub `launch_failed` path |
| CLI for spawn, wait, cancel, logs, ps | worker | Core interfaces | `crates/borant/src/cli.rs`, `crates/borant-host/src/api.rs` (wait routes), `delivery.rs` (AgentHistory) |
| Skill, guide, and e2e | worker | every other task | `integrations/claude-code/skills/orchestrate/SKILL.md`, `docs/guide.md`, a `tests/e2e` scenario: a queued spawn survives a hub restart |

Each code task: one test for each rule it owns
(`just test-crate <crate> <filter>`), then `cargo check --workspace --all-targets`,
`just fmt`, and `just lint`. The orchestration doc's phase checks are the phase
done test: a script spawns two omp workers and one Claude Code worker, a
retried spawn with the same key gives one agent, a queued spawn survives a hub
restart, and a worker frees its slot when its task closes.

## Decision records

Decision: `stop_on_close` is the default for a worker bound to a task.
Evidence: the orchestrate skill tells the owner that an idle worker keeps its
slot and no tool stops it; a bound worker has nothing left to do after its
task closes. Reversible: yes, per spawn. Friend: none, routine. Blocker: none.

Decision: the queue stores `roles::Resolved` at enqueue time. Evidence: a
spawn by a person has no parent, and a parent can be gone by the time the
queue places the record. Reversible: yes. Friend: none. Blocker: none.

Decision: the watchdog caps launch tries at 3. Evidence: tonight a helper
launch sat at a folder-trust screen and the hub relaunched it until the tmux
name clashed; nothing capped it. Reversible: yes. Friend: none. Blocker: none.
