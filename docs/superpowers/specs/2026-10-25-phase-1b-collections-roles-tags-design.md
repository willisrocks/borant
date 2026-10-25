# Phase 1b: collections, roles, and host tags

This spec builds roadmap phase 1b from Chris's decisions (roadmap, "Phase 1b").
Borant stays one swarm. A collection groups projects and owns an identity. A
role on each agent sets its harness, model, placement, tools, and skill. Host
tags drive placement. It also takes over the parts that phase 1 cut: bundles
beyond the harness defaults, and the scheduler filter on bundles.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Goals

1. Two collections, `personal` and `work`, each with its secret bundles, its
   git author, and its channel prefix. The hub keeps agents inside their
   collection for spawns, channels, joins, and task calls.
2. Four roles as configuration, not code: chief, orchestrator, worker, and
   helper. Each sets the harness and model pair, placement, which roles it
   may spawn, and the skill its brief loads.
3. Host tags with `require`, `prefer`, and `avoid` rules for each role.
4. `chief-personal` and `chief-work` run on the laptop. Orchestrators and
   workers run on the boxes.
5. A `borant:chief-of-staff` skill in the Claude Code plugin.

## Cuts

- A cross-collection check on `send`. A direct message does not go through
  the hub, and the host daemon has no state store to look up the target.
  Joins are guarded, so broadcasts and task events stay inside a
  collection. Chris asked for no stronger isolation now.
- Profiles as a separate concept. Roles replace the profiles of the
  orchestration doc.
- Reject reasons in `borant ps`, and resources in the heartbeat (roadmap
  phase 5).
- Hiding MCP tools per role. The hub refuses a call that the role does not
  allow, which is enough.
- A Docker e2e scenario with two collections. Hub tests cover the guards.
- Bundles on a spawn call and on a channel. Bundles come from the collection
  and the role only.

## Constraints

- Strict ports and adapters. No core constant names a role, a tag, or a
  collection. `laptop`, `vps`, `chief`, and `worker` exist only in `hub.toml`
  and `host.toml`.
- Records with no collection or no role (everything that exists today) keep
  today's behavior. A spawn from an agent with no role may spawn any role.
- All new fields are `#[serde(default)]`. New protocol variants need a
  release on every host before the hub sends them.
- Channel names allow `[a-z0-9_-]`, at most 63 characters (`ids.rs`).

## Interfaces

### Names and records

- `RoleName` and `CollectionName`: new `name_type!` names in `ids.rs`. `Tag`
  exists (phase 4).
- `Agent.role: Option<RoleName>`, `Agent.collection: Option<CollectionName>`.
- `Project.collection: Option<CollectionName>`.
- `Channel.collection: Option<CollectionName>`: from its project, else from
  the agent that created it.
- `HostInfo.bundles: Vec<BundleName>`: every bundle that the host defines.
- `ToolCall::Spawn` gets `role: Option<RoleName>`.
- `ToolCall::ProjectAdd` gets the project's `collection` through
  `Project.collection`. The CLI gets `borant project add <name> ...
  --collection <c>`.
- `LaunchOrder` gets `bundles: Vec<BundleName>` (the bundles of the
  collection and the role, in that order) and
  `git_author: Option<GitAuthor>` with `GitAuthor { name, email }`.
- `LocalRegister` and `RegisterSession` get `role` and `collection`, read by
  the hook from `BORANT_ROLE` and `BORANT_COLLECTION` (and by the Pi and omp
  extension), so a chief that a person starts gets its role.

### `hub.toml`

```toml
[collections.personal]
bundles = ["github-personal"]
git_author = { name = "Chris Fenton", email = "you@example.com" }

[collections.work]
bundles = ["github-work"]
git_author = { name = "Chris Fenton", email = "you@work.example" }

[roles.chief]
harness = "claude-code"
model = "opus"
prefer = ["laptop"]
pinned = true
one_per = "collection"
spawns = ["orchestrator", "helper"]
skill = "borant:chief-of-staff"

[roles.orchestrator]
harness = "claude-code"
require = ["vps"]
one_per = "project"
spawns = ["worker", "helper"]
skill = "borant:orchestrate"

[roles.worker]
harness = "omp"
model = "lunaroute/deepseek-4.1-flash:auto"
require = ["vps"]
avoid = ["laptop"]
spawns = []

[roles.helper]
on_parent_host = true
spawns = []
```

`RoleConfig { harness, model, require, prefer, avoid, pinned, one_per, spawns, skill, on_parent_host, bundles }`
and `CollectionConfig { bundles, git_author }` live in
`borant-core::rules::roles`, with `Deserialize`. All fields are optional
except `spawns`, which defaults to empty.

### Rules (`borant-core::rules::roles`)

- `pub fn resolve(spec: &SpawnInput, parent: Option<&Agent>, roles: &BTreeMap<RoleName, RoleConfig>) -> Result<Resolved, RoleError>`:
  the role of the child (the `role` of the call), its harness and model
  (explicit call values win over the role), its placement rule, its
  collection (the parent's), and whether it is pinned.
- `pub fn may_spawn(parent_role: Option<&RoleName>, child: &RoleName, roles: ...) -> bool`:
  true when the parent has no role, or when the child is in `spawns` of the
  parent role.
- `pub fn brief(prompt: &str, skill: Option<&str>) -> String`: puts the line
  "First load the <skill> skill." before the prompt when the role names a
  skill.

## Placement with tags

`schedule.rs` gets a `PlacementReq { host: Option<HostName>, require: Vec<Tag>, prefer: Vec<Tag>, avoid: Vec<Tag> }`.

1. A host qualifies only if it has every `require` tag, and every bundle of
   the launch (`HostInfo.bundles`).
2. The sort key, in order: the same harness; fewer `avoid` tags; more
   `prefer` tags; then the fill-first order of phase 4 (static before pool,
   busier pool hosts first, then today's order for static hosts).
3. A helper (`on_parent_host`) gets `host = parent's host`.

`avoid` is a soft rule, and `require` a hard one (see the decision records).

## Collections

The hub keeps each agent inside its collection:

1. Spawn: the child gets the parent's collection. The channel of the spawn
   must be in the same collection, or have none.
2. Channel create: in a collection, the name must start with
   `<collection>-`. The hub refuses another name with `invalid`. A channel of
   a project gets the project's collection, which must match the creator's.
3. Join and the auto-join at registration: the channel must be in the
   agent's collection, or have none.
4. Task calls, `members`, `schema_*`, and `channel_transfer`: the channel must
   be in the caller's collection, or have none.
5. `channel_list`, `agents`, and `project_list` show only the caller's
   collection plus the records with none. A person sees everything.
6. Launch: `LaunchOrder.bundles` and `git_author` come from the collection
   (and the role's `bundles`). The host sets `GIT_AUTHOR_NAME`,
   `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME`, and `GIT_COMMITTER_EMAIL`, and
   resolves the harness defaults first, then `LaunchOrder.bundles`. A later
   bundle wins on the same var, and the host logs both bundle names.

## Roles

1. `one_per = "collection"` or `"project"`: a spawn of that role fails with
   `conflict` while another agent of the role holds a lease, or is pinned,
   in the same collection or project.
2. `pinned = true` sets `Agent.pinned` (the phase 0 pinned-agents feature).
3. The hub refuses a spawn when `may_spawn` is false, with `denied`.
4. A role names its skill in the brief with `brief`.

## The chief of staff

`integrations/claude-code/skills/chief-of-staff/SKILL.md`: the role of a chief
for one collection. It owns the projects of the collection, spawns one
orchestrator for each project with a brief, answers their `needs_human`
questions or brings them to Chris, and watches the swarm. It does not merge or
build. The `orchestrate` skill gets a short section on roles and tags: a
worker spawn uses `role: "worker"`, and the role sets the harness and model.

## Tasks

| Task | Who | Blocked by | Files |
|---|---|---|---|
| Core interfaces for roles and collections | worker | none | `crates/borant-core` (ids, model, proto, `rules/roles.rs` types and signatures) |
| Placement with tags | worker | Core interfaces | `crates/borant-core/src/rules/schedule.rs`, `crates/borant-hub/src/moves.rs` (`host_views`, the call) |
| Roles in the hub | worker | Core interfaces | `rules/roles.rs` bodies, `crates/borant/src/config.rs` (`HubFile.roles`), `crates/borant-hub/src/{moves.rs,calls.rs,leases.rs}` (spawn, authorize, register) |
| Collection guards in the hub | worker | Core interfaces | `crates/borant-hub/src/calls.rs` (create, join, task calls, lists), `config.rs` (`HubFile.collections`), CLI `--collection` |
| Host side of collections and roles | worker | Core interfaces | `crates/borant-host/src/commands.rs` (git author, bundle order), `crates/borant/src/hook.rs`, `integrations/pi/borant.ts` |
| Chief of staff skill | worker | none | `integrations/claude-code/skills/` |
| Phase 1b release, deploy, and the two chiefs | orchestrator | every code task | `borant-infra` (`hub.toml`, `host.toml` tags) |

Each code task: one test for each rule it owns, run with
`just test-crate <crate> <filter>`, then `just fmt` and `just lint`.

## Questions for Chris

1. The git author emails for `personal` and `work`, and which GitHub account
   each collection uses. Default: `personal` uses `willisrocks` and the
   noreply address of that account; `work` waits for your answer.
2. Starting the two chiefs as Orca tabs with `BORANT_ROLE=chief` and
   `BORANT_COLLECTION=<c>`: you start them, or the orchestrator does through
   Orca. Default: the orchestrator writes the exact commands in a
   `needs_human` task.

## Decision records

Decision: roles live in `hub.toml` as configuration, and core holds no role,
tag, or collection name. Evidence: the strict ports rule bans `vps` or
`laptop` assumptions in core. Reversible: yes. Friend: see below. Blocker:
none.

Decision: records with no collection or role keep today's behavior.
Evidence: every agent, channel, and project today has none, and the swarm
must keep working during the rollout. Reversible: yes. Friend: see below.
Blocker: none.
