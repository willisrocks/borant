# Verifiable chief approvals and standing permissions

Status: proposed (2026-11-03), after the mod v3 data tasks.

## Problem

An orchestrator cannot tell a chief's relay of the person's decision from
any other agent message: every message from an agent is agent text, and the
hub marks all of it "not human input". So Chris approved the same steve-bot
actions four times, and orchestrators re-ask questions he already answered.
The standing permissions that the chiefs keep in their memory files reach no
orchestrator, and no worker brief.

## Design

### `approve`

A new call, `ToolCall::Approve { channel, task, quote }`: the CLI form
`borant approve <channel/id> --quote "<person's words>"`, and the MCP tool
`approve` with `channel`, `id`, and `quote`.

Only an agent whose role in `hub.toml` has `approves = true` may make it.
The call layer checks the role of the session and answers `denied` for any
other role, and `invalid` to a person's shell: a person answers instead. The
channel must be in the caller's collection, as every other call.

The hub:

1. Redacts the quote with the rule of `borant-core::rules::redact`, so a
   secret that the person typed never reaches the log or a message.
2. Records `task.approved { task, by, session, quote }`, with the channel and
   the task in the event context, before it sends anything.
3. Sends the task's claimer, or the channel owner when nobody holds the
   task, a direct message whose `from` is the new sender type
   `{"type": "approval", "id": "<chief>"}` and whose text is the redacted
   quote. Only the hub writes that sender, so a reader can tell an attested
   approval from an agent's claim of one.
4. If the task is in `needs_human`, answers it as a board answer does: the
   attention message clears, a claimed task returns to `in_progress`, the
   channel gets the usual task event, and the hub records `task.answered`
   with `via: chief` and `by` the approval sender.

The harness renders the message as its own kind:

```
[borant] approval from chief-1 on the person's word: do it for all three
(id 01J...)
```

An agent that reads it counts it as the person's decision within the scope of
the quote; plain agent text still does not. The borant:orchestrate skill
states that, and the chief-of-staff skill says to use `approve` when the
person has decided, quoting their words.

### `standing_permissions`

A new key of two `hub.toml` tables:

```toml
[collections.work]
standing_permissions = ["Merge PRs after green CI."]

[projects.steve-bot]
standing_permissions = ["Deploy on auto-pilot."]
```

It is config only: the hub stores nothing new. The permissions apply to a
spawn by the collection of its agent and the project of its channel, the
collection's first, without duplicates. The hub appends them to every spawn
brief, under the section title "Standing permissions from the person", after
the prompt and the skill line. `ToolCall::ProjectGet` returns the merged list
in `standing_permissions`, so an orchestrator reads it again after a
compaction, and `project_list` does not.

The `via` field of `task.answered` is new: `person` (the default, so an event
from before the field parses) or `chief`. The task history words a chief's
answer as "answered by the chief's approval".

## Tests

- `borant-hub approve`: a chief's approval answers a `needs_human` task,
  records `task.approved` with the redacted quote and the session, sends the
  claimer an `approval` message, and leaves the inbox. A worker's call is
  `denied`, a person's is `invalid`, and neither records anything. An
  approval of a working task leaves its status.
- `borant-hub spawns_the_brief_carries_the_standing_permissions`, and
  `project_get_returns_the_standing_permissions`.
- `borant-core roles`: the merge order, and the brief section.
- `borant config`: both tables parse, and an older `hub.toml` without the key
  parses with none.
- The event round trip lists `task.approved`, and `task.answered` with `via`.

## Out of scope

A bot GitHub identity for work PRs. The wording of the real `hub.toml` lives
in `borant-infra` and lands after this hub is deployed: an older hub refuses
an unknown key.
