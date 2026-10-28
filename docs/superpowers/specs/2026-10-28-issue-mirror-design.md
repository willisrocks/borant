# Issue mirror: Borant to Linear, one way

Date: 2026-10-28. Status: draft for Chris's approval.

The sagas and epics spec (decision 4) added the link fields and the setting:
`issue` on a saga channel, an epic, and a task, and `mirror_issues` on a
collection with an override for each project. This spec adds the mirror that
uses them. It is the smallest thing that works.

## Decisions already made

1. The hub is the source of truth for sagas, epics, and tasks.
2. The mirror is one way, Borant to the tracker. An edit in Linear never flows
   back.
3. Any saga, epic, or task can link to an issue.
4. `mirror_issues` turns the mirror on for a collection, with an override for
   each project. The work collection turns it on, and personal stays off.

## Shape

The shape copies the PR watch (`crates/borant-hub/src/changes.rs`).

- **A port.** `borant-core::ports::issues::IssueTracker`, with neutral types,
  an in-memory adapter in `borant-memory`, and a contract suite in
  `borant-core::testing`.
- **An exec adapter.** `borant-exec::issues` speaks the exec plugin protocol
  with `kind: "issues"`.
- **A hub loop.** `Hub::mirror_loop` runs on its own task, outside the
  request lock, every `every_secs`. `[mirror]` in `hub.toml` wires it, through
  a `mirror_tracker()` in `config.rs` next to `changes_watch()`.
- **A private plugin.** `borant-mirror-linear` is in `borant-infra`, as
  `pkgs/borant-mirror-linear.sh`. It uses curl and jq against Linear's
  GraphQL API. The repo rule keeps vendor code out of this repo.
- **New field `issue_target: Option<String>`.** It goes on `CollectionConfig`,
  `Project`, `borant project add --issue-target`, the proto, and the board
  project line. `roles::issue_target(project, collection)` resolves it the
  same way as `roles::mirror_issues`: the project wins. The core does not read
  it. It passes it to the plugin as the Linear team key, such as `ENG`.
  `CollectionConfig` has `deny_unknown_fields`, so this field must ship before
  `borant-infra` sets it.

## What maps to what

An item is mirrored when its project resolves `mirror_issues` to true.

| Borant | Linear | Parent |
|---|---|---|
| Saga channel | Issue | none |
| Epic task in a saga | Sub-issue | the saga's issue |
| Standalone epic channel | Issue, held by the channel | none |
| Root epic task of a standalone epic channel | Not mirrored. The channel's issue stands for it. | — |
| Task under an epic, at any depth | Sub-issue | the nearest mirrored ancestor's issue |
| The gate task "Approve the scope and the epics" | Not mirrored | — |
| Plain task of a project channel | Only when it has an `issue` link | none |

Nested tasks stay flat under their epic's issue unless the parent task itself
is mirrored. Under that rule, every task under an epic is mirrored, so the
parent is always the closest one.

### Fields the mirror writes

- **Title.** The task title, or the channel `title`, falling back to the name.
- **Description.**
  - For a task, its body.
  - For a saga, the body of its gate task, which holds the scope.
  - For a standalone epic channel, the body of its root epic task.
  - Then a footer: "Mirrored from Borant (`<channel>/<task>`). Edits here do
    not flow back." The footer adds the PR link when the task's
    `properties.pr_url` is set.
- **State.**
- **Parent.**

The mirror writes no assignee, label, priority, estimate, cycle, or comment.

### States

The core sends a neutral state. The plugin picks the team's first workflow
state of the matching Linear state type, so teams with custom states work.

**A plain task, by its own status:**

| Task | Neutral | Linear type |
|---|---|---|
| `open` | `todo` | unstarted |
| `in_progress`, `needs_human`, `stuck` | `started` | started |
| closed `done` | `done` | completed |
| closed `wontfix`, `duplicate`, `superseded` | `canceled` | canceled |

`needs_human` and `stuck` stay `started`. Linear has no blocked type, and the
board already shows the question.

**A container: a saga, an epic task, or a standalone epic channel.** The
first rule that matches wins.

1. Closed, meaning a closed channel or a closed epic task: `done` if it closed
   `done` or as a channel, else `canceled`.
2. A saga whose gate task is not closed `done`: `backlog`.
3. Any child that is not open and not canceled: `started`. A child here is any
   task under the container, the gate excluded. `in_progress`,
   `needs_human`, `stuck`, and closed `done` all count.
4. Otherwise: `todo`.

Epic tasks are never claimed, so a container's state comes from its children
and never from its own status.

## When the mirror writes

The mirror writes when an item changes. A sweep finds the change.

1. Every `every_secs` (default 60), the loop takes every channel from
   `open_channels()`, which includes closed ones, and their tasks, in mirrored
   projects.
2. For each item, it builds the record that it would write: title,
   description, parent key, and state.
3. It hashes the record and compares that hash with the KV record of the item.
4. **An item with no KV record:**
   - An open item gets a new issue. A closed item is left alone, so closed
     history is not backfilled.
   - An item that already has an `issue` link is adopted (see below).
5. **An item with a KV record:** the mirror writes only when the hash differs,
   and also when the item is closed, so its last state goes out.
6. It writes at most `max_writes` per sweep (default 50). Parents go before
   children, and items are sorted by `updated`, oldest first. A channel uses
   its `created`.

An unchanged item costs no API call. The sweep survives a hub restart and a
Linear outage with no event log. One minute of lag is fine for a tracker. The
cap keeps a backfill well under Linear's API limits.

The mirror compares with what it wrote, not with what Linear shows. A person
who moves an issue in Linear keeps that move until the Borant item changes
again.

### The KV record

The key is `mirror.<channel>.<task>`, or `mirror.<channel>` for a channel.

```json
{"issue": "ENG-123", "hash": "<sha256 of the record>", "owned": true, "stopped": null}
```

`stopped` is `null`, `"not_found"`, or `"unlinked"`. A stopped item gets no
writes.

### The link

When the plugin creates an issue, the hub sets the item's `issue` to the
returned key, such as `ENG-123`. The hub writes it, because only the hub
writes control-plane keys. The loop runs outside the request lock, so it
writes with a revision check and a retry, as `close_channel` does. It changes
only `issue`.

A create is idempotent:

1. The hub sends `key`, a UUID with the version 4 bits set, built from a hash
   of the Borant id.
2. The plugin passes it as the `id` of `issueCreate`.
3. On a create error, the plugin first looks the issue up by that id. If the
   issue exists, it returns it.

A crash between the create and the link therefore makes no second issue.

### Errors

These are the exec protocol codes, plus the runner's timeout.

- **`unavailable`, a timeout, or `internal`:** the mirror logs a warning and
  retries on the next sweep.
- **`not_found`:** the linked issue is gone. The hub tells the channel owner
  once and sets `stopped: "not_found"`.
- **`invalid`:** the hub tells the owner once and skips the item until its
  hash changes.
- **`denied`:** the key is wrong or lacks access.
  - The mirror pauses until the next hub start.
  - The hub keeps a `mirror_refused` fact in memory, plus an `AlertView`
    field and an alert rule, the same pattern as `provisioner_refused`. The
    notifier tells Chris once.

## An item already linked to a Linear issue

An orchestrator or a person can set `issue` to an existing issue. The mirror
adopts that issue but does not own it:

- **State only.** It writes the state and nothing else. It never changes the
  title, the description, or the parent of an issue it did not create, and
  it never re-parents one.
- **`owned: false`.** The KV record keeps this rule across restarts. An item
  that the mirror created has `owned: true`, and the mirror writes all four
  fields for it.
- **Key or URL.** `issue` can be a key such as `ENG-42` or a Linear URL. The
  plugin accepts both.
- **Clearing the link.** Clearing `issue` on an item sets
  `stopped: "unlinked"`. The mirror then stops for that item and leaves the
  Linear issue as it is.

The mirror never deletes a Linear issue. When a mirror turns off, its issues
stay as they are.

## The port

```rust
pub enum IssueState { Backlog, Todo, Started, Done, Canceled }

pub struct IssueWrite {
    pub key: String,              // a v4-shaped UUID, for an idempotent create
    pub target: String,           // the issue_target, opaque to the core
    pub parent: Option<String>,   // the parent issue's key
    pub title: String,
    pub body: String,
    pub state: IssueState,
}

pub struct IssueRef { pub key: String, pub url: String }

#[async_trait]
pub trait IssueTracker {
    async fn create(&self, w: &IssueWrite) -> PortResult<IssueRef>;
    /// `full` false writes the state only (an adopted issue).
    async fn update(&self, issue: &str, w: &IssueWrite, full: bool) -> PortResult<IssueRef>;
}
```

The exec ops are `describe`, `create`, and `update`. Each request carries
`config` from `[mirror.config]`, the same as for a provisioner.

The plugin translates between Borant and Linear:

- It resolves `target` (the team key) to the team's UUID.
- It resolves `parent` and `issue` (a key or a URL) to issue UUIDs.
  `issue(id:)` accepts a key.
- It caches the team's workflow states for each run.

## Linear auth and where the key lives

- **The key.** One Linear personal API key. Linear lets a key be limited to
  chosen teams and permissions: Create issues and Write. Limit it to the work
  team. Issues show the key's user as their creator.
- **Where it lives.** It lives in the Borant vault, because only Chris can
  write to 1Password and the boxes read only that vault. Chris creates the
  item `linear-mirror` with the field `credential`.
- **No agent gets it.** No bundle names the item.
- **How it reaches the hub.**
  1. The new recipe `just hub-linear-credential` reads the item through the
     box store over SSH, as `hub-backup-credential` does.
  2. It writes the key to `/var/lib/borant/credentials/linear-token` (root,
     0400).
  3. The `borant-hub` unit loads that file with `LoadCredential`.
  4. The plugin reads `$BORANT_CREDENTIALS_DIRECTORY/linear-token`.
- **Rotation.** A new push and a hub restart.
- **Risk.** A host that holds the store token can read the item, as it can
  read the S3 key today. The team-limited key limits the damage.

## Config

```toml
[mirror]
adapter = "exec"
program = "/nix/store/…/bin/borant-mirror-linear"
credential = "linear-token"
every_secs = 60
max_writes = 50

[collections.work]
mirror_issues = true
issue_target = "ENG"      # the Linear team key

# per project, optional: borant project add … --mirror-issues false --issue-target OPS
```

`borant-infra` adds the `[mirror]` table to the hub module, behind
`hub.mirror.enable`. It also sets `mirror_issues` and `issue_target` on the
work collection. Personal stays off.

Turning it on for work takes three steps:

1. Chris creates the vault item.
2. Run `just hub-linear-credential`.
3. Set `hub.mirror.enable = true` and deploy borant-1. This needs a Borant
   release that has `issue_target`.

`borant board` already shows the resolved `mirror_issues`. A saga or task
line shows its `issue` key once the mirror has set it.

## Tests

- **`borant-core`:** unit tests for the container state rule, the record hash,
  `issue_target` resolution, and the v4-shaped key.
- **The hub, against the in-memory tracker:**
  - It creates an issue once, then updates only on a change.
  - It writes only the state of an adopted issue.
  - A crash between the create and the link makes no duplicate.
  - A `not_found` is told once.
  - `max_writes` caps a sweep, with parents first.
  - A closed item with no KV record makes no call.
  - A project with the mirror off makes no calls.
  - A link write that loses a revision race retries.
- **`borant-exec`:** the protocol, against a stub script, as in the changes
  tests.
- **The plugin:** `borant-infra` tests it by hand against a scratch Linear
  team.

## Not in this spec

- Sync from Linear to Borant, comments, assignees, labels, and creating Borant
  items from Linear.
- Mirroring to more than one tracker at a time.
- Event-driven writes. The sweep is enough until it is not.
