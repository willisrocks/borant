# Issue mirror Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub copies sagas, epics, and tasks out to an issue tracker, one way. A sweep every `every_secs` builds the record of each mirrored item, compares its hash with a KV record, and creates or updates the issue through the `IssueTracker` port. The hub writes the returned key into the item's `issue` field. An item with no team goes to `needs_human` until someone sets `issue_target`.

**Architecture:** Three tasks, one PR each. Task 1 adds everything that does not run in the hub: the `issue_target` field, the `IssueTracker` port and its neutral types, the in-memory adapter, the contract suite, the pure rules in `rules/mirror.rs`, and the exec adapter. Task 2 adds the hub loop, the KV record, the link write, the `needs_human` path, the errors, the `mirror_refused` alert, and the `[mirror]` config. Task 3 adds the surface: `issue_target` on `channel_create`, the MCP, CLI, and Pi flags, issue keys on the board, the skills, and the guide. Tasks 2 and 3 run in parallel.

```
Task 1 ──> Task 2
   └─────> Task 3
```

**Tech Stack:** Rust (serde, sha2, tokio, clap, rmcp, schemars), TypeScript (Pi extension, bun), POSIX sh and jq (the test plugin).

**Spec:** `docs/superpowers/specs/2026-10-28-issue-mirror-design.md`

**Exec wire format.** The private plugin is written against this format. Task 1 must match it byte for byte:

```
{"v":1,"op":"describe","config":{}}
  -> {"ok":{"v":1,"kind":"issues"}}
{"v":1,"op":"create","config":{},"write":{"key":"<uuid>","target":"ENG"|null,"parent":"ENG-1"|null,"title":"..","body":"..","state":"backlog|todo|started|done|canceled"}}
  -> {"ok":{"key":"ENG-123","url":"https://..."}}
{"v":1,"op":"update","config":{},"issue":"ENG-123 or URL","write":{...same...},"full":true|false}
  -> {"ok":{"key":"ENG-123","url":"https://..."}}
```

- Errors use the standard codes of `crates/borant-exec/src/lib.rs:372-380`.
- The default timeouts are 10 s for `describe`, and 30 s for `create` and `update`.
- `target` and `parent` are always present, as `null` when unset. They are never left out.
- `describe` carries `config`, as the provisioner's `describe` does (`crates/borant-exec/src/provision.rs:83-91`). The shared `borant_exec::describe` helper (`crates/borant-exec/src/lib.rs:237-247`) sends no `config`, so the issues adapter does not use it.

## Global Constraints

- **Ports and adapters.** No type, rule, error text, attention message, config key, or test fixture in this repo names a vendor. The port is `IssueTracker`, the exec kind is `issues`, the table is `[mirror]`, and the fields are `issue` and `issue_target`. The word "Linear" may appear only in `docs/` and in the skills under `integrations/claude-code/skills/`. Before each push, run `rg -i linear crates integrations/pi tests`. Expected: no match.
- **Only the hub writes control-plane keys.** The mirror loop runs in the hub. It writes:
  - `mirror.*`,
  - the `issue` field of a task or a channel,
  - the `needs_human` status and its restore.

  The plugin writes nothing to Borant.
- **The loop holds no request lock.** Every write to a task or a channel record is revision-checked. A link write that loses the race reads the record again and retries, up to 5 times. A `needs_human` write or a restore that loses it waits for the next sweep.
- **Every new stored field is `#[serde(default)]`.** This covers:
  - `Task.issue_target`, `NewTask.issue_target`, and `TaskPatch.issue_target`,
  - every field of the KV record,
  - `MirrorConfig`.

  The hub rewrites no old record.
- **Hooks and the Pi extension never break the harness.** No task changes their exit-0 behavior.
- **Focused tests only, outside CI:** `just test-crate <crate> <filter>`. Run `just fmt` and `just lint` before each push. Task 3 also runs `just test-pi`.
- **Test names start with the filter of their task.** The filters are `mirror` (core rules, hub, alerts, and config), `issues` (port, memory, and exec), and `issue_target` (the surface). `cargo test` filters on the test path, not the file name.

## Review Focus

1. **No vendor name.** Check every string in `rules/mirror.rs`, `hub/src/mirror.rs`, `borant-exec/src/issues.rs`, and `tests/plugins/issues-file.sh`. The spec's attention text starts with "Linear team needed". This plan replaces it with neutral text (choice M7).
2. **The hub never makes a second issue.** The create key is a v4-shaped UUID, derived from the Borant id (`rules::mirror::issue_key`). The order of the writes is (choice M3):
   1. create,
   2. the KV record with `linked: false`,
   3. the link,
   4. `linked: true`.

   A crash at any point leaves state that the next sweep finishes without a second create. Tests: `mirror_crash_between_create_and_link_makes_no_duplicate` and `mirror_record_without_link_relinks`.
3. **An adopted issue gets the state only.** `update(.., full: false)` is the only call for an item whose record says `owned: false`. Test: `mirror_adopted_issue_gets_state_only`.
4. **The `needs_human` write can be undone.** The KV record keeps `was`. The restore runs only when the task is still in `needs_human` with the mirror's message. A gate that already waits for approval keeps its question (choice M8). Tests: `mirror_no_team_raises_needs_human_once_and_restores` and `mirror_no_team_on_a_waiting_gate_keeps_its_question`.
5. **The link write is revision-checked.** It changes only `issue` and `updated`. After a conflict, it reads the record again, so no concurrent `task_update` is lost. Test: `mirror_link_write_retries_a_lost_race`.
6. **Rollout order.** `HubFile` has `#[serde(deny_unknown_fields)]` (`crates/borant/src/config.rs:240-241`). A hub older than Task 2 refuses a `hub.toml` with `[mirror]`. `borant-infra` adds the table only after the hub runs the release with Task 2.
7. **A turned-off project makes no call.** The sweep checks `roles::mirror_issues` before it builds any record. Test: `mirror_off_project_makes_no_calls`.
8. **The cap counts calls, not items.** An unchanged item costs nothing. A child whose parent has no issue yet waits and makes no call. Test: `mirror_max_writes_caps_parents_first`.

---

### Task 1: Core, memory, and exec

Blocked by nothing. Size: about 1,300 lines, half of them tests, plus a mechanical churn of 22 struct literals.

**Files:**
- Modify: `crates/borant-core/src/model.rs`:
  - `Task` 544-582: add `issue_target` after `issue` at 572-574.
  - `NewTask` 797-821: after `issue` at 810-812.
  - `TaskPatch` 823-848: after `issue` at 839-841.
  - Test `sagas_old_records_parse_without_kinds` 1270-1376.
  - The test `Task` literal at 1205.
- Create: `crates/borant-core/src/ports/issues.rs`.
- Modify: `crates/borant-core/src/ports/mod.rs`. Add `pub mod issues;` after `pub mod changes;` at line 5, and a `pub use issues::{..}` line after line 16.
- Modify: `crates/borant-core/src/testing.rs`. Add imports at 18-23, and `issue_tracker_contract` after `change_requests_contract` at 581-596.
- Create: `crates/borant-core/src/rules/mirror.rs`.
- Modify: `crates/borant-core/src/rules/mod.rs`. Add `pub mod mirror;` after `pub mod members;` at line 8.
- Modify: `crates/borant-core/Cargo.toml`. Add `sha2.workspace = true` to `[dependencies]` at 12-25. `sha2 = "0.10.9"` is already in the workspace at `Cargo.toml:44`. `Cargo.lock` changes.
- Modify: `crates/borant-hub/src/tasks.rs`:
  - `task_create` 58-131: store `new.issue_target` in the `Task` literal at 93-115.
  - `task_update` 133-280: the `issue` patch at 191-193, plus `issue_target` next to it.
- Create: `crates/borant-memory/src/issues.rs`.
- Modify: `crates/borant-memory/src/lib.rs`. Add `mod issues;` at 4-9, and `pub use issues::{IssueCall, MemoryIssues};` at 11-16.
- Modify: `crates/borant-memory/tests/contract.rs`. Add `issues_contract` after `changes_contract` at 42-56.
- Create: `crates/borant-exec/src/issues.rs`.
- Modify: `crates/borant-exec/src/lib.rs`. Add `pub mod issues;` after `pub mod changes;` at line 10.
- Create: `tests/plugins/issues-file.sh` (executable).
- Create: `crates/borant-exec/tests/fixtures/issues_request.sh` (executable).
- Create: `crates/borant-exec/tests/issues.rs`.
- Modify, literals only. Add `issue_target: None` after the `issue:` line of each `Task` literal:
  - `crates/borant-core/src/board.rs:584`
  - `crates/borant-core/src/rules/continuation.rs:211`
  - `crates/borant-core/src/rules/saga.rs:345`
  - `crates/borant-core/src/rules/graph.rs:159`
  - `crates/borant-core/src/rules/tasks.rs:195`, `:278`, `:334`
  - `crates/borant-core/tests/board_fixture.rs:93`
  - `crates/borant-hub/src/calls.rs:647`, `:668`
  - `crates/borant-hub/src/elastic.rs:1026`
  - `crates/borant-hub/src/http/tests.rs:94`, `:318`
  - `crates/borant-hub/tests/board.rs:82`
  - `crates/borant-hub/tests/prune.rs:228`
  - `crates/borant-hub/tests/restart.rs:212`
- Modify, literals only. The full `NewTask` literals:
  - `crates/borant-hub/src/events.rs:275-285`
  - `crates/borant-hub/tests/epics.rs:113`
  - `crates/borant-hub/tests/sagas.rs:123`
- Modify, literals only. The full `TaskPatch` literal: `crates/borant-hub/src/events.rs:410-421`. Every other `NewTask` and `TaskPatch` literal uses `..Default::default()`.

**Interfaces:**
- Consumes:
  - `Task`, `Channel`, `ChannelKind`, `TaskKind`, `CloseReason`, and `TaskStatus`.
  - `saga::approved` (`crates/borant-core/src/rules/saga.rs:32`) and `saga::descendants` (`:230`).
  - `crate::changes::PR_URL` exists only in the hub, so the core declares its own `pub const PR_URL: &str = "pr_url";` in `rules/mirror.rs`.
  - `borant_exec::{call, ExecConfig, VERSION}`.
- Produces, in `model.rs`:
  - `Task.issue_target: Option<String>`, `NewTask.issue_target: Option<String>`, and `TaskPatch.issue_target: Option<String>`, each `#[serde(default)]`.
  - Doc: "The team of the issue tracker for this item and the items under it. The core never reads it; the tracker adapter does."
- Produces, in the hub's `tasks.rs`:
  - `task_create` stores `new.issue_target`.
  - `task_update` sets it from the patch.
  - The clear (choice M1): an empty string in `TaskPatch.issue` or `TaskPatch.issue_target` stores `None`. Any other value stores `Some(value)`.
- Produces, in `ports/issues.rs`. All types derive `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`:
  - `pub enum IssueState { Backlog, Todo, Started, Done, Canceled }`, with `#[serde(rename_all = "snake_case")]`. It also derives `Copy` and `Hash`.
  - `pub struct IssueWrite { pub key: String, pub target: Option<String>, pub parent: Option<String>, pub title: String, pub body: String, pub state: IssueState }`. No `skip_serializing_if`: `null` goes on the wire.
  - `pub struct IssueRef { pub key: String, pub url: String }`.
  - The trait:

    ```rust
    #[async_trait::async_trait]
    pub trait IssueTracker: Send + Sync + std::fmt::Debug {
        async fn create(&self, w: &IssueWrite) -> PortResult<IssueRef>;
        /// `full` false writes the state only (an adopted issue).
        async fn update(&self, issue: &str, w: &IssueWrite, full: bool) -> PortResult<IssueRef>;
    }
    ```

  - `ports/mod.rs` re-exports `IssueRef`, `IssueState`, `IssueTracker`, and `IssueWrite`.
- Produces, in `testing.rs`: `pub async fn issue_tracker_contract(t: &dyn IssueTracker, target: &str)`.
- Produces, in `borant_memory::MemoryIssues` (`Clone`, with shared state as in `MemoryChanges`):
  - `new()` and `set_available(bool)`.
  - `seed(key, target) -> IssueRef`, an existing issue for an adoption.
  - `remove(key)`, an issue that is gone.
  - `fail_next(PortError)`, which fails the next call once.
  - `on_create(impl Fn(&IssueWrite) + Send + Sync + 'static)`, a hook that runs inside `create` before it returns.
  - `calls() -> Vec<IssueCall>`, where `pub enum IssueCall { Create(IssueWrite), Update { issue: String, write: IssueWrite, full: bool } }`.
  - `issue(key) -> Option<(IssueRef, Option<String> /* team */, IssueWrite)>` and `count() -> usize`.
  - A create with the same `key` returns the first `IssueRef` and makes nothing new. A create without `target` takes the parent's team. A create without both is `Invalid`. An update of an unknown key or URL is `NotFound`. Keys are `<team>-<n>`. URLs are `https://issues.example.test/<key>`.
- Produces, in `borant_exec::issues`:
  - `pub const KIND: &str = "issues";`
  - `pub const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);`
  - `pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);`
  - `pub struct ExecIssues { exec: ExecConfig, config: BTreeMap<String, Value> }` with:
    - `new(exec, config)`.
    - `async fn open(exec, config) -> PortResult<Self>`. It sends `describe` with `config` and checks `v == 1` and `kind == "issues"`, else `Invalid`.
  - `impl IssueTracker`:
    - `create` sends `{"config", "write"}`.
    - `update` sends `{"config", "issue", "write", "full"}`.
    - Each op runs with its own default timeout and `log_stderr: false`.
    - A reply of the wrong shape is `PortError::other`, with no reply text in the message, as `crates/borant-exec/src/changes.rs:46-55` does.
- Produces, in `rules/mirror.rs` (pure, no I/O):
  - `pub const PR_URL: &str = "pr_url";`
  - `pub enum ItemId { Channel(ChannelName), Task(ChannelName, TaskId) }`, with `fn borant_id(&self) -> String`. The id is `<channel>` for a channel and `<channel>/<task>` for a task.
  - `pub struct Item { pub id: ItemId, pub parent: Option<ItemId>, pub depth: u32, pub title: String, pub body: String, pub state: IssueState, pub target: Option<String>, pub link: Option<String>, pub closed: bool, pub order: Timestamp, pub ask: TaskId }`:
    - `parent` is the nearest mirrored ancestor.
    - `target` is the own or inherited `issue_target`.
    - `link` is today's `issue` of the item.
    - `ask` is the task that gets `needs_human`: the task itself, the gate of a saga, or the root of an epic channel.
  - `pub fn anchor(channel: &Channel) -> Option<TaskId>`: `gate` for a saga, `root` for an epic channel, else `None`.
  - `pub fn items(channel: &Channel, tasks: &[Task]) -> Vec<Item>`: every mirrored item of one channel, sorted by `depth`, then `order`.
  - `pub fn task_state(task: &Task) -> IssueState`.
  - `pub fn container_state(channel: &Channel, tasks: &[Task], epic: Option<&Task>) -> IssueState`. `None` means the channel itself.
  - `pub fn target_of(channel: &Channel, tasks: &[Task], id: &ItemId) -> Option<String>`.
  - `pub enum Team { Target(String), Parent, Missing }` and `pub fn team(target: Option<&str>, parent_issue: Option<&str>) -> Team`.
  - `pub fn body(text: &str, borant_id: &str, pr_url: Option<&str>) -> String`.
  - `pub fn record_hash(w: &IssueWrite, full: bool) -> String`.
  - `pub fn issue_key(borant_id: &str) -> String`.
  - `pub fn no_target_message(channel: &ChannelName, task: &TaskId) -> String`.

The rules, exactly:

- **`items`.** A channel without a project gives nothing. The hub checks `mirror_issues` before it calls `items`.
  - Saga channel: one `Channel` item.
    - `depth` 0, `parent` `None`, `order` `channel.created`, `ask` the gate.
    - Title: `channel.title`, else the name.
    - Body: the gate's body.
    - State: `container_state(.., None)`.
    - `link` is `channel.issue`.
    - `closed` is `channel.closed.is_some()`.
  - Epic channel: the same shape.
    - `ask` is the root.
    - Body: the root's body.
    - `link` is `channel.issue`, else the root's `issue` (choice M5).
    - `closed` is true when the channel or the root is closed.
  - Epic task in a saga: always mirrored.
    - `depth` 1, `parent` the channel item.
    - State: `container_state(.., Some(epic))`.
  - Root of an epic channel, and the gate of a saga: never mirrored.
  - Any other task: mirrored when `properties.pr_url` is a non-empty string, or when `issue` is set.
    - `parent`: the first mirrored task up the `parent` chain. Then the channel item, when the channel is a saga or an epic channel. Else `None`.
    - `depth`: the parent's `depth + 1`, or 0 without a parent.
    - State: `task_state`.
    - `order`: `task.updated`.
  - The walk up `parent` keeps a seen set, as `open_blockers` does.
- **`task_state`.**
  - `open` gives `Todo`.
  - `in_progress`, `needs_human`, and `stuck` give `Started`.
  - Closed `done` gives `Done`.
  - Any other close gives `Canceled`.
- **`container_state`.** The first rule that matches wins:
  1. Closed:
     - A saga channel that is closed gives `Done`, unless its gate closed with a reason other than `done`, which gives `Canceled` (choice M4).
     - An epic channel that is closed, or whose root is closed, gives `Done` when the root closed as `done`, or when the root is open and the channel closed. Otherwise it gives `Canceled`.
     - An epic task that is closed gives `Done` when its reason is `done`, else `Canceled`.
  2. A saga whose gate is not closed `done` gives `Backlog`.
  3. A child that is not `open` and not closed with a non-`done` reason gives `Started`. The children are:
     - for a saga: every task except the gate,
     - for an epic task: `saga::descendants`,
     - for an epic channel: every task except the root.
  4. Otherwise, `Todo`.
- **`target_of`.** The first match wins:
  1. The item's own `issue_target`. For a channel item, this is the anchor task's.
  2. The nearest task up `parent` that has one.
  3. The anchor task's `issue_target`, for a task in a saga or an epic channel.
  4. `None`.
- **`team`.**
  - `Some(t)` gives `Target(t)`.
  - Else, a parent issue gives `Parent`.
  - Else, `Missing`.
- **`body`.**
  - The text, then a blank line, then `` Mirrored from Borant (`<id>`). Edits here do not flow back. ``
  - When `pr_url` is set, a second line: `PR: <url>`.
  - An empty text gives only the footer.
- **`record_hash`.** The SHA-256, in lowercase hex, of the JSON of `{title, body, parent, state}` when `full` is true, or of `{state}` alone when it is false. `key` and `target` are never in the hash: a team set later does not rewrite an issue.
- **`issue_key`.**
  1. Take the first 16 bytes of `sha256("borant:" + id)`.
  2. Set `b[6] = (b[6] & 0x0f) | 0x40` and `b[8] = (b[8] & 0x3f) | 0x80`.
  3. Format the bytes as `8-4-4-4-12` lowercase hex.
- **`no_target_message`.** ``Issue team needed: set issue_target on this task (for example `borant task update <channel>/<task> --issue-target ENG`).``

Choices made here:
- **M1.** The spec has a "clearing the link" step, but today `TaskPatch.issue` cannot clear a value (`crates/borant-hub/src/tasks.rs:191-193`). An empty string clears `issue` and `issue_target`. A channel's `issue` has no update call. A saga's link is set at create and cannot change, and the spec's clearing step applies to tasks only.
- **M2.** The hash covers only what the mirror would write. An adopted issue hashes its state only, so a title edit costs no call.
- **M4.** The spec maps a closed channel to `done`. A saga that was rejected at its gate, or an epic channel whose root closed as `wontfix`, maps to `canceled` instead. This matches the board's `SagaState::Closed` (`crates/borant-core/src/board.rs:388-401`).
- **M5.** The hub writes the root epic of an epic channel with the channel's `issue` (`crates/borant-hub/src/calls.rs:668`). So the root's `issue` is the fallback link of the channel item.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/borant-core/src/model.rs` tests, extend `sagas_old_records_parse_without_kinds`: the old JSON parses with `issue_target == None`, and the round trip sets `issue_target: Some("ENG".into())` on a `Task`, a `NewTask`, and a `TaskPatch`. Rename nothing.
  - In `crates/borant-core/src/rules/mirror.rs` tests:
    - `mirror_task_state_maps_each_status`.
    - `mirror_container_state_rules`: one case for each rule, plus a rejected saga, a wontfix root, and a child closed as `wontfix` that does not start a container.
    - `mirror_items_of_a_saga`: the channel, the epics, a task with `pr_url`, a task with `issue`, a task with neither (left out), and the gate (left out). Assert the parents and the depths.
    - `mirror_items_of_an_epic_channel`: the root is left out, and a child's parent is the channel item.
    - `mirror_items_of_a_project_channel`: a plain task with `pr_url` has no parent. A task under a plain mirrored task gets it as its parent. A channel without a project gives nothing.
    - `mirror_target_resolution_order`: own, then ancestor, then anchor, then none.
    - `mirror_team_falls_back_to_the_parent`.
    - `mirror_record_hash`: stable, changes with each of the four fields when full, ignores the title when not full, and ignores `key` and `target`.
    - `mirror_issue_key_is_v4_shaped`: the form matches `^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`, is stable for one id, and differs for two ids.
    - `mirror_body_has_the_footer`: with and without `pr_url`, and with an empty body.
    - `mirror_no_target_message_is_neutral`: the message names the command and `issue_target`, and holds no vendor name.
  - In `crates/borant-core/src/ports/issues.rs` tests: `issues_write_has_the_wire_shape`. Serialize an `IssueWrite` with `target: None` and assert the exact JSON of the wire format above, `"target":null` included. Parse `{"key":"ENG-1","url":"u"}` as an `IssueRef`.
  - In `crates/borant-memory/tests/contract.rs`: `issues_contract` runs `testing::issue_tracker_contract(&MemoryIssues::new(), "T")`.
  - In `crates/borant-exec/tests/issues.rs`:
    - `issues_the_file_plugin_passes_the_contract`, with `tests/plugins/issues-file.sh` and a temp file in `config.file`.
    - `issues_a_program_of_another_kind_is_invalid`, using fixture `describe_notifier.sh`.
    - `issues_the_requests_match_the_wire_format`. Fixture `issues_request.sh` records the request in argv[1], then replies `{"ok":{"key":"T-1","url":"u"}}` for `create` and `update`, and `{"ok":{"v":1,"kind":"issues"}}` for `describe`. Assert each recorded request equals the JSON of the wire format: `describe` carries `config`, `create` carries `config` and `write`, and `update` carries `config`, `issue`, `write`, and `full`.
    - `issues_error_codes_map_to_kinds`, using fixture `error.sh` with `not_found`, `invalid`, `denied`, `unavailable`, and `internal`. `internal` maps to `Other`.
    - `issues_default_timeouts`, a unit test in `src/issues.rs`: 10 s and 30 s.
- [ ] **Step 2: Run** `just test-crate borant-core mirror`, `just test-crate borant-core issues`, and `just test-crate borant-core sagas_old_records`. Expected: FAIL, nothing exists yet.
- [ ] **Step 3: Add the fields and the patch apply.** Add the three fields in `model.rs`. Fix the 22 literals in the Files list. Store `new.issue_target` in `task_create`. Apply M1 to `issue` and `issue_target` in `task_update`.
- [ ] **Step 4: Write `ports/issues.rs`, the re-exports, and `issue_tracker_contract`.** The contract runs in this order:
  1. A create with `target` returns a non-empty key and URL.
  2. A second create with the same `key` returns the same `IssueRef`.
  3. A create with only `parent` set to that key passes.
  4. A create with neither is `Invalid`.
  5. `update(key, w, true)` and `update(url, w, false)` return the same key.
  6. `update("NOPE-999999", ..)` is `NotFound`.
- [ ] **Step 5: Write `MemoryIssues`** with the API above. Model it on `crates/borant-memory/src/changes.rs:1-65`, with `Availability`.
- [ ] **Step 6: Write `rules/mirror.rs`.** Add `sha2` to the core dependencies.
- [ ] **Step 7: Write `borant_exec::issues` and `tests/plugins/issues-file.sh`.** The plugin keeps `{"issues": {<key>: {...}}, "next": n}` in `config.file`, with jq:
  - It maps `write.key` to an issue key, so a second create returns the first issue.
  - A child without a target takes its parent's team.
  - It accepts a key or a URL ending in `/<key>` for `issue` and `parent`.
  - Its error codes are `invalid` and `not_found`.
  - It needs no network.
- [ ] **Step 8: Run** the commands of step 2, then:
  - `just test-crate borant-memory issues`,
  - `just test-crate borant-exec issues`,
  - `just test-crate borant-hub tasks`,
  - `just test-crate borant-hub http`,
  - `cargo check --workspace --all-targets`.

  Expected: PASS. Then run `rg -i linear crates tests`. Expected: no match.
- [ ] **Step 9: Commit** `feat(core): issue_target, the IssueTracker port, the mirror rules, and the exec adapter`.

### Task 2: Hub: the mirror loop

Blocked by Task 1. Runs in parallel with Task 3. Size: about 1,100 lines, 550 of them tests.

**Files:**
- Create: `crates/borant-hub/src/mirror.rs`.
- Modify: `crates/borant-hub/src/lib.rs`:
  - Add `pub mod mirror;` after `mod members;` at line 20, and `pub use mirror::IssueMirror;` after `pub use changes::ChangeWatch;` at line 37.
  - `HubConfig` 79-108: a `mirror` field after `changes` at 92-93.
  - `Default` 110-127: `mirror: None` after `changes: None` at 119.
  - `Memory` 162-220: `mirror_refused` before `maintenance` at 219.
  - `run`: spawn the mirror task after the watcher at 503-509, and abort it next to `watcher.abort()` at 516 and 536.
- Modify: `crates/borant-hub/src/changes.rs:220-237`. Make `open_channels` `pub(crate)`, so the mirror reads the same channels.
- Modify: `crates/borant-core/src/rules/alerts.rs`:
  - `MIRROR_REFUSED` after `PROVISIONER_REFUSED` at line 22.
  - `AlertView.mirror_refused: Option<String>` after `refused` at 67-68.
  - The rule after the refusal loop at 236-246.
  - A test after `provisioner_refused_fires_for_a_current_refusal` at 621-643.
- Modify: `crates/borant-hub/src/alerts.rs`. In `alert_view` at 168-215, read `mem.mirror_refused` next to `elastic_refused` at 201-204, and set it in the `AlertView` literal at 205-213.
- Modify: `crates/borant-core/src/config.rs`. Add `MirrorConfig` after `ChangesConfig` and its `Default` at 104-138.
- Modify: `crates/borant/src/config.rs`:
  - The import at line 11.
  - `use borant_exec::issues::ExecIssues;` after line 25.
  - A `HubFile.mirror` field after `changes` at 266-269.
  - `mirror_tracker()` after `changes_watch()` at 569-620.
  - Tests after the `changes_*` tests at 1883-1925.
- Modify: `crates/borant/src/roles.rs`. Call `file.mirror_tracker().await?` after `changes_watch` at 52-54, and pass `mirror` in the `HubConfig` literal at 60-73.
- Test: `crates/borant-hub/tests/mirror.rs`.

**Interfaces:**
- Consumes:
  - Task 1: `IssueTracker`, `IssueWrite`, `IssueState`, `rules::mirror::*`, `MemoryIssues`, `ExecIssues`, and `Task.issue_target`.
  - `Hub::open_channels` (`crates/borant-hub/src/changes.rs:222`), `Hub::tasks` (`crates/borant-hub/src/tasks.rs:22`), `Hub::tell` (`crates/borant-hub/src/inbox.rs:156`), and `Hub::task_event` (`crates/borant-hub/src/tasks.rs:624`).
  - `roles::mirror_issues` (`crates/borant-core/src/rules/roles.rs:255`), `self.cfg.collections`, and `keys::PROJECT_PREFIX`.
  - `Store::get`, `put`, and `upsert` (`crates/borant-hub/src/store.rs:40-88`).
- Produces:
  - In `mirror.rs`: `#[derive(Clone)] pub struct IssueMirror { pub tracker: Arc<dyn IssueTracker>, pub every: Duration, pub max_writes: usize }`, with a `Debug` that hides the tracker, as `ChangeWatch` does at `crates/borant-hub/src/changes.rs:26-39`.
  - `pub async fn mirror_loop(self: Arc<Self>, shutdown: CancellationToken)`: one sweep per `every`, with `MissedTickBehavior::Delay`, a copy of `change_loop` at `crates/borant-hub/src/changes.rs:67-86`.
  - `pub async fn sweep_mirror(&self, m: &IssueMirror) -> Result<usize, ApiError>`, public for tests. It returns the number of tracker calls.
  - The KV record, `mirror.<channel>.<task>` or `mirror.<channel>`:

    ```rust
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct MirrorRecord {
        #[serde(default)] pub issue: Option<String>,
        #[serde(default)] pub hash: Option<String>,
        #[serde(default)] pub owned: bool,
        #[serde(default)] pub stopped: Option<Stopped>,   // no_target | not_found | unlinked
        #[serde(default)] pub was: Option<TaskStatus>,
        #[serde(default)] pub linked: bool,               // M3
        #[serde(default)] pub refused: Option<String>,    // M6: the hash that got `invalid`
    }
    ```

  - `Memory.mirror_refused: Option<String>`.
  - `HubConfig.mirror: Option<IssueMirror>`.
  - `alerts::MIRROR_REFUSED = "mirror_refused"`, with alert key `mirror_refused:tracker`, KV key `alert.mirror_refused.tracker`, title "The issue tracker refused the mirror", and priority High.
  - `MirrorConfig { adapter: Option<String>, program: Option<String>, credential: Option<String>, every_secs: u64 /* 60 */, max_writes: usize /* 50 */, config: BTreeMap<String, serde_json::Value> }`, with `#[serde(deny_unknown_fields)]` and every field `#[serde(default)]`.
  - `HubFile::mirror_tracker(&self) -> anyhow::Result<Option<IssueMirror>>`, a copy of `changes_watch`:
    - no adapter gives `None`,
    - an adapter other than `exec` fails,
    - a missing `program` fails,
    - a `credential` without `CREDENTIALS_DIRECTORY` fails,
    - a program that fails `ExecIssues::open` gives a warn and `None`.

The sweep, exactly. `sweep_mirror`:

1. Returns `Ok(0)` when `mem.mirror_refused` is set.
2. Reads the projects, then `open_channels()`, which includes the closed channels.
3. Keeps each channel whose project resolves `mirror_issues` to true.
4. For each channel, reads `tasks` and calls `rules::mirror::items`.
5. Sorts every item of every channel by `(depth, order)`.
6. Keeps a map from `ItemId` to issue key, for the items whose issue is known: from the KV record, or made in this sweep.

For each item, while fewer than `max_writes` calls are made:

- **Read** `rec = get(mirror key)`.
- **The parent issue.** `item.parent` maps to a key through the map. A parent that is mirrored but has no key yet makes the child wait: no call, no stop, and no `needs_human` (choice M9).
- **The write.** Build `IssueWrite`:
  - `key: issue_key(borant_id)`,
  - `target: item.target`,
  - `parent`: the parent key,
  - title, body, and state from the item.
- **The hash.** `h = record_hash(&w, owned)`. `owned` is `rec.owned`, or true for a new create.
- **The cases.** Take the first that matches:
  1. **Stopped** `not_found` or `unlinked`:
     - If the item's `link` is now set and differs from `rec.issue`, adopt it: `owned: false`, the stop cleared, then case 5.
     - Else, skip.
  2. **No record, closed:** skip. Closed history is not backfilled.
  3. **No record, `link` set:** adopt.
     1. Upsert `{issue: link, owned: false, linked: true, hash: None}`.
     2. Then case 5, which writes the state at once.
  4. **No record, open, no link; or a record with `issue: None`:**
     - If `rec.refused == Some(h)`, skip.
     - If `rec.stopped == no_target`, first resolve the team again:
       - Resolve with `team(item.target, parent_key)`.
       - `Missing`, with no stop yet: raise `needs_human` (below), then skip.
       - `Missing`, already stopped: skip.
       - Found, and stopped `no_target`: restore (below), then create.
     - The create: `tracker.create(&w)`, and on success:
       1. Upsert `{issue: key, owned: true, hash: h, linked: false}` (M3).
       2. The link write.
       3. Upsert `linked: true`.
       4. Put the key in the map.
  5. **A record with an issue:**
     - If `rec.linked` is false and the item's `link` is `None`, redo the link write, set `linked: true`, and make no tracker call.
     - Else, if `rec.linked` is true and the item's `link` is `None`, the link was cleared. Upsert `stopped: unlinked` and skip.
     - Else, if `rec.hash != Some(h)` (`h` uses `rec.owned`):
       1. `tracker.update(&rec.issue, &w, rec.owned)`.
       2. Upsert `hash: h`.

  A closed item whose state changed has a new hash, so its final state goes out once. The spec's "also writes when the item is closed" adds nothing more (choice M10).
- **The errors** of a call:
  - `Unavailable`, `Timeout`, `Other`, or `Conflict`: a `warn` log line. The sweep goes on. The next sweep retries.
  - `NotFound`:
    1. Upsert `stopped: not_found`.
    2. Tell the channel owner once: `Issue <key> of <id> is gone from the tracker; the mirror stops for it. Set issue on the item to link another one.`
  - `Invalid`:
    1. Upsert `refused: Some(h)`. For a failed create, that is a record with `issue: None`.
    2. Tell the owner once: `The issue tracker refused <id>: <message>. The mirror skips it until it changes.`
  - `Denied`:
    1. Set `mem.mirror_refused = Some(message)`.
    2. Log an `error`.
    3. Stop the sweep.

  The pause holds until the next hub start.
- **The link write.** `link(&ItemId, key)` runs up to 5 times:
  1. Read the task, or the channel.
  2. If its `issue` already equals the key, stop.
  3. Set `issue = Some(key)` and, for a task, `updated = now`.
  4. Put with the revision of the read.
  5. On `Conflict`, read again.

  After 5 losses it returns `internal`, and the next sweep runs case 5 and links. `close_channel` (`crates/borant-hub/src/sagas.rs:80-107`) does a single revision-checked put after a fresh read (`crates/borant-hub/src/tasks.rs:436-438`). The retry loop is new here, shaped like `Store::upsert` (`crates/borant-hub/src/store.rs:78-88`).
- **Raise `needs_human`** on `item.ask`:
  1. Read the task.
  2. If it is closed, skip.
  3. Set `was: Some(status)`.
  4. Set `status = NeedsHuman` and `attention_msg = no_target_message(..)`.
     - When the task is already in `needs_human` (a gate that waits for approval), append the message to the existing `attention_msg` after a blank line instead, and keep `was: Some(NeedsHuman)` (choice M8).
  5. Set `updated = now`, then put with the revision of the read, as `crates/borant-hub/src/continuation.rs:127-133` does.
  6. Upsert the record: `stopped: no_target`, `issue: None`.
  7. Send `task_event(.., TaskEventKind::Updated, "needs an issue team")`, so the owner wakes.

  On `Conflict`, skip. The next sweep tries again.
- **Restore.**
  1. Read the task.
  2. If its `attention_msg` holds the mirror's message:
     - When `was` is `NeedsHuman`, strip the appended text and leave the status.
     - Else, set `status = was` and `attention_msg = None`, and put with the revision.
  3. Clear `stopped` and `was` in the record, whether or not the task changed.
- **The cap and the order.** Every tracker call counts. A link write, a restore, and a record write do not count. Items are visited by `(depth, order)`, so parents go first and the oldest go first.

Choices made here:
- **M3.** The spec writes the link, then the record. A crash between those two writes would make the next sweep adopt the hub's own issue as not owned. So the record goes first, with `linked: false`. Then the link, then `linked: true`. The `linked` field is new.
- **M6.** "Skips the item until its hash changes" needs the refused hash, so the record gets a `refused` field. A create that is refused leaves a record with `issue: None`, which is not a stop.
- **M7.** The spec's attention text names the vendor. The hub uses `rules::mirror::no_target_message`. It also names `<channel>/<task>`, because `borant task update` takes that form (Task 3), not a bare id.
- **M8.** A saga created with no team asks at its gate. The spec says "the two arrive together". The mirror must not overwrite the scope question of a gate that already waits, so it appends.
- **M9.** A child of an item that waits for a team, or that was capped out of this sweep, waits silently. Setting the ancestor's team fixes both, and Chris gets one question, not one per epic.
- **M10.** A closed item with a record gets one write, when its state changes. It does not get one every sweep.
- **M11.** The `needs_human` write goes straight to the store, with the revision of the read, as the continues limit does. It sends a task event, so the orchestrator wakes and asks the chief. `notify_approvers` is not called: the alert pass already reports every `needs_human` task (`crates/borant-hub/src/alerts.rs:185-194`).

To avoid conflicts with Task 3, which runs at the same time:
- **No shared source file.** Task 2 changes the hub loop, the alerts, and the config. Task 3 changes `proto.rs`, `calls.rs`, `mcp.rs`, `cli.rs`, `board.rs`, the Pi extension, the skills, and the guide.
- **The one coupling is `ToolCall::ChannelCreate`.** Task 3 adds `issue_target` to it and fixes its 47 literals. `crates/borant-hub/tests/mirror.rs` must not build a `ToolCall::ChannelCreate`. It seeds every channel, task, and project with `fx.state.seed(key, bytes)`, as `crates/borant-hub/tests/board.rs:110-122` does. It changes tasks only through `ToolCall::TaskUpdate` as a person (`fx.call(None, ..)`), which Task 1 already supports.
- **The attention text names `borant task update`,** which Task 3 adds. The text is a string, so neither PR waits for the other.
- **`Cargo.lock`:** neither task adds a dependency.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/borant-core/src/rules/alerts.rs` tests: `mirror_refused_fires_and_resolves`. A view with `mirror_refused: Some("denied")` fires `mirror_refused:tracker`. A view without it resolves the alert.
  - In `crates/borant/src/config.rs` tests:
    - `mirror_the_exec_adapter_builds_the_tracker`, with `tests/plugins/issues-file.sh`, `every_secs = 30`, `max_writes = 7`, and a `[mirror.config]` table.
    - `mirror_without_an_adapter_is_off`.
    - `mirror_a_credential_needs_the_directory_of_the_unit`.
    - `mirror_parses_the_spec_table`: the TOML of the spec section "Config", with the defaults 60 and 50.
  - In `crates/borant-hub/tests/mirror.rs`. The fixture: `Fx::with(HubConfig { mirror: Some(m), collections: {work: mirror_issues = true}, .. })`, with `m.tracker` a `MemoryIssues` and `every` 3600 s. Seed project `web` in `work`, one project channel `work-web` owned by a registered `boss`, one saga channel `work-saga` with its gate, and one epic channel `work-epic` with its root. Each test calls `fx.hub.sweep_mirror(&m)`.
    - `mirror_creates_once_then_updates_on_change`:
      - A task with `pr_url` and `issue_target: "T"` gets one create.
      - The task's `issue` is the returned key, and the record says `owned: true` and `linked: true`.
      - A second sweep makes no call.
      - A title change makes one `update` with `full: true`.
    - `mirror_adopted_issue_gets_state_only`:
      - A task linked to a seeded issue `T-9` gets `Update { full: false }` and no create.
      - The record says `owned: false`.
      - A title change makes no call. A status change makes one state-only update.
    - `mirror_crash_between_create_and_link_makes_no_duplicate`:
      - `on_create` sets the state store unavailable, so the record write fails.
      - Make it available again and sweep once more.
      - Two `Create` calls with the same `key`, `count() == 1`, and the task is linked.
    - `mirror_record_without_link_relinks`: a seeded record `{issue: "T-1", owned: true, linked: false}` and an unlinked task give no tracker call, a linked task, and `linked: true`.
    - `mirror_no_team_raises_needs_human_once_and_restores`:
      - A task with `pr_url`, no target, and status `in_progress` goes to `needs_human`, with the neutral message. The record says `stopped: no_target` and `was: in_progress`. There is no call.
      - A second sweep changes nothing: same revision, no call.
      - `task_update` with `issue_target: "T"`, as a person. The next sweep sets `in_progress` with no attention message, and makes one create.
    - `mirror_no_team_on_a_waiting_gate_keeps_its_question`: a saga gate in `needs_human` with a scope message keeps the scope text, with the team text appended. After the target is set, the scope text is back alone.
    - `mirror_sub_issue_without_target_uses_parent_team`:
      - The saga channel is linked to seeded `T-1`, and no task has a target.
      - The epic is created with `target: None` and `parent: Some("T-1")`.
      - `MemoryIssues::issue` reports team `T`.
    - `mirror_task_gets_issue_when_pr_url_appears`: a task without `pr_url` makes no call. After `task_update` sets `properties.pr_url`, one create.
    - `mirror_not_found_is_told_once`:
      - `remove` the linked issue and change the task.
      - The owner's inbox has one message. The record says `stopped: not_found`.
      - A later change makes no call and no message.
    - `mirror_invalid_is_told_once_until_the_hash_changes`: with `fail_next(invalid)`, one message, then no call. A title change retries.
    - `mirror_denied_pauses_until_restart`:
      - With `fail_next(denied)`, the sweep stops. The next sweep returns 0 calls.
      - After `fx.advance` past the alert interval and `fx.hub.tick()`, the key `alert.mirror_refused.tracker` exists.
    - `mirror_unavailable_retries_next_sweep`: `set_available(false)` gives no record. Then it is available again, and one create.
    - `mirror_unlinked_stops`: `task_update` with `issue: ""` sets `stopped: unlinked`, and makes no further call.
    - `mirror_max_writes_caps_parents_first`:
      - `max_writes = 2`, with a saga that has a team, two epics, and one task.
      - The first sweep creates the saga and the older epic. The second creates the other epic and the task.
      - The calls come in that order.
    - `mirror_closed_item_without_record_makes_no_call`: a closed task with `pr_url`, and a closed saga.
    - `mirror_off_project_makes_no_calls`: a project with `mirror_issues: Some(false)` in `work`, and a project in a collection with the mirror off.
    - `mirror_link_write_retries_a_lost_race`:
      - `on_create` re-seeds the task with a new title, which bumps its revision.
      - The task ends linked and keeps the new title.
      - `count() == 1`.
    - `mirror_saga_state_follows_the_gate_and_children`:
      - `Backlog` before approval.
      - `Todo` after the gate closes as `done`.
      - `Started` when a child goes `in_progress`.
      - `Done` after the channel closes.
- [ ] **Step 2: Run** `just test-crate borant-hub mirror`, `just test-crate borant-core mirror_refused`, and `just test-crate borant mirror`. Expected: FAIL.
- [ ] **Step 3: Add `MirrorConfig`, `HubFile.mirror`, `mirror_tracker`, `HubConfig.mirror`, and the wiring in `roles.rs`.** `ExecConfig` takes `command: vec![program]`, `credential` as in `changes_watch` (`crates/borant/src/config.rs:588-596`), the timeout `WRITE_TIMEOUT`, and `log_stderr: false`, because the plugin holds a personal key.
- [ ] **Step 4: Add the alert rule, `AlertView.mirror_refused`, and `Memory.mirror_refused`,** and read it in `alert_view`.
- [ ] **Step 5: Write `mirror.rs`:** `IssueMirror`, `MirrorRecord`, `mirror_loop`, `sweep_mirror`, `link`, `raise_no_target`, and `restore`. Spawn `mirror_loop` in `run`, next to the watcher, and abort it in the two places.
- [ ] **Step 6: Run** the commands of step 2, then `just test-crate borant-hub alerts`, `just test-crate borant-hub changes`, and `just test-crate borant config`. Expected: PASS. Then run `rg -i linear crates`. Expected: no match.
- [ ] **Step 7: Commit** `feat(hub): the issue mirror sweep, its KV record, and the mirror_refused alert`.

### Task 3: Surface: channel create, flags, board, skills, and guide

Blocked by Task 1. Runs in parallel with Task 2. Size: about 450 lines, plus a mechanical churn of 47 `ToolCall::ChannelCreate` literals (one line each).

**Files:**
- Modify: `crates/borant-core/src/proto.rs`. Add `#[serde(default)] issue_target: Option<String>` to `ToolCall::ChannelCreate` at 340-362, after `issue`.
- Modify: `crates/borant-hub/src/calls.rs`:
  - The `ChannelCreate` arm 184-209.
  - `channel_create` 575-718: the signature at 576-587, the gate `Task` literal at 632-652, and the root `Task` literal at 653-673.
- Modify, churn only: every other `ToolCall::ChannelCreate {` literal gets `issue_target: None`. Today that is `crates/borant-hub/src/events.rs:258`, `crates/borant-host/tests/daemon.rs:124` and `:573`, `crates/borant-host/tests/wait.rs:46`, and 43 sites in 30 files under `crates/borant-hub/tests/`. Find them with `grep -rn "ToolCall::ChannelCreate {" crates`.
- Modify: `crates/borant/src/mcp.rs`:
  - The params `ChannelCreate` 63-79, `TaskCreate` 103-118, and `TaskUpdate` 133-148.
  - `channel_create_call` 273-278 and `task_create_call` 280-286.
  - The `task_update` tool 459-472: move its JSON into `fn task_update_call(a: TaskUpdate) -> Value`.
  - Tests 635-679.
- Modify: `crates/borant/src/cli.rs`:
  - `TaskCmd` 340-352: add `Update`.
  - The handler 1399-1422.
  - `render_board` 1070-1158 and `epic_text` 1160-1170.
  - Test `board_renders_projects_sagas_and_questions` 2716-2737.
  - A parse test after `task_close_parses_the_reason_and_the_message` at 2011-2034.
- Modify: `crates/borant-core/src/board.rs`. `epic_channel_line` 360-367 reads `channel.issue` before the root's `issue` (M5).
- Modify: `integrations/pi/borant.ts`: `channel_create` 403-415, `task_create` 440-454, and `task_update` 464-479.
- Modify: `integrations/claude-code/skills/orchestrate/SKILL.md`:
  - The facts table 15-35: one row after line 34.
  - "Set up" 37-51: step 3.
  - "Sagas and epics" 53-67: step 1.
  - "Red flags" 241-255: one row.
- Modify: `integrations/claude-code/skills/chief-of-staff/SKILL.md`. One facts row after line 35, and one red-flag row after line 156.
- Modify: `docs/guide.md`:
  - `[mirror]` in the `hub.toml` example of "Run the hub" at 30-41.
  - A new `### Issue mirror` after "Sagas and epics", which ends at 312.
  - A row for `borant task update` in "Commands" at 427-457.

**Interfaces:**
- Consumes: Task 1's `NewTask.issue_target`, `TaskPatch.issue_target`, the M1 clear, and `rules::mirror::anchor`.
- Produces:
  - `ToolCall::ChannelCreate { .., issue_target: Option<String> }`. A saga's gate task and an epic channel's root task are stored with it. On a project channel it is `invalid`: "issue_target needs a saga or an epic channel; set it on the task".
  - MCP params `channel_create { issue_target? }`, `task_create { issue_target? }`, and `task_update { issue_target? }`. Doc: "The issue tracker team for this item and the items under it, for example ENG. An empty string clears it."
  - Pi: the same three params.
  - CLI: `borant task update <channel>/<id> [--issue-target T] [--issue I]`. It sends `ToolCall::TaskUpdate` with only those fields set, and prints the reply with `pretty`. At least one flag is required. `--issue ""` and `--issue-target ""` clear.
  - Board text:
    - A saga line gains `  <issue>` after the title when `issue` is set: `saga #c  Title  ENG-1  approved  3/5 epics`.
    - `epic_text` gains it after the title.
    - The JSON already carries `issue` on `SagaLine` and `EpicLine` (`crates/borant-core/src/board.rs:69-98`).

Choices made here:
- **M12.** The board has no task lines. It shows plain tasks only as counts (`ProjectBoard.tasks`). The spec's "a task line shows its `issue` key" has nothing to change, so the board shows keys on saga and epic lines only.
- **M13.** No `borant task create` exists, and a person cannot create a channel (`crates/borant-core/src/proto.rs:592-603`). So the CLI flag lives on the new `borant task update` only. It is also the command that the mirror's question names.
- **M14.** The chief-of-staff skill gets one row, because the spec says "the chief asks Chris". The orchestrate skill carries the main rule.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/borant-hub/tests/sagas.rs`, using its fixture at 122-160:
    - `sagas_issue_target_goes_on_the_gate`: `channel_create` of a saga with `issue_target: Some("ENG")` stores it on the gate task.
    - `sagas_issue_target_goes_on_the_epic_root`: the same for an epic channel and its root.
    - `sagas_issue_target_on_a_project_channel_is_invalid`.
  - In `crates/borant-hub/tests/tasks.rs`: `tasks_issue_target_is_stored_and_cleared`. Set it with `task_create`, change it with `task_update`, clear it with `""`, and clear `issue` with `""`.
  - In `crates/borant/src/mcp.rs` tests:
    - Extend `channel_create_passes_kind_title_and_issue` and `task_create_passes_kind_and_issue` with `issue_target`.
    - Add `task_update_passes_issue_target`.
  - In `crates/borant/src/cli.rs` tests:
    - `task_update_parses_issue_target`.
    - `task_update_needs_a_flag`.
    - Update `board_renders_projects_sagas_and_questions` for the issue keys. Its fixture `board_view()` sets `issue` on one saga and one epic.
  - In `crates/borant-core/src/board.rs` tests: `board_epic_channel_line_reads_the_channel_issue`.
  - The Pi extension has no tests of tool params. `integrations/pi/borant.test.ts` covers targets, SSE, and events only. So `just test-pi` checks the new params by type check alone.
- [ ] **Step 2: Run** `just test-crate borant-hub issue_target`, `just test-crate borant-hub tasks_issue_target`, `just test-crate borant mcp`, `just test-crate borant task_update`, and `just test-crate borant-core board_epic_channel`. Expected: FAIL.
- [ ] **Step 3: Add the `ChannelCreate` field, and fix every literal** (`issue_target: None`). The arm in `calls.rs` passes the field on, and `channel_create` puts it on the gate or the root.
- [ ] **Step 4: Add the MCP params, `task_update_call`, the CLI `task update`, and the Pi params.**
- [ ] **Step 5: Change `epic_channel_line`, `render_board`, and `epic_text`.**
- [ ] **Step 6: Run** the commands of step 2, then `just test-crate borant-hub sagas`, `just test-crate borant-hub board`, `just test-crate borant board`, `just test-crate borant lists_all_tools`, `just test-pi`, and `cargo check --workspace --all-targets`. Expected: PASS.
- [ ] **Step 7: Update the orchestrate skill.** The skills may name Linear.
  - Facts row:
    - Fact: "In a project with `mirror_issues` on, the hub copies each saga, epic, and task with a PR to the issue tracker (Linear) and writes the key into `issue`. A create needs a team: the item's `issue_target`, an ancestor's, or the parent issue's. With none, the item goes `needs_human` with 'Issue team needed'."
    - So: "Set `issue_target` (the Linear team key, for example `ENG`) at `channel_create` for a saga or an epic, or on the first task of a plain project. When you do not know the team, ask the chief before you create. Answer a team question by `task_update(issue_target: ...)`; the hub then restores the task's status."
  - "Set up" step 3 and "Sagas and epics" step 1: add `issue_target` next to `issue`.
  - Red-flag row: "Guess the Linear team" → "Ask the chief. A wrong team puts the issue in the wrong place; the mirror never moves it."
- [ ] **Step 8: Update the chief-of-staff skill.**
  - A facts row: "An 'Issue team needed' question means the mirror lacks a Linear team." So: "Ask the person for the team key, then `task_update(issue_target: <key>)` on that task, or tell the orchestrator."
  - A red-flag row: "Pick a team for the orchestrator" → "Ask the person."
- [ ] **Step 9: Update `docs/guide.md`.**
  - In "Run the hub", the `[mirror]` table of the spec section "Config", with comments.
  - `### Issue mirror` covers:
    - one way;
    - what maps to what, the table of the spec in short;
    - the states;
    - the team order and the `needs_human` question;
    - adopting an existing issue by setting `issue`, and clearing it with `""`;
    - the KV record `mirror.*`;
    - the errors, and the `mirror_refused` alert;
    - the sweep, `every_secs`, and `max_writes`.
  - The exec protocol of `kind: "issues"`, with the wire format above. Linear is named as the tracker of the private plugin.
  - A "Commands" row: `borant task update <channel>/<id> [--issue-target T] [--issue I]`.
- [ ] **Step 10: Run** `rg -i linear crates integrations/pi tests`. Expected: no match. Then **Commit** `feat: issue_target on channel create, the CLI, MCP, and Pi; issue keys on the board; the mirror in the skills and the guide`.

---

**Out of scope, in the private `borant-infra` repo:**
- The `borant-mirror-linear` exec plugin (`pkgs/borant-mirror-linear.sh`), written against the wire format above.
- The `just hub-linear-credential` recipe and the `LoadCredential` of the hub unit.
- The `hub.mirror.enable` option, which adds `[mirror]` and sets `mirror_issues = true` on the work collection.

That repo pins the release that has Task 2 before it enables the table (Review Focus 6).
