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

The mirror copies Linear's shape from the PR watch (`crates/borant-hub/src/changes.rs`).

- **A port.** `borant-core::ports::issues::IssueTracker` has neutral types
  and no vendor name. It also has an in-memory adapter in `borant-memory` and
  a contract suite in `borant-core::testing`.
- **An exec adapter.** `borant-exec::issues` speaks the exec plugin protocol
  (`kind: "issues"`).
- **A hub loop.** `Hub::mirror_loop` runs on its own task, outside the request
  lock, every `every_secs`. It is wired from `[mirror]` in `hub.toml`.
- **A private plugin.** `borant-mirror-linear` lives in `borant-infra`
  (`pkgs/borant-mirror-linear.sh`, curl and jq against Linear's GraphQL API).
  The repo rule keeps vendor code out of this repo.

## What maps to what

| Borant | Linear | Parent |
|---|---|---|
| Saga channel | Issue | none |
| Epic task in a saga | Sub-issue | the saga's issue |
| Standalone epic channel | Issue | none |
| Task under an epic | Sub-issue | the epic's issue |
| Plain task of a project channel | Not mirrored, unless it has an `issue` link | none |

One Linear team receives the issues of a project. The team is a string that
the core does not read: `issue_target` on the collection, with an override on
the project. The plugin reads it as a Linear team key, such as `ENG`.

### Fields the mirror writes

- The title.
- The description: the Borant body, then a footer: "Mirrored from Borant
  (`<channel>/<task>`). Edits here do not flow back." The footer also carries
  the PR link when the task has a `pr_url`.
- The state.
- The parent.

The mirror writes no assignee, label, priority, estimate, cycle, or comment.

### States

The core sends a neutral state. The plugin picks the team's first workflow
state of the matching Linear state type, so a team with custom states still
works.

| Borant | Neutral state | Linear state type |
|---|---|---|
| Saga, gate not done (proposed) | `backlog` | backlog |
| Task `open`; saga or epic channel with nothing started | `todo` | unstarted |
| Task `in_progress`, `needs_human`, or `stuck`; saga or epic channel with a started epic or task | `started` | started |
| Task closed `done`; saga or epic channel closed | `done` | completed |
| Task closed `wontfix`, `duplicate`, or `superseded` | `canceled` | canceled |

`needs_human` and `stuck` stay `started`. Linear has no blocked state type,
and the board already shows the question.

## When the mirror writes

The mirror writes when an item changes. A sweep finds the change:

1. Every `every_secs` (default 60), the loop lists the open items of each
   project where `mirror_issues` resolves to true.
2. It also lists the items that closed since the last sweep.
3. For each item, it builds the record that it would write (title,
   description, state, parent).
4. It compares the hash of that record with the hash that it last wrote. The
   hub keeps that hash in the KV key `mirror.<channel>.<task>`, or
   `mirror.<channel>` for a saga or epic channel.
5. It calls the plugin only when the hashes differ.

A sweep survives a hub restart and a Linear outage with no event log, and an
unchanged item costs no API call. One minute of lag is fine for a tracker.
The loop writes at most `max_writes` (default 50) per sweep, oldest change
first. That keeps a backfill well under Linear's API limits.

The mirror compares with what it wrote, not with what Linear shows. A person
who moves an issue in Linear keeps that edit until the Borant item changes
again.

### The link

When the plugin creates an issue, the hub sets the item's `issue` to the
returned key, such as `ENG-123`. The hub writes the key, because only the hub
writes control-plane keys. A create is idempotent:

1. The hub sends `key`, a UUIDv5 of the Borant id.
2. The plugin passes it as the `id` of `issueCreate`.
3. A retry after a crash finds the existing issue and does not make a second
   one.

### Errors

The plugin answers with the exec protocol codes:

- `unavailable`, `timeout`: the loop retries on the next sweep.
- `not_found`: the linked issue is gone. The hub tells the channel owner
  once, keeps the link, and stops writing to that issue.
- `denied`: the key is wrong or lacks access. The hub logs it, raises a
  `mirror_denied` alert to the notifier once, and pauses the mirror until
  the next hub start.
- `invalid`: the hub tells the owner once and skips the item.

## An item already linked to a Linear issue

An orchestrator or a person can set `issue` to an existing issue. The mirror
then adopts that issue but does not own it:

- It writes the **state only**. It never changes the title, description, or
  parent of an issue it did not create, and it never deletes or re-parents
  one.
- The KV record holds `owned: false`, so the rule survives restarts.
- An item that the mirror created holds `owned: true`. For that item, the
  mirror writes all four fields.
- An `issue` value can be a key (`ENG-42`) or a Linear URL. The plugin
  accepts both.
- Clearing `issue` on a task stops the mirror for that item and leaves the
  Linear issue as it is. The next sweep does not create a new one, because
  the KV record remembers the item.

The mirror never deletes a Linear issue. When a mirror turns off, its issues
stay as they are.

## Turning it on

The first sweep after a project turns on backfills its open sagas, epics, and
epic tasks. Closed items are not backfilled.

## The port

```rust
pub enum IssueState { Backlog, Todo, Started, Done, Canceled }

pub struct IssueWrite {
    pub key: String,              // stable id for an idempotent create
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

The exec ops are `describe`, `create`, and `update`, with `config` from
`[mirror.config]`, the same as for the provisioner.

## Linear auth and where the key lives

- **The key.** One Linear personal API key, restricted to the work teams if
  Linear's key scopes allow it, with access to create and edit issues. Issues
  show the key's user as their creator.
- **Where it lives.** It lives in the Borant vault, because only Chris can
  write to 1Password, and the boxes read only that vault. Chris creates item
  `linear-mirror` with field `credential`.
- **How it reaches the hub.** No bundle names the item, so no agent ever
  gets it. A new recipe, `just hub-linear-credential`, reads it through the
  box store over SSH, as `hub-backup-credential` does. The recipe writes it to
  `/var/lib/borant/credentials/linear-token` (root, 0400). The `borant-hub`
  unit loads it with `LoadCredential`, and the plugin reads
  `$BORANT_CREDENTIALS_DIRECTORY/linear-token`.
- **Rotation.** A rotation means a new push and a hub restart.
- **Risk.** A host that holds the store token can read the item, as it can
  read the S3 key today. A key scoped to the work teams limits the damage.

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
`hub.mirror.enable`, and sets `mirror_issues` and `issue_target` on the work
collection. The personal collection stays off. Turning it on for work takes
these steps:

1. Chris creates the vault item.
2. Run `just hub-linear-credential`.
3. Set `hub.mirror.enable = true` and deploy borant-1.

`borant board` already shows the resolved `mirror_issues`. A saga or task
line shows its `issue` key once the mirror sets it.

## Tests

- **Core.** The state mapping rules and the record hash, unit tested in
  `borant-core`.
- **Hub, against the in-memory tracker.** Create once, then update only on a
  change. Adopted issues get the state only. A crash between the create and
  the link makes no duplicate. A `not_found` is told once. `max_writes` caps a
  sweep. A project with the mirror off makes no calls.
- **Exec adapter.** The protocol, against a stub script, as in the
  `borant-exec` changes tests.
- **Plugin.** `borant-infra` tests it by hand against a scratch Linear team.

## Not in this spec

- Linear to Borant sync, comments, assignees, labels, and the creation of
  Borant items from Linear.
- Mirroring to more than one tracker at a time.
- Event-driven writes. The sweep is enough until it is not.
