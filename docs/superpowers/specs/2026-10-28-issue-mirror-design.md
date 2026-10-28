# Issue mirror: Borant to Linear, one way

Date: 2026-10-28. Status: approved by Chris (via the chief), 2026-10-28.

The sagas and epics spec (decision 4) added the link fields and the setting:
`issue` on a saga channel, an epic, and a task, and `mirror_issues` on a
collection with an override for each project. This spec adds the mirror that
uses them. It is the smallest thing that works.

## Decisions

1. The hub is the source of truth for sagas, epics, and tasks.
2. The mirror is one way, Borant to the tracker. An edit in Linear never flows
   back.
3. Any saga, epic, or task can link to an issue.
4. `mirror_issues` turns the mirror on for a collection, with an override for
   each project. The work collection turns it on, and personal stays off.

Chris's answers (2026-10-28):

5. **The team depends on the issue.** No collection or project has a team.
   Each item has its own team, set when the item is created, and an item
   inherits the team of its saga or epic. When no team is set and none can be
   inferred, the mirror does not guess. It skips the item and raises
   `needs_human` on it, so the chief can ask Chris.
6. **Chris's own Linear personal key** creates the issues. The key is not
   limited to a team.
7. **The full task body** goes to Linear.
8. **Everything that gets a PR gets an issue,** including plain project tasks
   outside epics. A task with no PR stays out unless it is linked.

## Shape

The shape copies the PR watch (`crates/borant-hub/src/changes.rs`).

- **A port.** `borant-core::ports::issues::IssueTracker` has neutral types.
  Its in-memory adapter is in `borant-memory`, and its contract suite in
  `borant-core::testing`.
- **An exec adapter.** `borant-exec::issues` speaks the exec plugin protocol
  with `kind: "issues"`.
- **A hub loop.** `Hub::mirror_loop` runs on its own task, outside the
  request lock, every `every_secs`. It is wired from `[mirror]` in `hub.toml`
  by `mirror_tracker()` in `config.rs`, next to `changes_watch()`.
- **A private plugin.** `borant-mirror-linear` is in `borant-infra`, at
  `pkgs/borant-mirror-linear.sh`. It uses curl and jq against Linear's GraphQL
  API. The repo rule keeps vendor code out of this repo.
- **A new field, `issue_target: Option<String>`.** It is on `Task`, `NewTask`,
  and `TaskPatch`, and in the MCP and CLI task tools as `--issue-target`.
  - The channel create of a saga or a standalone epic also takes
    `issue_target`, and puts it on the channel's anchor task. That is the
    gate task of a saga, or the root epic task (`Channel.root`) of an epic
    channel.
  - The core never reads the value. The plugin reads it as a Linear team key,
    such as `ENG`.
  - See "The team" below.

## What maps to what

An item is mirrored only when its project resolves `mirror_issues` to true.

| Borant | Mirrored when | Linear | Parent |
|---|---|---|---|
| Saga channel | always | Issue | none |
| Epic task in a saga | always | Sub-issue | the saga's issue |
| Standalone epic channel | always | Issue, held by the channel | none |
| Root epic task of an epic channel | never: the channel's issue stands for it | — | — |
| A saga's gate task | never | — | — |
| Any other task: under an epic, under a plain task, or plain in a project channel | it has a `pr_url` property or an `issue` link | Issue | the issue of the nearest mirrored ancestor, else none |

A task with no PR gets its issue when its PR opens, which is when the worker
sets `pr_url`. The "nearest mirrored ancestor" comes from walking `parent` up
to the epic, then to the saga or epic channel.

### Fields the mirror writes

- **Title:** the task's title. For a channel, the channel's `title`, or its
  name when it has no title.
- **Description:** the full body of the task.
  - A saga uses its gate task's body, which holds the scope.
  - A standalone epic channel uses its root epic task's body.
  - A footer follows the body: "Mirrored from Borant (`<channel>/<task>`).
    Edits here do not flow back." The footer adds the PR link when
    `properties.pr_url` is set.
- **State:** see "States" below.
- **Parent:** see the table above.

The mirror writes no assignee, label, priority, estimate, cycle, or comment.

### States

The core sends a neutral state. The plugin picks the team's first workflow
state of the matching Linear state type, so a team with custom states still
works.

A plain task maps by its own status:

| Task | Neutral | Linear type |
|---|---|---|
| `open` | `todo` | unstarted |
| `in_progress`, `needs_human`, `stuck` | `started` | started |
| closed `done` | `done` | completed |
| closed `wontfix`, `duplicate`, `superseded` | `canceled` | canceled |

`needs_human` and `stuck` stay `started`. Linear has no blocked type, and the
board already shows the question.

A container is a saga, an epic task, or a standalone epic channel. The first
rule that matches gives its state:

1. **Closed.** A closed channel or a closed epic task is `done` when it
   closed as `done` or as a channel. Otherwise it is `canceled`.
2. **A saga not approved yet.** A saga whose gate task is not closed `done`
   is `backlog`.
3. **Work has begun.** A container with any child that is not open and not
   canceled is `started`. A child is any task under the container, except the
   gate. `in_progress`, `needs_human`, `stuck`, and closed `done` all count.
4. **Otherwise:** `todo`.

Epic tasks are never claimed, so a container's state comes from its children,
never from its own status.

## The team

A create needs a team, and an update does not. An item that is linked to an
existing issue therefore needs no team. For a create, the mirror resolves the
team in this order. The first match wins.

1. **The item's own `issue_target`.** For a saga or an epic channel, this is
   its anchor task's `issue_target`.
2. **The nearest ancestor with an `issue_target`.** The mirror walks
   `parent` up, then checks the anchor task of the saga or epic channel.
3. **The parent issue's team.** If the nearest mirrored ancestor already has
   an issue, the hub sends no target, and the plugin creates the sub-issue in
   the parent issue's team. This covers a saga that was linked to an existing
   issue with no team set.
4. **None.** The mirror does not guess:
   - The mirror skips the item and sets `stopped: "no_target"` in the item's
     KV record (see "The KV record" below).
   - It raises `needs_human` on the item, with a revision-checked write, the
     way the continues limit does in `continuation.rs`. For a saga, the target
     is its gate task. For an epic channel, the target is its root epic task.
   - The attention message reads: "Linear team needed: set issue_target on
     this task (for example `borant task update <id> --issue-target ENG`)."
   - The KV record keeps the task's earlier status in `was`.
   - The question shows in the board's `needs_you`, and the chief asks Chris.
   - Once `issue_target` is set, the next sweep:
     1. clears the stop,
     2. puts the task back to its `was` status, if the task is still in
        `needs_human` with that message,
     3. creates the issue.

     The PR watch skips tasks in `needs_human`, so the status comes back on
     its own.

A saga that is created with no team asks at its gate. The gate is already a
question for Chris, so the two arrive together. Orchestrators should set
`issue_target` at create when they know the team. When they do not know it,
they should ask the chief.

## When the mirror writes

The mirror writes when an item changes. A sweep finds the change.

1. **Read.** Every `every_secs` (default 60), the loop reads every channel
   from `open_channels()`, which includes closed ones, and their tasks, in the
   mirrored projects.
2. **Build.** For each mirrored item, it builds the record that it would
   write: the title, the description, the parent key, and the state.
3. **Compare.** It compares a hash of that record with the item's KV record.
4. **Act.**
   - An item with no KV record:
     - If it is open, the mirror creates an issue for it.
     - If it already has an `issue` link, the mirror adopts that issue (see
       "An item already linked to a Linear issue").
     - If it is closed, the mirror leaves it alone, so closed history is not
       backfilled.
   - An item with a KV record:
     - The mirror writes when the hash differs.
     - It also writes when the item is closed, so the final state goes out.
     - It skips stopped items.
5. **Cap.** It writes at most `max_writes` per sweep (default 50). Parents go
   before children. Then the oldest go first, by `updated`, or by `created`
   for a channel.

An unchanged item costs no API call. The sweep survives a hub restart and a
Linear outage with no event log. One minute of lag is fine for a tracker. The
cap keeps a backfill well under Linear's API limits.

The mirror compares with what it wrote, not with what Linear shows. A person
who moves an issue in Linear keeps that move until the Borant item changes
again.

### The KV record

The key is `mirror.<channel>.<task>`, or `mirror.<channel>` for a channel.

```json
{"issue": "ENG-123", "hash": "<sha256 of the record>", "owned": true,
 "stopped": null, "was": null}
```

- `stopped` is `null`, `"no_target"`, `"not_found"`, or `"unlinked"`. A
  stopped item gets no writes.
- `issue` is `null` while the record is `no_target`.
- `was` is the status from before the mirror raised `needs_human`.

### The link

When the plugin creates an issue, the hub sets the item's `issue` to the key
that comes back, such as `ENG-123`. The hub writes it, because only the hub
writes control-plane keys. The loop runs outside the request lock, so the
write is revision-checked with a retry, as `close_channel` does it. It changes
only `issue`. For a channel, it changes `Channel.issue`.

A create is idempotent, so a crash between the create and the link makes no
second issue:

1. The hub sends `key`, a UUID with the version-4 bits set, built from a hash
   of the Borant id.
2. The plugin passes it as the `id` of `issueCreate`.
3. On a create error, the plugin first looks the issue up by that id. If it
   finds it, it returns it.

### Errors

The exec protocol's codes, and the runner's timeout:

| Error | What the hub does |
|---|---|
| `unavailable`, a timeout, `internal` | It logs a warning and retries on the next sweep. |
| `not_found` | The linked issue is gone. The hub tells the channel owner once and sets `stopped: "not_found"`. |
| `invalid` | The hub tells the owner once and skips the item until its hash changes. |
| `denied` | The key is wrong or was revoked. The mirror pauses until the next hub start. |

For `denied`, the hub keeps a `mirror_refused` fact in memory, with an
`AlertView` field and an alert rule, as it does for `provisioner_refused`.
The notifier tells Chris once.

## An item already linked to a Linear issue

An orchestrator or a person can set `issue` to an existing issue. The mirror
then adopts that issue but does not own it.

- **State only.** For an adopted issue, the mirror writes the state and
  nothing else. It never changes the title, the description, or the parent of
  an issue it did not create, and it never re-parents it.
- **Ownership is recorded.** The KV record says `owned: false`, so the rule
  survives restarts. An item that the mirror created has `owned: true`, and
  the mirror writes all four fields for it.
- **Key or URL.** `issue` can be a key such as `ENG-42`, or a Linear URL. The
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
    pub target: Option<String>,   // the team; None: the parent's team
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

A create with neither `target` nor `parent` is `invalid`. The hub never sends
one.

The exec ops are `describe`, `create`, and `update`, with `config` from
`[mirror.config]`, as for a provisioner. The plugin does the Linear lookups:

- It resolves `target`, a team key, to the team's UUID.
- It resolves `parent` and `issue`, each a key or a URL, to issue UUIDs.
  `issue(id:)` accepts a key.
- It caches each team's workflow states for one run.

## Linear auth and where the key lives

- **The key.** Chris's own Linear personal API key, the same key that his
  laptop's Linear CLI uses. The issues show Chris as their creator.
- **Where it lives.** In the Borant vault, because only Chris can write to
  1Password and the boxes read only that vault. The item is `linear-mirror`,
  with the field `credential`. No bundle names the item, so no agent gets it.
- **How it reaches the hub.**
  1. A new recipe, `just hub-linear-credential`, reads the item through the
     box store over SSH, as `hub-backup-credential` does.
  2. The recipe writes the key to `/var/lib/borant/credentials/linear-token`
     (root, 0400).
  3. The `borant-hub` unit loads that file with `LoadCredential`.
  4. The plugin reads `$BORANT_CREDENTIALS_DIRECTORY/linear-token`.
- **Rotation.** A rotation needs a new push and a hub restart.
- **Risk.** A host that holds the store token can read the item, as it can
  read the S3 key today. Because the key is Chris's full personal key, that
  read gives access to his whole Linear workspace. Chris accepted this when he
  chose his own key.

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
```

`borant-infra` adds the `[mirror]` table to the hub module behind
`hub.mirror.enable`. It also sets `mirror_issues = true` on the work
collection. Personal stays off.

Turning it on for work:

1. Chris creates the vault item.
2. Run `just hub-linear-credential`.
3. Ship a Borant release with the mirror, and pin it in `borant-infra`.
4. Set `hub.mirror.enable = true`, then deploy borant-1.

`borant board` already shows the resolved `mirror_issues`. A saga line or a
task line shows its `issue` key once the mirror sets it.

## Tests

- **`borant-core`:** unit tests for:
  - the container state rule,
  - the record hash,
  - the team resolution,
  - which tasks are mirrored,
  - the v4-shaped key.
- **The hub, against the in-memory tracker:**
  - It creates an issue once, then updates only on a change.
  - It writes only the state of an adopted issue.
  - A crash between the create and the link makes no duplicate.
  - A task with no team goes to `needs_human` once, and gets its status back
    after `issue_target` is set.
  - A sub-issue with no target uses the parent's team.
  - A task gets its issue when `pr_url` appears.
  - A `not_found` is told once.
  - `max_writes` caps a sweep, with parents first.
  - A closed item with no KV record makes no call.
  - A project with the mirror off makes no calls.
  - A link write that loses a revision race retries.
- **`borant-exec`:** the protocol against a stub script, as in the changes
  tests.
- **The plugin:** `borant-infra` tests it by hand against a scratch Linear
  team.

## Not in this spec

- Linear to Borant sync, comments, assignees, labels, and creating Borant
  items from Linear.
- More than one tracker at a time.
- Event-driven writes. The sweep is enough until it is not.
