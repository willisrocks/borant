# Changes for the person, and task history

Status: proposed for the chief mod v3 (design gate, 2026-11-02).

## Problem

The chief mod v3 lists "what changed since you last looked", by name: 3 new
tasks in steve, PR #2440 approved, PR #612 merged, v0.19.2 released. It also
shows, for a selected item, its history: who claimed it, when it asked,
what the orchestrator recommended, and what the person answered.

`borant events --since` returns raw swarm events: leases, sessions,
deliveries, and task status changes. They are too many and too low-level for
a person, and they do not hold the messages about a task.

## Design

### Changes

A new read, `borant changes --since <time> --json`, returns person-level
changes, newest first. The hub derives them from the event log with rules
in `borant-core`:

| Change | From |
|---|---|
| new tasks (grouped: one change per creator and channel within 5 minutes) | task creates |
| task done, with its PR if it has one | `task.status` to closed as done |
| PR approved, merged, or checks failed | `change.updated` |
| release | a task closed as done whose title starts with "Release" |
| gate closed (approved) | a saga gate closed as done |
| answered | `Answer` calls |
| spawn blocked or unblocked, orchestrator offline or back | `spawn.*`, `session.*` of an orchestrator |

Each change has `at`, `project`, `channel`, `task` (if any), `verb`, `text`,
and `by`. The mod keeps the "seen" time in `$.store`. Borant does not keep it,
because "seen" belongs to the person's view, not to the swarm.

### Task history

A new read, `borant task history <channel/id> --json`, returns, oldest
first: the task's status changes, its claims, its answers, and the direct
messages that name the task.

To name a task, `send` gains `--task <channel/id>` (and the MCP `send` tool
a `task` field). The mod's Ask sends with it, so the orchestrator's reply
names the task when it replies with the borant tools, which carry the task of
the message they answer. The chief skill tells orchestrators to reply that
way.

### Additions after review

- Each change carries the sequence number of its last event. The mod keeps
  the sequence of the last change that the person marked seen, not a wall
  time, so a late event is never lost.
- A group of new tasks never spans the seen mark: tasks created after the
  mark start a new group.
- One task gives one primary change: a closed gate shows as "approved", a
  release as "released", and the plain "done" for that task is left out.
- A release is a task closed as done with the property `release: true`. The
  title prefix "Release" is only a fallback.
- An answer becomes a durable event, `task.answered`, with the answer key, so
  "answered" in the feed and the history comes from the log.
- `task history` returns at most the last 50 entries.

## Tests

- `borant-core`: the change rules on a recorded event log, including the
  grouping of new tasks.
- Hub: `task history` returns tagged messages and leaves out others.

## Cut

No push of changes to the mod: the mod already follows `borant events
--follow` and reads `changes` after an event, as it reads the board today.
