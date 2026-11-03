# Agent activity

Status: proposed for the chief mod v3 (design gate, 2026-11-02).

## Problem

The chief mod v3 shows, for each worker, when it last acted and what it
did: "last tool call 40s ago", and for a silent worker the last tool call
and its last text. With that, the person can tell a worker that thinks from
a worker that is dead. Borant has no such data. The stalled-card alarm
(`blocked_alarm.rs`) uses `task.updated`, which changes only when the task
record changes. A worker can run tools for an hour without a task update.

## Design

The host already sees every tool call: the Claude Code plugin calls
`borant hook post-tool-use`, and the Pi extension has the same hook. The host
keeps, per session, in memory:

- `last_tool_at`: the time of the last tool call.
- `last_tool`: the tool name and the first 80 characters of its main
  argument (the command of Bash, the path of Read or Edit). The redaction
  rules of `rules/redact.rs` apply first.
- `last_text_at` and `last_text`: the first 120 characters of the last
  assistant text, from the Stop hook. Redacted the same way.

The host sends these to the hub with the lease renewal that it already
makes, at most once each 30 s. The hub keeps them on the agent's status, and
`AgentStatus` gains the four fields. `borant status --json` and the board's
task lines (`claimed_by`) expose them.

"Silent" in the mod means: the task is claimed and in progress, the lease is
held, and `last_tool_at` is older than `[alerts] stall_minutes` (30 by
default). The stalled-card alarm uses `last_tool_at` in place of
`task.updated` when it has one.

### Additions after review

- PreToolUse marks a tool as running (`tool_running_since`); PostToolUse
  ends it. A worker with a tool running is "running a tool for 41m", not
  silent.
- No data is not silence: a session from before this change, or a harness
  without the hooks, shows "no activity data", and the mod never calls it
  silent.
- The session end cause and the relaunch state that the mod shows for an
  offline orchestrator come from the existing spawn record and `session.*`
  events, not from these fields.
- The stalled-card alarm's switch to `last_tool_at` is a separate change; it
  can ship later.

## Privacy

The text fields can hold project content. They stay on the hub, they go only
to callers that may read the agent's channel, and they are redacted before
they leave the host.

## Tests

- Host: the hook updates the fields, and the renewal carries them.
- Hub: a renewal stores them; a stalled card uses `last_tool_at`.
- Redaction: a token in a Bash command does not reach the hub.

## Cut

No full activity log: the session log (`borant logs`) already has one. Only
the last call and the last text are kept.
