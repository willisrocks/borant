# Phase 11: bounded task continuation

The "run until done" item of the roadmap's Later list, cut down. A worker
bound to one task gets a "continue" message when it goes idle while its task
is still in progress, a bounded number of times. It proves workflow state,
not correctness: the owner still reviews and closes the task. The
supervisor's brief is `.orchestrator/phase-11-continuation.md`; Codex
(gpt-6.1-sol, high) reviewed the scope twice.

## Goals

1. An agent spawned with `--continue N` (and bound to a task) gets at most N
   continues, each when it goes idle while its task is `in_progress` and
   claimed by it.
2. A worker that is finished calls the `wait` tool on its task (wait for
   closed), and the continues stop.
3. When the continues or the session hours run out, the task goes
   `needs_human`, and the agent never gets another continue.
4. The count survives a hub restart, a host restart, and a move, and one idle
   period never gets two continues.

## Cuts

- A default for the fleet, and any change to `hub.toml` roles. Enabling it
  for real work is Chris's decision.
- Continues for an agent without a bound task, or based on "all my claims
  closed" (a claim can vanish by release or expiry).
- Any reading of assistant text to guess completion.

## Constraints

- Only the hub writes control-plane state, so the hub decides each continue,
  writes the count into the agent record, and then sends the message (a
  direct message from the hub). The host keeps reporting idle in the
  heartbeat, as today. (The brief said "the host daemon sends"; the hub does
  it here, because the count lives in the agent record.)
- Every new field is `#[serde(default)]`.

## Interfaces

- `ToolCall::Spawn` gets `continues: Option<ContinuePolicy>` with
  `ContinuePolicy { max: u32, max_hours: Option<u32> }` (`max` at least 1).
  The hub refuses it with Invalid unless the spawn binds a task. CLI:
  `borant spawn ... --task C/ID --continue N [--continue-hours H]`; the MCP
  spawn tool passes it.
- `Agent.continues: Option<ContinueState>` with `ContinueState { policy,
  used: u32, last_idle: Option<Timestamp>, exhausted: bool }`.
- A pure rule in `borant-core::rules::continuation`:
  `decide(agent, task, idle_since, waiting_on_task, now) -> Decision` with
  `Decision::{Continue, Skip(why), Exhaust(why)}`. It continues only when all
  hold: the agent has a policy and is not exhausted; the agent is not paused,
  retired, or moving; `idle_since` is set and differs from `last_idle`; the
  bound task is `in_progress` and claimed by this agent; the agent has no
  stored wait on that task; and `used < max` and the session hours are under
  `max_hours`. It exhausts when the task is `in_progress` and claimed by the
  agent, the agent is idle, and `used == max` or the hours ran out. Every
  other case skips.
- The hub, on each heartbeat that reports a bound session idle: call
  `decide`; on Continue, write `used + 1` and `last_idle = idle_since` into
  the agent record first, then send "Continue with task <channel>/<id>. When
  it is done, call the wait tool on it; do not stop otherwise." On Exhaust,
  set `exhausted`, set the task `needs_human` with "continue limit reached
  (N)" or "continue hours reached (H)", and send nothing.
- The orchestrate skill's worker brief gets one line: a worker that is done
  calls the `wait` tool on its task for `closed`, and never closes it.

## Tasks

| Task | Blocked by | Files | Proof |
|---|---|---|---|
| Core: the rule and the types | none | `borant-core::rules::continuation`, model and proto fields | `just test-crate borant-core continuation` (each skip reason; continue; exhaust by count and by hours; the same idle period twice gives one continue; old Agent JSON parses) |
| Hub: continue on idle, CLI, MCP, and the skill line | core | `crates/borant-hub/src/continuation.rs`, the heartbeat path, CLI, MCP, the orchestrate skill | `just test-crate borant-hub continuation` (a continue per idle period; the record is written before the send; a wait stops it; exhaust sets needs_human; a spawn without a task is Invalid) |
| e2e `continue` | hub | `tests/e2e/run.sh`, the fake harness if needed | `just e2e continue`: idle -> continue -> the agent finishes and waits; a wait stops continues; max -> needs_human; a host restart and a move keep the count |

Each code task also runs `cargo check --workspace --all-targets`, `just fmt`,
and `just lint`.
