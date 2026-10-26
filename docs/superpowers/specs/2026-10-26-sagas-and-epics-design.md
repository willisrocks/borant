# Sagas and epics

Sagas and epics become objects that the hub knows. Today a channel is one flat
task graph, and only the words in its name tell a feature from a fix. The
chief, and later a Claude Code mod for the chief, must show for each project
the active saga or epic, its progress, and what waits on Chris. A later mirror
must copy the same objects out to an issue tracker. This spec gives the hub
the types, the rules, and one read view for both. It is a design only.

## Decisions by Chris (2026-10-26, through chief-personal)

1. Three levels: saga, epic, task. Most feature work is at least an epic. A
   small fix is a plain task.
2. One channel per saga. The whole task graph of the saga lives in that
   channel. An epic in a saga is a parent task, and its children are its
   sub-graph. The epic carries its branch, its PR, and an optional issue link.
   A `blocked_by` edge can cross epics, because they share one channel.
3. A channel has an explicit kind: `saga`, `epic` (a standalone epic with its
   own channel), or `project` (the plain channel of a project, for plain
   tasks). A task has a kind: `epic` or `task`.
4. The hub and its tasks are the source of truth. Any saga, epic, or task can
   link to an issue in an outside tracker (Linear for Chris). A one-way mirror
   out to the tracker runs only where it is on: a collection setting with an
   override for each project. The work collection turns it on, and personal
   stays off. The mirror is a later project. This spec defines only the link
   fields and the setting.
5. One approval gate: a saga stops for Chris's approval of its scope and its
   list of epics before its epics start. The approval goes through the chief.
   Epics then run without more gates.
6. One orchestrator per project still owns all sagas and epics of the project.
7. Out of scope, but this design must serve them: the chief's Claude Code mod
   (mockup v0.0.1, approved), which reads its data from the hub, and then the
   mirror.

## Goals

1. The hub stores the kind of each channel and each task. A record without a
   kind parses as today.
2. The hub enforces the saga gate: no claim and no spawn in a saga before its
   approval.
3. A worker of an epic starts on the branch of the epic.
4. One read view, the board, gives a collection's projects, sagas, epics,
   progress, and the questions for a person, over MCP, the CLI, and HTTP.
5. Every saga, epic, and task can carry an issue link, and the hub resolves
   the mirror setting for each project.

## Non-goals

- The mirror and the mod. They are later projects.
- A gate for each epic, nested sagas, and an epic inside an epic.
- `blocked_by` across channels, for example from one saga to another.
- A change of kind after the create.
- Hub enforcement of "one orchestrator per project". The skills keep that rule.
- Changes to the web page at `/`.

## Data model

All new fields are `serde(default)`, so old records parse unchanged.

| Type | Field | Meaning |
|---|---|---|
| `ChannelKind` (new) | `project` (default), `saga`, `epic` | Set at the create. It never changes. |
| `Channel` | `kind: ChannelKind` | |
| `Channel` | `title: Option<String>` | A display name, for example "Elastic compute". The channel name stays a slug. |
| `Channel` | `issue: Option<String>` | The issue link of a saga. |
| `Channel` | `gate: Option<TaskId>` | The approval task of a saga. Only a saga has one. |
| `Channel` | `root: Option<TaskId>` | The epic task of a standalone epic channel. |
| `TaskKind` (new) | `task` (default), `epic` | Set at the create. It never changes. |
| `Task`, `NewTask` | `kind: TaskKind` | |
| `Task`, `NewTask`, `TaskPatch` | `issue: Option<String>` | The issue link of an epic or a task. |
| `CollectionConfig` | `mirror_issues: bool` (default false) | `[collections.<name>] mirror_issues = true` in `hub.toml`. |
| `Project` | `mirror_issues: Option<bool>` | The override of one project. `borant project add --mirror-issues true|false`. |

The issue link is a URL or a tracker key, such as a Linear issue key. The
field is `issue`, not `linear`, because the repo rule forbids vendor names in
core types and keys. The Linear adapter of the later mirror reads it.

The branch and the PR of an epic stay the task properties `branch` and
`pr_url`. Both are in the default schema of every channel, and the PR watch
of phase 6 already reads `pr_url`. As a result, the owner hears when the epic
PR is green and when it merges, with no new code. `kind` and `issue` are
first-class fields, because the hub rules and the mirror read them, and a
property would need a schema change in every old channel.

## The three channel kinds

- **Project channel.** As today. A task of kind `epic` is refused here.
- **Saga channel.** It must name a project. At the create, the hub also
  creates the gate task, "Approve the scope and the epics", with status
  `open`. An epic is a task of kind `epic` with no parent. Every other task,
  except the gate, must have a parent, and its chain of parents must end at an
  epic. `blocked_by` works across the epics of the channel, as today.
- **Epic channel.** It must name a project. At the create, the hub creates
  the root task of kind `epic`, with the channel title. A task created without
  a parent gets the root as its parent. A second task of kind `epic` is
  refused.

## Rules

1. **Create.** Any agent that can create a channel can create a saga or an
   epic channel, as today. By the skill rule, the project's orchestrator
   creates them. If another agent creates one, it transfers the channel to the
   orchestrator.
2. **Gate.** While the gate of a saga is not closed as `done`, the hub refuses
   `task_claim` and `spawn` in the channel with the new error code
   `saga_not_approved`, and `task_list(ready_only)` returns no task. The
   orchestrator creates the epics, then sets the gate to `needs_human` with
   an `attention_msg` that states the scope and lists the epics. When a gate
   goes to `needs_human`, the hub sends a direct message to each agent of the
   saga's collection whose role has `approves = true`. Today only the channel
   owner wakes on a task event, and the chief is not a member of the saga
   channel. (Orchestrator's call, after the builder check.)
3. **Approve.** The gate closes as `done` only by a person (the CLI
   `borant task close`) or by an agent whose role has `approves = true` in
   `hub.toml` (`[roles.chief]`). The owner of the channel cannot close it,
   unless its role has the flag. A close as `wontfix` rejects the saga, and
   the orchestrator then closes the channel.
4. **After the approval.** An epic created after the approval gets no new
   gate. The hub sends the usual task event to the channel, and the board
   shows the new epic.
5. **An epic is not work.** No agent claims a task of kind `epic`, and no
   spawn binds to one. `ready` never returns one. The board derives the
   status of an epic from its children: `open` while every child is open,
   `in_progress` when one child is not open, and the epic's own status when it
   is closed, stuck, or needs a human. (Orchestrator's call.)
6. **Blocks flow down.** A task is ready only if its own `blocked_by` tasks
   and the `blocked_by` tasks of each of its parents are closed. So "epic 2
   is blocked by epic 1" holds back every child of epic 2, and the
   orchestrator can still create every task before the first spawn. The
   claim check and the board counts use the same rule. (Orchestrator's call.)
7. **Epic branch.** A spawn bound to a task starts on the `branch` property
   of the task's epic, if the epic has one. Otherwise it starts on the channel
   branch, as today. The hub computes it where it fills `LaunchOrder.branch`
   (`crates/borant-hub/src/spawns.rs`). The branch must be on `origin` before
   the spawn.
8. **Close an epic.** The rule of today applies: a parent with open children
   cannot close. When the root epic of an epic channel closes, the hub closes
   the channel.
9. **Close a channel.** The new call `channel_close` sets `Channel.closed`.
   Only the owner or a person can call it. It is refused while a task of the
   channel is not closed. A closed channel refuses `task_create`,
   `task_claim`, and `spawn`. Today the hub never sets `closed`, so this rule
   is new for every kind.

## API, MCP, and CLI

- `channel_create` gets `kind`, `title`, and `issue`. Its result names the
  gate of a saga and the root of an epic.
- `task_create` gets `kind` and `issue`. `task_update` gets `issue`.
- `channel_list` returns the new channel fields.
- New: `channel_close { name }`. The CLI is `borant channel close <name>`.
- New: `ToolCall::Board { collection: Option<CollectionName> }`. Without a
  collection, it returns every collection. It is the MCP tool `board`, the CLI
  `borant board [--collection C] [--json]`, and the read route
  `GET /api/board?collection=C` of the HTTP listener. It is a computed read.
  It stores nothing.

The board is the read that the mod needs. The mod can call `borant board
--json` through the local host, or the HTTP route on the tailnet, and follow
`/api/events` for changes. Its shape:

```
BoardView { at, collections: [CollectionBoard] }
CollectionBoard { collection, projects: [ProjectBoard], needs_you: [Question] }
ProjectBoard {
  project, mirror_issues,          // the resolved setting
  orchestrators: [AgentLine],      // the owners of the project's open channels
  sagas: [SagaLine],               // active first, then proposed, then closed
  epics: [EpicLine],               // standalone epic channels
  tasks: Counts,                   // the plain tasks of the project channels
}
AgentLine { agent, online, busy, host }
SagaLine  { channel, title, issue, state, epics: [EpicLine], done, total }
           // state: proposed (gate not done), approved, closed
           // done/total: epics closed as done / epics not closed as wontfix
EpicLine  { channel, task, title, status, issue, branch, pr_url, counts }
Counts    { open, ready, running, blocked, stuck, needs_human, done }
Question  { project, channel, task, title, status, attention_msg, since, gate }
```

`running` counts `in_progress` tasks. `blocked` counts open tasks that are not
ready. `needs_you` is the `borant inbox` list (`needs_human` and `stuck`)
filtered to the collection, and `gate` is true for a saga gate. The "Done"
button of the mod maps to the existing `borant answer`, or to a gate close.

## Skills

The orchestrate skill:

1. Size each goal: a plain task in the project channel, a standalone epic, or
   a saga.
2. For a saga: create the channel with `kind: saga`, a title, and the issue
   link. Create the epics with a body that states the scope of each. Set the
   gate to `needs_human`, and end the turn. After the approval, create the
   tasks of every epic, with `blocked_by` between epics for order, and spawn
   for the ready tasks.
3. When an epic becomes ready: create the epic branch from the base branch,
   push it, and set `branch` on the epic task. The branch then holds the
   merged work of the epics before it. Workers open their PRs against the epic
   branch, and the orchestrator merges them there. When the epic's children
   are closed, the orchestrator opens the epic PR against the base branch,
   sets `pr_url`, merges it when green, and closes the epic.
4. When the last epic of a saga closes, call `channel_close`.

The chief-of-staff skill:

1. Map a new goal to a project and a size, and tell the orchestrator.
2. A saga gate is the main `needs_human` case. Show Chris the title, the
   scope, and the epics, with a suggested answer. On Chris's yes, close the
   gate as `done` with Chris's words as the message. On a no, close it as
   `wontfix`.
3. Answer status questions from `board`.

`borant-infra` adds `approves = true` to `[roles.chief]`, and
`mirror_issues = true` to `[collections.work]` when the mirror exists.

## Migration

Every old channel parses as kind `project`, and every old task as kind `task`.
The hub rewrites no record. Old parent tasks stay plain tasks. Work in flight
finishes in its current channel. There is no tool to convert a channel. A
CLI or host of an older version ignores the new fields, and the host daemon
needs no change, because the hub computes the launch branch.

## Open questions for Chris

1. Do worker PRs target the epic branch, with one epic PR into the base
   branch? Default: yes. This spec assumes it from "the epic carries its
   branch, its PR". The other choice is that workers merge straight into the
   base branch and the epic only tracks them. Then the epic has no branch,
   rule 7 goes, and a release can hold half an epic.
2. Does an epic added after the approval of its saga need a new approval?
   Default: no. The hub adds it with no gate, and the board shows it to the
   chief (rule 4).

## Left to the builder

- The text of the error messages, and the exact JSON names in `BoardView`
  beyond the fields listed in the board shape.
- The order of rows inside a list, beyond "active first".
- The text layout of `borant board`.
- Where the board code lives. `crates/borant-hub/src/view.rs` and `inbox.rs`
  have the patterns.
- The focused tests. Each rule in "Rules" needs one.

## Cut

- The issue mirror and its adapter.
- The chief's mod.
- Deadlines and schedules on the board. The mockup shows them, and the mod can
  read them from the task and `borant schedules` until the board needs them.
- A gate on epics added after the approval.
- Conversion of an old channel to a saga or an epic.
