# Phase 6: durable work

Roadmap phase 6 has five items: deadlines, retries, stall detection, and
`wait`; stop, resume, and caps; the human inbox; close gates; and the GitHub
PR watch. This spec builds them from items 2 to 6 of the
[durable orchestration vision](2026-10-24-durable-orchestration-vision-design.md)
("the vision doc"), cut to what the swarm needs after phases 2 and 3. Phase 2
built the `agent_stalled` alert and notices. Phase 3 built `borant wait`,
`stop_on_close`, retire, cancel, and the spawn queue.

The orchestrator wrote it in auto-pilot, with a second opinion from Codex
(conversation codex-297). The decision records are at the end.

## Goals

1. A task can carry a deadline and a retry limit. A missed deadline or a used
   up retry limit puts the task in front of its owner, and a task at its limit
   cannot be claimed again until a person or the owner opens it.
2. An agent can wait for a task state with a timeout through an MCP tool. The
   hub wakes it with a direct message, also after a hub restart.
3. A person stops and resumes one agent or a whole channel from the CLI. A
   stopped agent keeps its claims, gets no stall alert, and is never placed
   until it is resumed. Caps on turns per day and session hours stop an agent
   the same way.
4. A person sees every question of the swarm with `borant inbox` and answers
   one with `borant answer`. The answer reaches the agent that asked.
5. Only the owner of a channel closes a task as `done`.
6. When a task has a `pr_url`, the hub watches the pull request. A worker
   hears about failed checks and requested changes on its own PR without an
   orchestrator turn. The owner hears when the checks are green and when the
   PR merges.

## Cuts

- Test gates at `task_close` (vision item 5). See the decision record "No test
  gate at close".
- The general `idem.{key}` store. Spawn, task create, and task close already
  have keys (phase 3).
- Phone action buttons and the HTTP bridge on the Tailscale address. They wait
  for Chris's phone path (roadmap decision 8).
- Escalation rules for unanswered questions.
- Close on merge for the PR watch. The owner closes after its own review, as
  today. The watch tells the owner of the merge.

## Constraints

- Strict ports and adapters. The PR watch is a port with neutral types
  (`ChangeRequests`), and its adapter is an exec plugin. The core never names
  GitHub.
- The hub is the only writer. The PR watch polls outside the hub request lock,
  and a slow or failed poll never delays a lease renewal.
- Every new field is `#[serde(default)]`. A v0.8.0 host and a v0.9.0 hub work
  together during a deploy.

## Interfaces

### Tasks (`borant-core`)

- `Task` gets `deadline: Option<Timestamp>`, `attempts: u32`, and
  `max_attempts: Option<u32>`.
- An attempt is one claim that ended without a `done` close: the claim timed
  out, the claimer released it, or the task went back to `open` from
  `in_progress`, `stuck`, or `needs_human`. One claim counts once, even when a
  timeout and a reopen both follow it.
- At `attempts == max_attempts`, the hub sets `needs_human` with
  "retry limit reached (N tries)". `task_claim` of such a task fails with
  Conflict, until a `task_update` by the owner or a person sets `open`. That
  update resets `attempts` to 0.
- A passed deadline sets `stuck` with "deadline passed" once, and sends the
  owner a direct message. It reassigns nothing.
- `task_claim` of a task in `needs_human` or `stuck` by an agent other than the
  claimer fails with Conflict. This fixes the claim that overwrites a question
  today.
- `ChannelConfig` or the channel record gets nothing new: a channel owner is
  `Channel.owner` today.

### Close rule

- `task_close` with reason `done` is allowed for the channel owner and for a
  person. Another agent gets Denied, with "only the owner closes a task as
  done". Other reasons (`wontfix`, `duplicate`, `superseded`) stay open to
  members.

### Wait tool

- `ToolCall::Wait { channel, task, until: Vec<TaskStatus>, timeout_secs }`
  (`timeout_secs` required, at most 86400). The hub stores
  `wait.<agent>.<id>` with the predicate and the deadline, and answers at once
  with `{wait: id, satisfied: bool}` when the task already matches.
- The tick checks each stored wait against the current task (not only
  events), so a restart or a lost event does not lose a wake. A satisfied or
  timed out wait sends one direct message from the hub to the agent ("task X
  is now done" or "wait X timed out") and deletes the key.
- The MCP server lists a `wait` tool. The CLI `borant wait` stays as it is.

### Stop, resume, caps

- `Agent` gets `paused: Option<PauseReason>` with `PauseReason { by: Sender,
  why: String, at: Timestamp }`. `Channel` gets `paused: bool`.
- `ToolCall::Stop { agent | channel, grace_secs: Option<u32> }` and
  `ToolCall::Resume { agent | channel }`, allowed for a person and the
  channel owner. CLI: `borant stop <agent>|#<channel> [--grace 60]`,
  `borant resume <agent>|#<channel>`.
- Stop sets `paused`, and sends `HostCommand::Stop` at the next turn end
  (like retire), or after `grace_secs` (default 120) with `HostCommand::Fence`
  when the turn does not end. Claims stay. The lease ends as `Released`, not
  parked.
- A paused agent is skipped by every placement path (the same guards as
  retire: place_agent, moves, takeover, wake, spawn queue, park), by claim
  expiry, and by the `agent_stalled` rule. A paused channel takes no new spawn
  (Conflict "channel paused") and pauses each member whose only channel it
  is. A member of several channels pauses only when all its channels pause.
- Resume clears `paused` and places the agent again with its last prompt
  context (a planned move, reason `Resume`).
- Caps in a role of `hub.toml`: `max_turns_per_day` and `max_session_hours`.
  The host already reports `SessionStats.turn`. The hub keeps per agent a
  `turns.<agent>.<UTC date>` counter and the session start, both in KV, so a
  move or a restart keeps them. At a cap, the hub stops the agent with
  `PauseReason.why = "cap: 200 turns today"`, sets `needs_human` on its claim
  (or, with no claim, adds an inbox item, see below), and sends one notice.
- `max_agents` per channel: `ChannelConfig`/`hub.toml` `[channels.<name>]
  max_agents`. A spawn past it queues (phase 3 queue) with the reject reason
  "channel full (4/4)".

### Human inbox

- `ToolCall::Inbox` (person only) lists: tasks in `needs_human` or `stuck` in
  every channel, with channel, title, claimer, `attention_msg`, and age; and
  paused agents with no claim, with `PauseReason.why`. CLI `borant inbox
  [--json]`.
- `ToolCall::Answer { channel, task, text, expect_updated: Timestamp, key }`
  (person only), one hub operation: if the task changed after
  `expect_updated`, Conflict. If a claimer holds it, set `in_progress`, clear
  `attention_msg`, and send the claimer a direct message from `human` with the
  text. If nobody holds it, send the text to the channel owner and leave the
  status. An answer never clears a pause or a retry limit. CLI `borant answer
  <channel>/<id> <text>`.
- The hub `needs_human` notice body gets the `attention_msg`.

### PR watch

- Port `borant-core::ports::ChangeRequests`: `async fn status(&self, url:
  &str) -> PortResult<ChangeStatus>` with `ChangeStatus { state: Open |
  Merged | Closed, head: String, checks: Pass | Fail { failed: Vec<String> } |
  Pending | None, review: Approved | ChangesRequested | None }`. `checks:
  None` means no check reported for that head, not a pass. A contract suite in
  `borant-core::testing` and a memory adapter.
- The `exec` adapter in `borant-exec` (op `status`, describe kind
  `change_requests`). The plugin `borant-changes-github` in `borant-infra`
  uses `gh pr view --json state,headRefOid,statusCheckRollup,reviewDecision`
  with the token of the hub's bundle.
- `hub.toml` `[changes] adapter = "exec"`, `program`, `every_secs = 60`.
- The hub runs the watch on its own task, outside the request lock: every
  `every_secs`, for each open or in-progress task with `pr_url`, call
  `status`, compare with the last state at `pr.<channel>.<task>` (KV, keyed by
  head SHA), and on a change:
  - `checks: Fail` or `review: ChangesRequested` for the current head: a
    direct message to the claimer, once per head.
  - `checks: Pass` and not yet told for this head: a direct message to the
    owner ("PR #N green at <sha>").
  - `Merged` or `Closed`: a direct message to the owner, once.
  - Each change publishes a `change.updated` event.

## Tasks

| Task | Owner | Blocked by | Files |
|---|---|---|---|
| Core interfaces | worker | none | `borant-core` model (Task fields, Agent.paused, Channel.paused, PauseReason), proto (Wait, Stop, Resume, Inbox, Answer), events (change.updated, agent.paused, agent.resumed), `ChangeRequests` port, contract, memory adapter, role caps, `[channels]`, `[changes]` config types; hub answers the new calls with Invalid "not built yet" |
| Deadlines, attempts, close rule, claim guard | worker | core | `crates/borant-hub/src/tasks.rs`, the tick |
| Wait tool | worker | core | new `crates/borant-hub/src/waits.rs`, MCP tool |
| Stop, resume, and caps | worker | core | new `crates/borant-hub/src/pause.rs`, guards next to the retire guards, CLI |
| Human inbox and answer | worker | core | new `crates/borant-hub/src/inbox.rs`, CLI, alert notice body |
| PR watch: exec adapter and hub poller | worker | core | `crates/borant-exec/src/changes.rs`, new `crates/borant-hub/src/changes.rs`, config |
| PR watch plugin | helper | core | `borant-infra/pkgs/borant-changes-github.*` |
| Release, deploy, and the orchestrate skill | orchestrator | all | skill rows: owner-only done, the wait tool, the PR watch messages |

Each code task proves itself with a focused test on the memory adapters:
`just test-crate borant-hub attempts` (one count per claim; the limit blocks a
claim; an owner reopen resets), `... deadline`, `... close_rule` (a member
cannot close done; the owner and a person can), `... waits` (satisfied at
once; a wake after a restart that lost the event; timeout), `... pause` (stop
at turn end; fence after the grace; no placement, no claim expiry, no stall
alert while paused; resume places; a channel pause blocks a spawn; a cap pauses
and marks the claim), `... inbox` (answer with a stale `expect_updated` is
Conflict; no claimer goes to the owner), `... changes` (fail goes to the
claimer once per head; green to the owner once; `checks: None` is not a pass;
a slow adapter does not block a lease renewal).

## Decision records

### No test gate at close

Decision: no command runs at `task_close`. Close the gap with the owner-only
`done` rule instead.
Evidence: the owner closes a task after the merge, so a gate at close runs
after the code is on main. A worker's focused test in its worktree does not
prove the combined branch, and CI plus the owner's combined check already
gate the merge. `Project.test` has no runner, so a gate needs host execution,
timeouts, and result storage for little gain. Codex agreed and proposed the
owner-only rule. On 2026-10-25 a worker merged its own PR and closed its own
task, which the rule now blocks at the hub.
Reversible: yes. A gate can come back as a `ready` check.

### PR watch messages go to the claimer and the owner

Decision: failures and requested changes go to the claimer; green and merge go
to the owner.
Evidence: the vision doc's main gain is that a worker fixes its CI without an
owner turn (Codex). The owner still decides the merge.
Reversible: yes.

### A port with an exec adapter for the PR watch

Decision: `ChangeRequests` port, exec adapter, plugin in `borant-infra`.
Evidence: the strict ports rule; the plugin keeps the GitHub token and `gh` out
of the hub binary; a public crate adapter can come later if a second user
needs it.
Reversible: yes.

### Stop at the turn end, fence after a grace

Decision: stop waits for the turn end, then fences after `grace_secs`.
Evidence: `HostCommand::Fence` kills the process at once, which loses the work
of the turn. A hung turn still needs a bound (Codex).
Reversible: yes.
