# Actions from the chief mod

Status: proposed for the chief mod v3 (design gate, 2026-11-02).

## Problem

The chief mod v3 lets the person act on an item with one key: answer,
approve, decline, close as done, ask the orchestrator, message the worker,
open the PR, go to the agent, retry or resume, and spawn without a missing
bundle. Most of these exist as CLI commands. Four do not, or do not give the
mod what it needs.

## What exists and is used as it is

| Key | Command |
|---|---|
| `y` approve a saga gate | `borant task close <channel/id> --reason done`: the chief's role approves gates |
| `d` close as done | the same, for a task that the chief may close; else the v0.19 Done path (a prompt to the chief) |
| `a` ask, `m` message | `borant send <agent> <text>` (with `--task`, see the changes spec) |
| `o` open the PR | `open <pr_url>` on the host |
| `u` retry a spawn, resume an agent | `borant resume <agent>` |

## What changes

### 1. An answer that the mod can confirm

`borant answer` gains `--key <key>`. The protocol already takes a key
(`ToolCall::Answer { key, .. }`); the CLI passes `None` today. With a key, a
second answer with the same key returns the first result and sends nothing.
The mod makes one key per draft. After a timeout it sends again with the same
key, which is safe. So the panel can say "not confirmed" and offer one retry
key, instead of guessing.

Review found that the key alone is not enough: the hub's answer handler
(`borant-hub/src/inbox.rs`) does not store the key today. The hub stores each
answer key with its result for 24 hours. The same key with the same text
returns the stored result. The same key with other text is a conflict, so an
edited draft gets a new key. The answer also sends the task version that the
person saw (`expect_updated`), not the version read just before the send.

### 2. Decline a gate (cut)

Cut after review: in the design, `n` only cancels the confirmation. To push
back on an approval, the person messages the orchestrator (`m`). The text
below stays for a later change.


`borant task decline <channel/id> --reason <text>`: the gate stays open,
its `attention_msg` becomes the reason, and the owner gets a direct message
with the reason. Today the only way is to close it as wontfix, which ends the
saga.

### 3. Spawn without a bundle

`borant resume <agent> --without-bundle <bundle>`: drops one bundle from the
queued spawn and places it again. The blocked-spawn alarm names this fix
today, but no command does it.

### 4. Go to an agent

`borant attach <agent> --window`: the host daemon of the person's machine
opens the attach command in a new window of its launcher, through a new
launcher port method, `focus(agent)`: a new window in the person's tmux
client, a focused Orca terminal, or a zellij tab. A launcher without it
returns "not supported", and the mod shows the attach command to copy.
The first version implements `focus` for tmux only; the other adapters return
"not supported" until they need it.

## Tests

- Answer: a second call with the same key sends nothing and returns the
  first result.
- Resume without a bundle: the spawn record loses the bundle and is placed.
- Launcher contract suite: `focus` on each adapter, or "not supported".

## Cut

No new "approve" verb: closing the gate as done is approval, as today.
