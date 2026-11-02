# Blocked-work alarm

Status: scope from Chris, 2026-11-02. No new config.

## The incident

Seven work helpers sat in the spawn queue on the macbook for about a day,
from 2026-11-01 18:25 UTC, with "missing bundle github-work". One of them was
roger-support's preprod QA helper, and orch-roger-support held release
v1.0.23 on that QA. chief-work asked chief-personal for the bundle.
chief-personal treated the bundle as chief-work's to fix, and nobody told
Chris. He found out by asking why his fix was not in prod.

## Design

### 1. Blocked spawns (hub)

A spawn is *blocked* when it has been `Queued` for at least 15 minutes and
its queue reason is not about capacity. Capacity means that every reject is
`full`, the channel is at its cap, or there is low memory or a full disk; a
drain fixes those.

Every other reason is blocked:
- missing bundle
- no member bundle
- missing tag
- no harness
- no sandbox
- cannot reach the repo
- failed recently
- offline (when no host is online)

A new hub tick step, checked every 60 s, does this on the first sighting and
then at most every 4 hours while the spawn stays blocked:

- **The person:** one alert through the existing `[alerts]` notifier: "spawn
  `<agent>` blocked `<for>`: `<reason>`. Unblock: `<action>`".
- **DMs** to the spawner, and to the chief of the spawn's collection (the
  agent with the chief role for that collection, when there is one). The DM
  carries the same text.
- **The bound task, if any:** set to `stuck`, with the same text as its
  `attention_msg`, so it shows in `needs_you` and in `borant inbox`.

The action comes from the reason:

| Reason | Action |
|---|---|
| missing bundle B | add bundle B to a host that the spawn may use, or spawn without B |
| missing tag T | tag a host with T, or drop T from the role |
| no harness H | install H on a host |
| failed recently | `borant resume <agent>` after fixing the launch error (quoted) |
| offline | bring a host online |

The hub keeps when it last told about each blocked spawn in memory. After a
hub restart, it may tell once more.

### 2. Stalled cards (hub)

A task that is `in_progress` and has had no task events for
`[alerts] stall_minutes` (default 30) is stalled when its claimer is in one
of these states:
- queued and blocked;
- dead (no held lease, with the spawn `Ended` or the session gone);
- fenced.

A stalled task goes to `stuck`, with `attention_msg` naming the claimer and
its state. Then the existing `needs_you` and inbox show it, and the owner and
the chief get one DM. A task that waits on the stalled task through
`blocked_by` (a held release, for example) is named in the same message:
"holds: `<task titles>`".

### 3. The board's need-you

`CollectionBoard` gets `blocked: Vec<BlockedSpawn>`. Each entry has the agent,
channel, project, reason, action, since, spawner, and bound task. Blocked
spawns that are bound to a task already appear through their stuck task, so
`blocked` lists only the unbound ones, to avoid listing a spawn twice.

The following count blocked entries as items for the person:
- the CLI's `borant board` need-you section, which prints the alarm's line;
- the chief mod's band and `/cos` digest.

The chief mod gives each blocked spawn a row of its own, after the questions
and before the projects: the `■` glyph, the agent and its refusal, `project ·
age`, and the unblock command in the tail. A row has no Done or Ask: a person
clears it by fixing the cause, and a Done that only closed the spawn would
hide the alarm. Enter opens a card with the reason, the one action, and the
command the action quotes, on a line of its own to copy. A spawn that arrives
while board mode holds the rows still waits for esc, as a question does, and
neither arrival moves a column.

### 4. The chief-of-staff skill

New rules:

- **Sweep every check.** At every check, the chief sweeps its collection:
  `borant board`, `borant ps` (the queue and its reasons), and agents that
  are dead or silent.
- **Report a blocker within one turn.** Within one turn of seeing a blocker
  that it cannot clear, the chief tells the person one line: what is
  blocked, why, and the one action that unblocks it.
- **Cross-collection requests.** When another chief asks for something that
  this chief controls (a host bundle, a tag, a host), the receiving chief
  fixes it at once or escalates it to the person at once. It never parks it.
