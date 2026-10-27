# Sagas and epics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub knows sagas, epics, and tasks. It stores the kind of each channel and task, holds a saga at its approval gate, starts an epic's workers on the top branch of the epic's stack, closes channels, and serves one board read over MCP, the CLI, and HTTP.

**Architecture:** Five tasks, one PR each. Task 1 puts the types and the pure rules in `borant-core`. Task 2 wires channel kinds, the gate, and the approval into the hub. Task 3 adds `channel_close`, the closed-channel and spawn rules (with the epic branch), and one Docker scenario. Task 4 builds the board. It runs in parallel with Tasks 2 and 3, because it only reads records and seeds them in its tests. Task 5 gives agents and people the new fields (MCP, Pi, CLI), and updates the skills and the guide.

```
Task 1 ──> Task 2 ──> Task 3 ──┐
   └─────> Task 4 ─────────────┴──> Task 5
```

**Tech Stack:** Rust (serde, tokio, axum, clap, rmcp, schemars), TypeScript (Pi extension, bun), bash (Docker e2e).

**Spec:** `docs/superpowers/specs/2026-10-26-sagas-and-epics-design.md`

**Split, and why it differs from the suggested four tasks:** the hub rules do not fit one PR. Adding `kind`, `title`, and `issue` to `ToolCall::ChannelCreate` alone touches 45 struct literals in 30 test files (about 140 lines of churn). The gate and the approval are about 700 lines with their tests. So the hub rules are two tasks: Task 2 (kinds, gate, approval) and Task 3 (channel close, spawn rules, e2e). The MCP and CLI flags for Task 2's fields move to Task 5, next to the skills that use them. `channel_close` keeps its MCP tool and CLI in Task 3, and `board` keeps its in Task 4, so each new call ships with its surface.

## Global Constraints

- Ports and adapters. No core type, rule, error text, or config key names a vendor. The field is `issue`, never `linear`. `gh stack` appears only in the skills, never in code.
- Only the hub writes control-plane keys. The gate task and the root epic are written by the hub inside `channel_create`.
- Every new field of a stored record is `#[serde(default)]`. The hub rewrites no record.
- Hooks and the Pi extension exit 0 with no output when the daemon is down. No task changes that.
- Outside CI, run focused tests only: `just test-crate <crate> <filter>`. Run `just fmt` and `just lint` before each push. Task 5 also runs `just test-pi`.
- The host daemon needs no logic change. It forwards every new `ToolCall` to the hub as today (`crates/borant-host/src/delivery.rs:385-397` handles only `send` and the histories). Task 1 adds one match arm in `crates/borant-host/src/api.rs`.
- `borant-memory`, `borant-nats`, and the contract suite in `borant-core::testing` need no change. There is no new port. The new fields ride in the JSON of existing keys.
- Test names start with the filter of their task (`saga`, `sagas_`, `epics_`, `board_`), because `cargo test` filters on the test path, not the file name.

## Review Focus

1. **No path to work around the gate.** Before the gate closes as `done`, `task_claim`, `spawn` (unbound, `TaskBinding::Existing`, and `TaskBinding::New`), and `task_list(ready_only)` must all refuse or return nothing. `task_update` cannot close a task (`crates/borant-core/src/rules/tasks.rs:271-276`), and `cancel` closes only a bound task, which a saga cannot have before approval. Task 2 test `sagas_gate_refuses_claim_and_ready`, Task 3 test `epics_spawn_refuses_an_unapproved_saga`.
2. **Who may close the gate.** The owner of the saga channel must not approve its own scope unless its role has `approves = true`. Task 2 test `sagas_owner_cannot_approve`.
3. **Old records parse.** A Channel, Task, Project, NewTask, and TaskPatch JSON from v0.16.0 must parse with the defaults. Task 1 test `sagas_old_records_parse_without_kinds`.
4. **Rollout order.** `RoleConfig` and `CollectionConfig` use `#[serde(deny_unknown_fields)]` (`crates/borant-core/src/rules/roles.rs:15`, `:61`). A hub older than this work refuses a `hub.toml` with `approves` or `mirror_issues`. `borant-infra` adds the keys only after the hub runs the release that has Task 1.
5. **Rule 6 changes `ready` for every channel.** A child of a parent with an open `blocked_by` is no longer ready, also in project channels. That is the spec. The claim check uses it only in saga and epic channels (see Task 1, choice C1), so a project channel claims as today.
6. **The approver message stays in its collection.** The direct message for a gate in `needs_human` goes only to approver agents of the saga's collection, never to an approver of another collection. Task 2 test `sagas_gate_needs_human_messages_the_approvers`.
7. **The board must not hold the hub request mutex.** It reads every task, like `ToolCall::View`. Task 4 routes it before the mutex in `Hub::handle` (`crates/borant-hub/src/lib.rs:309-331`). An agent sees only its own collection. Task 4 test `board_agent_sees_only_its_collection`.
8. **A failed gate write leaves no stuck saga.** `channel_create` writes the channel first (it fails on a name that exists), then the gate. If the gate write fails, the hub deletes the channel record and its membership before it returns the error. Task 2 step 4.
9. **Churn stays mechanical.** The 45 `ToolCall::ChannelCreate` literals in Task 2 get only `kind: ChannelKind::Project, title: None, issue: None`. A reviewer skims them.

---

### Task 1: Core types and rules

Blocked by nothing.

**Files:**
- Modify: `crates/borant-core/src/model.rs` (`Channel` 430-453, `Project` 455-473, `Task` 500-532, `NewTask` 747-766, `TaskPatch` 768-789, test literals 1131-1172; add the two enums after `CloseReason` at 491-498)
- Modify: `crates/borant-core/src/error.rs` (`CoreError` 4-35)
- Modify: `crates/borant-core/src/proto.rs` (`ApiErrorCode` 26-83, `From<CoreError>` 121-131, test `every_known_error_kind_parses_and_round_trips` 903-925)
- Create: `crates/borant-core/src/rules/saga.rs`; Modify: `crates/borant-core/src/rules/mod.rs` (add `pub mod saga;`)
- Modify: `crates/borant-core/src/rules/tasks.rs` (`ready` 9-22, test literals 148-165, 229-245, 283-300)
- Modify: `crates/borant-core/src/rules/roles.rs` (`RoleConfig` 16-57, `CollectionConfig` 62-68, next to `git_author` 233-235, literal 248)
- Modify, literals only: `crates/borant-core/src/rules/graph.rs:143`, `crates/borant-core/src/rules/schedules.rs:21`, `crates/borant-core/src/rules/continuation.rs:196`, `crates/borant-core/tests/roles.rs:62`, `:77`
- Modify: `crates/borant-host/src/api.rs:26-34` (one arm for the new error code)
- Modify: `crates/borant-hub/src/http.rs:84-92` (one arm for the new error code)
- Modify, literals only: `crates/borant-hub/src/tasks.rs:91`, `crates/borant-hub/src/calls.rs:590`, `crates/borant-hub/src/elastic.rs:1011`, `crates/borant-hub/src/http/tests.rs:79`, `:145`, `:296`, `crates/borant-hub/tests/restart.rs:166`, `crates/borant-hub/tests/prune.rs:240`, `crates/borant-hub/tests/export.rs:447`, `crates/borant-hub/tests/projects.rs:16`, `crates/borant-hub/tests/collections.rs:56`, `:183`, `crates/borant-hub/tests/bundles.rs:105`, `:189`
- Modify: `crates/borant/src/cli.rs` (`ProjectCmd::Add` 354-377, its handler 1324-1358, a test after `project_add_names_a_collection` at 1753)
- Modify: `crates/borant/src/config.rs` (a test after `parses_hub_collections` at 1026)

**Interfaces:**
- Consumes: `Task`, `Channel`, `NewTask`, `Project`, `TaskStatus`, `CloseReason`, `RoleConfig`, `CollectionConfig`.
- Produces:
  - `pub enum ChannelKind { #[default] Project, Saga, Epic }` and `pub enum TaskKind { #[default] Task, Epic }` in `model.rs`. Both derive `Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize` with `#[serde(rename_all = "snake_case")]`.
  - `Channel { .., kind: ChannelKind, title: Option<String>, issue: Option<String>, gate: Option<TaskId>, root: Option<TaskId> }`, each `#[serde(default)]`.
  - `Task` and `NewTask` gain `kind: TaskKind` and `issue: Option<String>`. `TaskPatch` gains `issue: Option<String>`. All `#[serde(default)]`.
  - `Project { .., mirror_issues: Option<bool> }`, `CollectionConfig { .., mirror_issues: bool }`, `RoleConfig { .., approves: bool }`, all `#[serde(default)]`.
  - `CoreError::SagaNotApproved(ChannelName)`, `CoreError::ChannelClosed(ChannelName)`, `CoreError::EpicNotWork(TaskId)`, `CoreError::Structure(String)`, `CoreError::Blocked { task: TaskId, by: Vec<TaskId> }`.
  - `ApiErrorCode::SagaNotApproved`, wire name `saga_not_approved`. `From<CoreError>`: `SagaNotApproved` maps to it, `ChannelClosed` and `Blocked` map to `Conflict`, `EpicNotWork` and `Structure` to `Invalid`.
  - In `rules/tasks.rs`: `pub fn ready(tasks: &[Task]) -> Vec<TaskId>` (same signature, new meaning) and `pub fn open_blockers(tasks: &[Task], id: TaskId) -> Vec<TaskId>`.
  - In `rules/roles.rs`: `pub fn approves(role: Option<&RoleName>, roles: &BTreeMap<RoleName, RoleConfig>) -> bool` and `pub fn mirror_issues(project: &Project, collection: Option<&CollectionConfig>) -> bool`.
  - In `rules/saga.rs`:
    - `pub const GATE_TITLE: &str = "Approve the scope and the epics";`
    - `pub fn check_channel(kind: ChannelKind, project: Option<&ProjectName>) -> Result<(), CoreError>`
    - `pub fn approved(channel: &Channel, tasks: &[Task]) -> bool`
    - `pub fn ready_in(channel: &Channel, tasks: &[Task]) -> Vec<TaskId>`
    - `pub fn check_create(channel: &Channel, tasks: &[Task], new: &NewTask) -> Result<Option<TaskId>, CoreError>` (returns the parent to store)
    - `pub fn check_parent(channel: &Channel, tasks: &[Task], task: &Task, parent: TaskId) -> Result<(), CoreError>`
    - `pub fn check_claim(channel: &Channel, tasks: &[Task], task: &Task) -> Result<(), CoreError>`
    - `pub fn check_spawn(channel: &Channel, tasks: &[Task], bound: Option<&Task>) -> Result<(), CoreError>`
    - `pub fn epic_of(tasks: &[Task], id: TaskId) -> Option<&Task>`
    - `pub fn descendants(tasks: &[Task], id: TaskId) -> Vec<&Task>` (every task below `id`, without epics)
    - `pub fn epic_status(epic: &Task, tasks: &[Task]) -> TaskStatus`
    - `pub fn epic_branch(tasks: &[Task], id: TaskId) -> Option<String>`
  - CLI: `borant project add .. --mirror-issues true|false`.

The rules, exactly:

- `ready`: a task is ready when its status is `open`, its kind is not `epic`, and `open_blockers` is empty. `open_blockers` is every `blocked_by` id of the task and of each parent up the chain that is not a `closed` task in `tasks`. An unknown blocker counts as open, as today. The walk keeps a seen set, so a parent cycle in old data ends.
- `approved`: true when `channel.gate` is `None`, or when the gate task is `closed` with `close_reason == Some(Done)`. A gate id with no task is not approved (it fails closed).
- `ready_in`: empty when `!approved`, else `ready(tasks)`.
- `check_channel`: `Saga` and `Epic` need a project. Error: `Structure("a saga channel needs a project")`, and the same for an epic channel.
- `check_create`: a closed channel gives `ChannelClosed`. Project channel: kind `epic` gives `Structure`. Saga channel: an epic must have no parent; a task must have a parent whose chain reaches a task of kind `epic` (the gate is not an epic, so a child of the gate is refused). Epic channel: kind `epic` is refused (the root is the only one); a task without a parent gets `channel.root`. Returns the parent to store.
- `check_parent` (a `task_update` with `parent`): project channel: `Ok`. Saga and epic channels: an epic gets no parent, the gate gets no parent, and the new parent's chain must reach an epic.
- `check_claim`: in this order, `ChannelClosed`, `EpicNotWork` for kind `epic`, `SagaNotApproved` when `!approved`, then, only in saga and epic channels and only for an `open` task, `Blocked` when `open_blockers` is not empty.
- `check_spawn`: `ChannelClosed`, `SagaNotApproved`, then for a bound task: `EpicNotWork`, and `Blocked` as in `check_claim`.
- `epic_of`: the task itself if it is an epic, else the first epic up its parent chain.
- `epic_status`: the epic's own status when it is `closed`, `stuck`, or `needs_human`. Else `in_progress` when one task of `descendants` is not `open`, else `open`. An epic with no children is `open`.
- `epic_branch`: the `branch` property of `epic_of`, when it is a non-empty string.
- `mirror_issues`: `project.mirror_issues`, else the collection's `mirror_issues`, else false.

Choices made here (the spec leaves them open):
- **C1.** The spec says "the claim check and the board counts use the same rule" (rule 6), but today `task_claim` checks no `blocked_by` at all (`crates/borant-hub/src/tasks.rs:282-347`). The block check on a claim and a bound spawn applies only in saga and epic channels. Project channels keep today's claims.
- **C2.** `epic_status` looks at every descendant, not only direct children.
- **C3.** The text of every new error, as in the bullets above.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/borant-core/src/model.rs` tests: `sagas_old_records_parse_without_kinds`. Literal JSON of a Channel, a Task, a Project, a NewTask, and a TaskPatch as v0.16.0 wrote them (no new keys). Assert `kind` is `Project` / `Task`, and `title`, `issue`, `gate`, `root`, `mirror_issues` are `None`. Then a round trip with every new field set.
  - In `crates/borant-core/src/rules/tasks.rs` tests: `ready_skips_epics`, `ready_waits_for_the_blockers_of_each_parent` (epic 2 blocked by epic 1: a child of epic 2 is not ready until epic 1 closes; a grandchild too), `ready_ends_on_a_parent_cycle`.
  - In `crates/borant-core/src/rules/saga.rs` tests: `saga_unapproved_until_the_gate_is_done` (open, `needs_human`, `wontfix`, `done`, and a missing gate task), `saga_ready_in_is_empty_before_approval`, `saga_create_rules_by_kind` (one case for each bullet of `check_create`, plus a closed channel), `saga_epic_channel_parents_to_the_root`, `saga_check_parent_keeps_the_chain`, `saga_claim_order` (closed before epic before gate before blocked), `saga_claim_checks_blocks_only_in_saga_and_epic_channels`, `saga_spawn_refuses_an_epic_binding`, `saga_epic_status_from_descendants`, `saga_epic_branch_reads_the_nearest_epic`, `saga_check_channel_needs_a_project`.
  - In `crates/borant-core/src/rules/roles.rs` tests: `roles_approves_reads_the_flag`, `roles_mirror_issues_project_wins_over_collection`.
  - In `crates/borant-core/src/proto.rs`: add `("saga_not_approved", ApiErrorCode::SagaNotApproved)` to the cases of `every_known_error_kind_parses_and_round_trips`.
  - In `crates/borant/src/config.rs`: `hub_file_parses_saga_settings` (`[roles.chief] approves = true`, `[collections.work] mirror_issues = true`, and both default to false).
  - In `crates/borant/src/cli.rs`: `project_add_sets_mirror_issues` (`--mirror-issues false` gives `Some(false)`; no flag gives `None`).
- [ ] **Step 2: Run** `just test-crate borant-core saga`, `just test-crate borant-core ready`, `just test-crate borant-core roles`, `just test-crate borant-core every_known_error`. Expected: FAIL, the types and functions do not exist.
- [ ] **Step 3: Add the enums, the fields, the errors, and `ApiErrorCode::SagaNotApproved`.** Add the arm to `crates/borant-host/src/api.rs` (`StatusCode::CONFLICT`) and to `crates/borant-hub/src/http.rs` (with `Conflict`).
- [ ] **Step 4: Write `rules/saga.rs`, the new `ready` and `open_blockers`, `approves`, and `mirror_issues`.**
- [ ] **Step 5: Fix every struct literal in the Files list.** Use the defaults: `kind: TaskKind::Task, issue: None` on a Task, the five defaults on a Channel, `mirror_issues: None` on a Project, `mirror_issues: false` on a CollectionConfig. `crates/borant-hub/src/tasks.rs:91` stores the defaults too; Task 2 makes it store `new.kind` and `new.issue`.
- [ ] **Step 6: Add `--mirror-issues`** to `ProjectCmd::Add` as `#[arg(long, action = clap::ArgAction::Set)] mirror_issues: Option<bool>`, and pass it into the `Project` at `crates/borant/src/cli.rs:1349`.
- [ ] **Step 7: Run** the commands of step 2, then `just test-crate borant config`, `just test-crate borant project_add`, `just test-crate borant-hub` with filter `http`, and `cargo check --workspace --all-targets`. Expected: PASS.
- [ ] **Step 8: Commit** `feat(core): channel and task kinds, the saga rules, and saga_not_approved`.

### Task 2: Hub: channel kinds, the saga gate, and approval

Blocked by Task 1.

**Files:**
- Modify: `crates/borant-core/src/proto.rs` (`ToolCall::ChannelCreate` 332-342)
- Modify: `crates/borant-hub/src/calls.rs` (the `ChannelCreate` arm 181-193, `channel_create` 550-616)
- Create: `crates/borant-hub/src/sagas.rs`; Modify: `crates/borant-hub/src/lib.rs` (add `mod sagas;` after `mod retire;` at line 24)
- Modify: `crates/borant-hub/src/tasks.rs` (`task_list` 43-55, `task_create` 57-122, `task_update` 124-245, `task_claim` 282-347, `task_close` 349-402, `is_owner_or_person` 631)
- Modify, churn only: every `ToolCall::ChannelCreate {` literal. Today: `crates/borant-hub/src/events.rs` (1), `crates/borant-host/tests/daemon.rs` (2), `crates/borant-host/tests/wait.rs` (1), and 41 sites in 28 files under `crates/borant-hub/tests/`. Find them with `grep -rn "ToolCall::ChannelCreate {" crates`.
- Test: `crates/borant-hub/tests/sagas.rs`

**Interfaces:**
- Consumes: everything Task 1 produces; `Hub::tell` (`crates/borant-hub/src/inbox.rs`), `Hub::task_event`, `Hub::agent`, `Hub::channel`, `self.cfg.roles`.
- Produces:
  - `ToolCall::ChannelCreate { name, schema, project, branch, #[serde(default)] kind: ChannelKind, #[serde(default)] title: Option<String>, #[serde(default)] issue: Option<String> }`. The reply is the stored `Channel`, so it names `gate` for a saga and `root` for an epic channel.
  - `TaskCreate` stores `NewTask.kind` and `NewTask.issue`. `TaskUpdate` sets `TaskPatch.issue`.
  - Error code `saga_not_approved` on `task_claim` in a saga before approval.
  - In `sagas.rs`: `pub(crate) async fn is_approver(&self, by: &Sender) -> Result<bool, ApiError>` (a person, or an agent whose role `approves`), and `pub(crate) async fn notify_approvers(&self, ch: &Channel, gate: &Task) -> Result<usize, ApiError>`.

Choices made here:
- **C4.** A gate close needs a person or an approver for every reason, not only `done`. A saga is rejected by the same people who approve it.
- **C5.** A saga or epic channel with no `title` takes its name as the title. The root epic of an epic channel gets that title.
- **C6.** The approver message is a direct message from the hub (sent as `Sender::Human`, like the deadline message at `crates/borant-hub/src/tasks.rs:520-534`). Text: `Saga #<channel> (<title>) waits for approval: <attention_msg>. Close task <channel>/<gate> as done to approve it, or as wontfix to reject it.` It goes once, on the change into `needs_human`. A channel with no collection messages approver agents with no collection.
- **C7.** `task_claim` lists the tasks of the channel for each claim, so `check_claim` can see the gate and the blockers. Channels hold tens of tasks.

- [ ] **Step 1: Write the failing tests in `crates/borant-hub/tests/sagas.rs`.** Build the fixture like `crates/borant-hub/tests/roles.rs:90-125`: roles `chief` (`approves = true`) and `orchestrator`, a project `web`, collection `work`, an orchestrator `boss`, a chief `chief`, and a worker `w1` in `work`, and an approver `other-chief` in collection `personal`.
  - `sagas_create_makes_the_gate`: `channel_create` with `kind: Saga` returns `gate`; the gate task has `GATE_TITLE`, status `open`, kind `task`, no parent.
  - `sagas_saga_and_epic_channels_need_a_project`.
  - `sagas_epic_channel_makes_the_root`: `root` is a task of kind `epic` with the channel title; a task created without a parent gets the root as parent; a second epic is refused.
  - `sagas_project_channel_refuses_an_epic`.
  - `sagas_tasks_hang_under_an_epic`: in a saga, a task with no parent, a task under the gate, and an epic with a parent are refused with `invalid`; a task under an epic and a grandchild pass; a `task_update` that moves a task under the gate is refused.
  - `sagas_gate_refuses_claim_and_ready`: a claim of an epic's child fails with code `saga_not_approved`, and `task_list(ready_only)` is empty. The spawn half of rule 2 is Task 3's test `epics_spawn_refuses_an_unapproved_saga`.
  - `sagas_owner_cannot_approve`: `boss` closes the gate as `done`: `denied`. `boss` closes it as `wontfix`: `denied`. A person closes it as `done`: ok, and then the child is ready and `w1` claims it.
  - `sagas_approver_role_approves`: `chief` closes the gate as `done`: ok.
  - `sagas_gate_needs_human_messages_the_approvers`: `boss` sets the gate to `needs_human` with an `attention_msg`. `chief`'s inbox has one direct message that names the channel and the gate. `other-chief` gets nothing. A second `task_update` of the message while it stays `needs_human` sends nothing new.
  - `sagas_epic_is_not_work`: a claim of an epic fails with `invalid`; `ready` never lists it.
  - `sagas_blocks_flow_down`: epic 2 `blocked_by` epic 1. After approval, the child of epic 2 is not ready and its claim fails with `conflict`. After epic 1's child and epic 1 close, it is ready and the claim passes.
  - `sagas_late_epic_needs_no_gate`: after approval, a new epic and its child: the child is ready.
  - `sagas_issue_is_stored`: `task_create` with `issue` and `task_update` with `issue`; `channel_create` with `issue` and `title`; `channel_list` returns them.
- [ ] **Step 2: Run** `just test-crate borant-hub sagas`. Expected: FAIL, the new `ChannelCreate` fields do not exist.
- [ ] **Step 3: Add the three fields to `ToolCall::ChannelCreate`, and fix every literal** (`kind: ChannelKind::Project, title: None, issue: None`). The match in `crates/borant-hub/src/calls.rs:181` passes them on.
- [ ] **Step 4: Extend `channel_create`.** Call `saga::check_channel`. Set `kind`, `title` (C5), `issue`. For a saga, pick `TaskId::new()` as `gate` and store the channel with it; for an epic channel, the same with `root`. Put the channel first (the put with `None` refuses a name that exists), then put the gate task (`GATE_TITLE`, kind `task`) or the root task (kind `epic`, the title, the channel's `issue`) directly with `self.store.put`, then join the owner. If the task put fails, delete the channel key and return the error. Send the usual `Created` task event for the new task.
- [ ] **Step 5: Wire the rules into `tasks.rs`.** `task_list`: use `saga::ready_in(&ch.value, &all)`. `task_create`: after the existing checks, `let parent = saga::check_create(&ch.value, &all, &new)?;` and store `parent`, `new.kind`, `new.issue`. `task_update`: before the parent change at line 191, call `saga::check_parent`; set `issue` from the patch; after the store, when `before.status != NeedsHuman`, the new status is `NeedsHuman`, and `ch.value.gate == Some(*id)`, call `notify_approvers`. A failed message logs a `warn` and does not fail the update. `task_claim`: list the tasks and call `saga::check_claim` before the other checks. `task_close`: when `ch.value.gate == Some(*id)`, replace the owner check with `is_approver` (C4); else keep it.
- [ ] **Step 6: Write `sagas.rs`** with `is_approver` (look up the agent's role and call `roles::approves` with `self.cfg.roles`) and `notify_approvers` (list `keys::AGENT_PREFIX`, keep agents that are not retired, whose `collection` equals the channel's, and whose role approves; `self.tell` each one).
- [ ] **Step 7: Run** `just test-crate borant-hub sagas`, `just test-crate borant-hub tasks`, `just test-crate borant-hub inbox`, `just test-crate borant-hub owner`, `just test-crate borant-host wait`. Expected: PASS.
- [ ] **Step 8: Commit** `feat(hub): saga and epic channels with the approval gate`.

### Task 3: Hub: channel close, spawn rules, and the saga scenario

Blocked by Task 2 (it uses `ChannelCreate` with `kind`, and it changes `tasks.rs` after Task 2).

**Files:**
- Modify: `crates/borant-core/src/proto.rs` (add `ToolCall::ChannelClose { name: ChannelName }` after `ChannelTransfer` at 350-353; add it to `a_human_may_stop_resume_inbox_and_answer` at 1022)
- Modify: `crates/borant-hub/src/calls.rs` (a `ChannelClose` arm after the `ChannelTransfer` arm at 217-239)
- Modify: `crates/borant-hub/src/sagas.rs` (`channel_close`, `close_channel`)
- Modify: `crates/borant-hub/src/tasks.rs` (`task_close`: the root epic closes its channel)
- Modify: `crates/borant-hub/src/spawns.rs` (the channel check 69-73, the binding 188-216, the repo branch 221-232)
- Modify: `crates/borant/src/mcp.rs` (tool `channel_close` after `channel_transfer` at 367-374; `lists_all_tools` at 508)
- Modify: `crates/borant/src/cli.rs` (`Command::Channel { cmd: ChannelCmd }` after `Command::Task` at 119-123; `ChannelCmd::Close { name: ChannelName }`; its handler after the `Command::Task` arm at 1248-1268; a parse test)
- Modify: `crates/borant-testkit/src/bin/borant-fake-harness.rs` (commands next to `CHANNEL` at 155-165)
- Modify: `tests/e2e/run.sh` (`ALL` at line 15; `scenario_saga` after `scenario_continue`, which ends at line 1564)
- Test: `crates/borant-hub/tests/epics.rs`

**Interfaces:**
- Consumes: Task 1 (`saga::check_spawn`, `saga::epic_branch`, `CoreError::ChannelClosed`), Task 2 (`ChannelCreate` with `kind`, `sagas.rs`).
- Produces:
  - `ToolCall::ChannelClose { name: ChannelName }`, allowed for a person. Reply: the stored `Channel` with `closed` set.
  - `pub(crate) async fn channel_close(&self, by: &Sender, mine: Option<&CollectionName>, name: &ChannelName) -> Result<Channel, ApiError>` and `pub(crate) async fn close_channel(&self, doc: Doc<Channel>) -> Result<Channel, ApiError>` in `sagas.rs`.
  - MCP tool `channel_close { name }`. CLI `borant channel close <name>`.
  - Fake harness commands: `SAGA <channel> <project>` prints `SAGA <channel> ok gate=<id>` or `SAGA <channel> failed`; `EPIC <channel> <title>` and `CHILD <channel> <parent> <title>` print `CREATED <id> <title>`.
  - e2e scenario `saga`.

Choices made here:
- **C8.** `channel_close` on a closed channel returns the channel unchanged (a retry is safe). A non-owner agent gets `denied`. Open tasks give `conflict` with their ids.
- **C9.** Rule 7 applies only when the bound task is in the spawn's channel, because the repo comes from that channel's project. A binding into another channel keeps the channel branch. The hub does not check that the branch is on `origin`; the skill says so.
- **C10.** A spawn bound to an open task that is blocked, in a saga or epic channel, is refused with `conflict` (`saga::check_spawn`). A worker would only fail its claim.
- **C11.** The scenario checks the gate across real hosts, not the board. The board has its own tests in Task 4.

- [ ] **Step 1: Write the failing tests in `crates/borant-hub/tests/epics.rs`** (fixture as in Task 2, plus two hosts with `Harness::Fake`):
  - `epics_channel_close_rules`: an agent that is not the owner gets `denied`; with an open task, `conflict`; after every task closes, the owner closes it; a second close returns it unchanged; a person may close.
  - `epics_closed_channel_refuses_work`: after the close, `task_create`, `task_claim`, and `spawn` fail with `conflict`.
  - `epics_root_close_closes_the_channel`: in an epic channel, close the children, then the root: the channel has `closed` set.
  - `epics_spawn_refuses_an_unapproved_saga`: unbound, bound to an existing child, and `TaskBinding::New` all fail before approval, and nothing is queued (`spawn_queue` is empty).
  - `epics_spawn_refuses_an_epic_binding`.
  - `epics_spawn_refuses_a_blocked_task` (C10).
  - `epics_spawn_starts_on_the_epic_branch`: after approval, set the epic's `branch` property to `epic/one`; a spawn bound to its child stores `repo.branch == "epic/one"` in the agent record (read `keys::agent`). An epic with no `branch`, and a plain task in a project channel, keep the channel branch.
  - In `crates/borant/src/cli.rs` tests: `channel_close_parses`.
  - Update `lists_all_tools` in `crates/borant/src/mcp.rs` with `channel_close`.
- [ ] **Step 2: Run** `just test-crate borant-hub epics` and `just test-crate borant channel_close`. Expected: FAIL.
- [ ] **Step 3: Add `ToolCall::ChannelClose` and `channel_close`.** Check the collection with `channel_for`, the caller with the owner-or-person rule of `crates/borant-hub/src/tasks.rs:631` (make it `pub(crate)`), refuse open tasks, set `closed = Some(self.now())` with the revision of the read.
- [ ] **Step 4: In `task_close`**, after the close, when `ch.value.kind == ChannelKind::Epic` and `ch.value.root == Some(*id)`, read the channel again and call `close_channel`.
- [ ] **Step 5: In `spawn`** (`crates/borant-hub/src/spawns.rs`): after the pause check at line 71, list the channel's tasks and call `saga::check_spawn(&ch, &tasks, None)`. In the `TaskBinding::Existing` arm, load the bound channel and its tasks and call `check_spawn` with the task. The `TaskBinding::New` arm already goes through `task_create` and its rules. At the repo branch (line 227), when the bound task is in `req.channel`, use `saga::epic_branch(&tasks, id)` before `ch.branch`.
- [ ] **Step 6: Add the MCP tool and the CLI command.** `borant channel close <name>` prints the reply with `pretty`.
- [ ] **Step 7: Run** `just test-crate borant-hub epics`, `just test-crate borant-hub spawns`, `just test-crate borant-hub projects`, `just test-crate borant channel_close`, `just test-crate borant lists_all_tools`. Expected: PASS.
- [ ] **Step 8: Add the fake harness commands and `scenario_saga`.** Add `saga` to `ALL` after `continue`. The scenario: `project add sagatest git://git/demo.git --branch main`; `start_agent host-a sboss` and `start_agent host-b sw1`; `sboss` runs `SAGA sagatest sagatest`, `EPIC sagatest e1`, `CHILD sagatest <e1> c1`; assert `borant tasks sagatest --ready` is `[]`; `sw1` runs `CLAIM sagatest <c1>` and logs `CLAIM <c1> failed`; a person runs `borant task close sagatest/<gate> --reason done --message approved` on host-c; wait until `tasks sagatest --ready` lists `c1`; `sw1` claims again and logs `CLAIM <c1> ok`. End with `borant cancel sboss` and `borant cancel sw1`. `affected()` needs no change: core and hub paths already run every scenario.
- [ ] **Step 9: Run** `just e2e saga`. Expected: `PASS saga`. Put the output in the PR.
- [ ] **Step 10: Commit** `feat(hub): channel_close, epic branches for spawns, and the saga scenario`.

### Task 4: The board

Blocked by Task 1. Runs in parallel with Tasks 2 and 3.

**Files:**
- Create: `crates/borant-core/src/board.rs`; Modify: `crates/borant-core/src/lib.rs` (add `pub mod board;`)
- Modify: `crates/borant-core/src/proto.rs` (add `ToolCall::Board { #[serde(default)] collection: Option<CollectionName> }` after `View` at 535-536)
- Create: `crates/borant-hub/src/board.rs`; Modify: `crates/borant-hub/src/lib.rs` (add `mod board;` after `mod alerts;` at line 8; route the call in `handle` next to the `View` arm at 309-331)
- Modify: `crates/borant-hub/src/calls.rs` (a `Board` arm next to the `View` arm at 480-484 that returns `internal`, as `View` does)
- Modify: `crates/borant-hub/src/http.rs` (route `/api/board` in `router` at 43-56, and its handler) and `crates/borant-hub/src/http/tests.rs` (a route test)
- Modify: `crates/borant/src/mcp.rs` (tool `board`; `lists_all_tools` at 508)
- Modify: `crates/borant/src/cli.rs` (`Command::Board` after `Command::Inbox` at 229-235; its handler after the `Inbox` arm at 1475-1482; `fn render_board(v: &BoardView) -> String`; a test)
- Test: `crates/borant-hub/tests/board.rs`

**Interfaces:**
- Consumes: Task 1 (`ChannelKind`, `TaskKind`, `Channel.gate/root/title/issue`, `saga::ready_in`, `saga::descendants`, `saga::epic_status`, `roles::mirror_issues`); `Hub::view` (`crates/borant-hub/src/view.rs:19`), `Hub::all_tasks` (`crates/borant-hub/src/alerts.rs:219`), `calls::allowed` (`crates/borant-hub/src/calls.rs:27`), `Hub::authorize` (`crates/borant-hub/src/calls.rs:122`).
- Produces, in `borant_core::board` (all `Debug, Clone, PartialEq, Serialize, Deserialize`):
  - `BoardView { at: Timestamp, collections: Vec<CollectionBoard> }`
  - `CollectionBoard { collection: Option<CollectionName>, projects: Vec<ProjectBoard>, needs_you: Vec<Question> }`
  - `ProjectBoard { project: ProjectName, mirror_issues: bool, orchestrators: Vec<AgentLine>, sagas: Vec<SagaLine>, epics: Vec<EpicLine>, tasks: Counts }`
  - `AgentLine { agent: AgentId, online: bool, busy: bool, host: Option<HostName> }`
  - `SagaState { Proposed, Approved, Closed }` (snake_case)
  - `SagaLine { channel: ChannelName, title: Option<String>, issue: Option<String>, state: SagaState, gate: Option<TaskId>, epics: Vec<EpicLine>, done: u32, total: u32 }`
  - `EpicLine { channel: ChannelName, task: TaskId, title: String, status: TaskStatus, issue: Option<String>, branch: Option<String>, pr_url: Option<String>, counts: Counts }`
  - `Counts { open: u32, ready: u32, running: u32, blocked: u32, stuck: u32, needs_human: u32, done: u32 }` (`Default`)
  - `Question { project: Option<ProjectName>, channel: ChannelName, task: TaskId, title: String, status: TaskStatus, attention_msg: Option<String>, since: Timestamp, gate: bool }`
  - `pub struct BoardInput<'a> { pub at: Timestamp, pub projects: &'a [Project], pub channels: &'a [Channel], pub tasks: &'a [(ChannelName, Task)], pub agents: &'a [AgentStatus], pub collections: &'a BTreeMap<CollectionName, CollectionConfig> }`
  - `pub fn build(input: &BoardInput, wanted: &[Option<CollectionName>]) -> BoardView`
  - `pub fn counts(channel: &Channel, tasks: &[Task], scope: &[&Task]) -> Counts`
  - In the hub: `pub(crate) async fn board(&self, mine: Option<&CollectionName>, collection: Option<CollectionName>) -> Result<BoardView, ApiError>`.
  - `ToolCall::Board { collection }` (allowed for a person), MCP tool `board { collection? }`, CLI `borant board [--collection C] [--json]`, HTTP `GET /api/board?collection=C`.

Choices made here:
- **C12.** The board types and the pure `build` live in `borant-core` (`board.rs`), like `SwarmView` in `model.rs`, so the CLI can deserialize and render them. The hub's `board.rs` only reads records and calls `build`.
- **C13.** Counts never include an epic or a gate. `open` counts every `open` task, `ready` the open tasks in `ready_in`, `blocked` is `open - ready`, `running` counts `in_progress`, `done` counts `closed` as `done`. An `EpicLine`'s counts cover its descendants. `ProjectBoard.tasks` covers the tasks of the project's channels of kind `project`.
- **C14.** `SagaState`: `Closed` when the channel is closed or the gate closed with a reason other than `done` (a rejected saga), `Approved` when `saga::approved`, else `Proposed`. Order: approved, proposed, closed; then by `created`. Epics inside a saga and standalone epic channels: open first, then by `created`. Questions: oldest `since` first, as `borant inbox` does.
- **C15.** `SagaLine` also carries `gate`, so the mod can close it. `done` counts epics closed as `done`; `total` counts epics not closed as `wontfix`.
- **C16.** Without `collection`, a person gets every collection in `hub.toml`, on a project, or on a channel, plus one board with `collection: None` when a project has none. An agent gets only its own collection; naming another one is `denied`. HTTP serves the person's view, like `/api/snapshot`.
- **C17.** `orchestrators` are the owners of the project's open channels, without duplicates, with `online` from the lease state `held` and `busy` and `host` from `Hub::view`.
- **C18.** The text of `borant board`: one block per collection, one line per project (`project  mirror=on|off  orchestrators: a (online), ..`), one indented line per saga (`saga #c  Title  approved  3/5 epics`) and per epic (`epic Title  in_progress  2 ready, 1 running, 4 done`), the plain-task counts, then "Needs you:" with one line per question (`[gate] c/id title: attention_msg (age)`).

To avoid conflicts with Tasks 2 and 3, which run at the same time:
- `proto.rs`: Task 2 edits `ChannelCreate` (332-342), Task 3 adds `ChannelClose` after `ChannelTransfer`, this task adds `Board` after `View`. Separate hunks.
- `calls.rs`: Task 2 edits the `ChannelCreate` arm and `channel_create`, Task 3 adds an arm after `ChannelTransfer`, this task adds an arm after `View`. Separate hunks.
- `lib.rs`: Task 2 adds `mod sagas;`, this task adds `mod board;` (line 8). Separate hunks.
- `mcp.rs`: Task 3 and this task both add a line to the `lists_all_tools` array (`board` first, `channel_close` before `channel_create`). The second PR to merge resolves a two-line conflict.
- `cli.rs`: Task 3 adds `Command::Channel` after `Command::Task`, this task adds `Command::Board` after `Command::Inbox`. Both append a test at the end of `mod tests`; the second PR resolves it.
- Tests: `crates/borant-hub/tests/board.rs` seeds every channel, task, and project with `fx.state.seed(key, bytes)` (`crates/borant-memory/src/state.rs:62`), never with `ToolCall::ChannelCreate`, so Task 2's new fields do not break it. Agents come from `fx.register_in`.

- [ ] **Step 1: Write the failing tests.**
  - In `crates/borant-core/src/board.rs` tests: `board_counts_skip_epics_and_the_gate`, `board_counts_blocked_is_open_minus_ready`, `board_saga_state_and_order`, `board_saga_done_and_total` (one epic done, one wontfix, one open: `1/2`), `board_epic_line_reads_branch_and_pr_url`, `board_questions_mark_the_gate`, `board_mirror_issues_is_resolved`.
  - In `crates/borant-hub/tests/board.rs`: `board_shows_a_collection` (a project with one saga of two epics, one standalone epic channel, one project channel with plain tasks, an orchestrator online; assert the whole `BoardView`), `board_without_a_collection_lists_every_collection`, `board_agent_sees_only_its_collection` (no collection: only its own; another one: `denied`), `board_writes_nothing` (key set and revisions equal before and after, as in `crates/borant-hub/tests/prune.rs`).
  - In `crates/borant-hub/src/http/tests.rs`: `board_route_serves_the_board` and `board_route_filters_by_collection`.
  - In `crates/borant/src/cli.rs` tests: `board_parses_its_flags` and `board_renders_projects_sagas_and_questions` (a fixed `BoardView`, the exact text).
  - Update `lists_all_tools` in `crates/borant/src/mcp.rs` with `board`.
- [ ] **Step 2: Run** `just test-crate borant-core board`, `just test-crate borant-hub board`, `just test-crate borant board`. Expected: FAIL.
- [ ] **Step 3: Write `borant_core::board`.**
- [ ] **Step 4: Write `Hub::board`.** Read the projects (`keys::PROJECT_PREFIX`), the channels (as `ChannelList` does at `crates/borant-hub/src/calls.rs:205-215`), `all_tasks`, and `view().agents`, then call `build`. In `Hub::handle`, route `CtlRequest::Call { actor, key: None, call: ToolCall::Board { collection } }` before the mutex: `authorize`, the caller's collection from its agent record, the C16 check, then `board`.
- [ ] **Step 5: Add the HTTP route, the MCP tool, and the CLI command.** The CLI prints JSON with `--json`, else `render_board`.
- [ ] **Step 6: Run** the commands of step 2, then `just test-crate borant-hub http` and `just test-crate borant lists_all_tools`. Expected: PASS.
- [ ] **Step 7: Commit** `feat: the board of sagas, epics, and questions (MCP, CLI, HTTP)`.

### Task 5: Agent surface, skills, and the guide

Blocked by Tasks 3 and 4.

**Files:**
- Modify: `crates/borant/src/mcp.rs` (params `ChannelCreate` 62-71, `TaskCreate` 95-105, `TaskUpdate` 119-131; tool bodies `channel_create` 337-345, `task_create` 389-397, `task_update` 413-425)
- Modify: `integrations/pi/borant.ts` (`channel_create` 398-407, `task_create` 427-439, `task_update` 449-463; new tools `channel_close` and `board` after `channel_transfer` and `project_get`)
- Modify: `integrations/pi/README.md` (line 5, the tool list)
- Modify: `integrations/claude-code/skills/orchestrate/SKILL.md` (facts table 13-29, "Set up" 31-41, "Event loop" 143-166, "Red flags" 180-192; new section "Sagas and epics" and "Epic stacks")
- Modify: `integrations/claude-code/skills/chief-of-staff/SKILL.md` (facts table 17-32, "Delegate" 34-49, "Event loop" 85-106; new section "Size the goal and approve sagas")
- Modify: `docs/guide.md` ("Run the hub" 25-39, "Work with channels and tasks" 162-206, "Commands" 320-348)

**Interfaces:**
- Consumes: `ChannelCreate { kind, title, issue }`, `NewTask { kind, issue }`, `TaskPatch { issue }` (Task 2), `channel_close` (Task 3), `board` (Task 4).
- Produces: MCP params `channel_create { kind?, title?, issue? }`, `task_create { kind?, issue? }`, `task_update { issue? }`; the same in the Pi extension, plus Pi tools `channel_close { name }` and `board { collection? }`. No CLI flag is needed for these, because a person cannot call `channel_create` or `task_create` today (`crates/borant-core/src/proto.rs:561-570` allows only agents to create channels, and there is no `borant task create`).

Choices made here:
- **C19.** The kind is a string in MCP and Pi (`"saga"`, `"epic"`, `"project"`, and `"epic"`, `"task"`), checked by the hub's serde, as `status` is today.
- **C20.** No skill exists for Pi or omp (`integrations/pi/` has none), so only the two Claude Code skills change.

- [ ] **Step 1: Write the failing tests.** In `crates/borant/src/mcp.rs` tests: `channel_create_passes_kind_title_and_issue` and `task_create_passes_kind_and_issue`. Each builds the JSON the tool forwards and parses it as `ToolCall`, as `wait_passes_the_states_and_the_timeout` does. To test it, move the JSON of each tool into a free function (`fn channel_create_call(a: &ChannelCreate) -> Value`), as `spawn_request` already is.
- [ ] **Step 2: Run** `just test-crate borant mcp`. Expected: FAIL.
- [ ] **Step 3: Add the MCP params and the Pi params and tools.**
- [ ] **Step 4: Run** `just test-crate borant mcp` and `just test-pi`. Expected: PASS.
- [ ] **Step 5: Update the orchestrate skill** per the spec sections "Skills" and "Epic stacks":
  - Facts: a saga holds every claim and spawn until the gate closes as `done` (`saga_not_approved`); an epic is never claimed; a child waits for its epic's `blocked_by`; a spawn bound to an epic's task starts on the epic's `branch`; only you run `gh stack`, in your own clone, and the host needs the `gh-stack` extension.
  - Set up: size each goal (plain task in the project channel, a standalone epic with `kind: "epic"`, or a saga with `kind: "saga"`, a title, and the issue link).
  - Saga: create the epics with a scope body each, set the gate to `needs_human` with an `attention_msg` that states the scope and lists the epics, end the turn. After the approval, create every epic's tasks with `blocked_by` between epics for order, and spawn for the ready tasks.
  - Epic stacks: the seven steps of the spec, short.
  - Close: when the last epic of a saga closes, `channel_close`. A gate closed as `wontfix`: close the epics as `wontfix`, then `channel_close`.
  - Red flags: "Spawn before the approval" → "The hub refuses it. Set the gate to needs_human and end the turn."; "Close the gate myself" → "Only a person or the chief approves.".
- [ ] **Step 6: Update the chief-of-staff skill:** map a new goal to a project and a size and tell the orchestrator; a saga gate is the main `needs_human` case (the hub sends you a direct message); show the person the title, the scope, the epics, and a suggested answer; on yes, `task_close(reason: "done", message: <the person's words>)` on the gate; on no, `reason: "wontfix"`; answer status questions from `board`. Add `board` to the tools the chief calls directly.
- [ ] **Step 7: Update `docs/guide.md`:** a "Sagas and epics" subsection under "Work with channels and tasks" (the three channel kinds, the gate, who approves, blocks that flow down, the epic branch, `channel_close`, the board); `approves` and `mirror_issues` in the `hub.toml` example of "Run the hub"; rows for `borant board [--collection C] [--json]`, `borant channel close <name>`, and `--mirror-issues true|false` on `borant project add` in "Commands".
- [ ] **Step 8: Commit** `docs: sagas and epics in the skills and the guide; MCP and Pi fields`.
