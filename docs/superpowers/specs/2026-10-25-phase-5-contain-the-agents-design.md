# Phase 5: contain the agents

Roadmap phase 5 has three items: host resources and reject reasons, a
bubblewrap sandbox, and the herdr launcher. This spec builds them from
sections 5 to 7 of the
[orchestration design](2026-10-24-orchestration-api-and-sandboxes-design.md)
("the orchestration doc"), adapted to the code of v0.7.0: host tags and roles
(phase 1b) replace labels and taints, and the persisted spawn queue (phase 3)
holds the reject reasons.

The orchestrator wrote it in auto-pilot from two research tasks on the boxes:
[herdr facts](../../research/2026-10-25-herdr-facts.md) (branch
`research/p5-herdr`) and [bubblewrap facts](../../research/2026-10-25-bwrap-facts.md)
(branch `research/p5-bwrap`). The decision records are at the end.

## Goals

1. When no host takes a spawn, `borant ps --queued` and the
   `placement.refused` event name the reason for each host, for example
   "borant-1: full (6/6), borant-2: low memory (700 MB free, 1024 reserved)".
2. A host with too little free memory takes no new agent.
3. A worker can run in a bubblewrap sandbox that hides the NATS credentials,
   `~/.ssh`, the other agents' clones, and the tmux socket. The role in
   `hub.toml` says which agents run sandboxed.
4. A VPS host can run its agents in herdr instead of tmux, so that a person
   attaches once and sees every agent of the host with its state.

## Cuts

- `BwrapNoNet`, the egress proxy, and `borant sandbox-shim` (the second
  sandbox step of the orchestration doc). See the decision record "No network
  sandbox in phase 5".
- Channel spread in the sort key. One more sort term changes every placement
  test, and no incident asked for it.
- `borant run graph.json` (orchestration doc phase 5). Phase 6 builds durable
  work.
- herdr as the default launcher. tmux stays the default; herdr is a host
  option.
- macOS sandboxes. The laptop takes only pinned agents and owners.

## Constraints

- Strict ports and adapters. The sandbox is a pure argv function in
  `borant-launch`, not a port: it names no vendor and has one implementation.
  herdr is a `Launcher` adapter in `borant-launch`. No core type names herdr or
  bwrap: the core has `SandboxKind::Namespace` (a kind, not a product).
- New fields are `#[serde(default)]`, so a v0.7.0 host and a v0.8.0 hub work
  together during a deploy.
- `place` stays a pure function in `borant-core::rules::schedule` with exact
  tests.
- Hosts restart one at a time, and a deploy must not stop a running session.

## Interfaces

### Resources and reject reasons (`borant-core`)

- `HostStats` gets `mem_avail_mb: Option<u64>` and `disk_avail_mb:
  Option<u64>` (the disk of `BORANT_HOME`). The host fills them from
  `sysinfo` in `stats()` (`crates/borant-host/src/lib.rs`).
- `HostInfo` gets `reserve_mem_mb: Option<u32>` from `[host] reserve_mem_mb`
  in `host.toml` (default 1024 when unset), and `sandboxes:
  Vec<SandboxKind>`.
- `HostView` gets `mem_avail_mb: Option<u64>`. `views()` in
  `crates/borant-hub/src/moves.rs` copies it from `mem.host_stats`.
- `pub enum Reject { Offline, Draining, Full { active: u32, max: u32 },
  NoRepo, NoHarness, MissingTag(Tag), MissingBundle(BundleName), LowMemory {
  avail_mb: u64, reserve_mb: u32 }, NoSandbox, RecentlyFailed }` with
  `Display` ("full (6/6)", "low memory (700 MB free, 1024 reserved)").
- `place(...) -> Result<(HostName, Harness), Vec<(HostName, Reject)>>`. The
  error lists every host with its first failed rule. A host with no
  `mem_avail_mb` passes the memory rule, so a v0.7.0 host keeps working.
  `PlacementReq` gets `sandbox: Option<SandboxKind>`.
- `refused_hosts()` in `moves.rs` goes away: the `placement.refused` event and
  `SpawnRecord.last_reject` take the list from `place`. `last_reject` becomes
  `Vec<(HostName, String)>` (serde default; the old `Option<String>` field
  name stays readable with an alias or a custom default).

### Sandbox

- `pub enum SandboxKind { Namespace }` in `borant-core::model` (serde
  `"namespace"`). `RoleConfig` gets `sandbox: Option<SandboxKind>`;
  `roles::Resolved` and `LaunchOrder` carry it.
- `host.toml`:

  ```toml
  [sandbox]
  enabled = true            # the host offers SandboxKind::Namespace
  program = "bwrap"         # the program on PATH
  ro = ["~/.config/gh"]     # extra read-only binds
  rw = []                   # extra writable binds
  ```

  At start, the host runs `<program> --ro-bind / / true`. If it fails, the host
  logs a warn and offers no sandbox.
- `borant-launch/src/sandbox.rs`: `pub fn wrap(plan: &SandboxPlan, program:
  Vec<String>) -> Vec<String>`, a pure function, with `SandboxPlan { program,
  home, work_dir, harness_dirs, sockets, secrets_dir, ro, rw, cwd }`. The argv
  follows section 6 of the orchestration doc, with the binds that the bwrap
  research found (see that file). It never has `--unshare-pid`, because the
  host tests liveness by the harness PID.
- `crates/borant-host/src/commands.rs`: the launch wraps outside the secret
  shim: `bwrap ... -- borant secret-exec <dir> -- <harness argv>`. The secrets
  directory is bound read-write (the shim deletes its `env` file), and only
  that agent's directory.
- The harness directories per harness live in one table in
  `borant-host` (Claude Code: `~/.claude`, `~/.claude.json`; omp: `~/.omp`,
  plus what the research found).

### herdr launcher (`borant-launch/src/herdr.rs`)

- `launcher = "herdr"` in `host.toml`, with `[herdr] session = "borant"` and
  `program = "herdr"`. One herdr session per host, which the host daemon
  starts as `setsid herdr --session <s> server` when a `workspace list` call
  answers `server_not_running`, with every `BORANT_*` variable removed from
  the server environment (panes inherit it). The server outlives a restart of
  the host daemon, so running sessions survive a deploy.
- `launch`: `herdr --session <s> workspace create --cwd <cwd> --label
  <name> --env K=V ... --no-focus`, then `herdr --session <s> pane run <pane>
  <argv...>`. `pane run` takes the full argv, so the sandbox and the secret
  shim work unchanged. The handle is the workspace ID plus the pane ID.
- `stop`: `workspace close <workspace>`. `is_running`: the pane exists and its
  process runs. `capture`: `pane read <pane> --source recent`, cut to the last `n` lines
  by the adapter.
  `attach_command`: `herdr --remote <ssh> --session <s>`.
- Names: `b-` plus the agent ID, cut to 32 characters with a 6-character hash
  suffix when longer, lowercase, `[a-z][a-z0-9_-]{0,31}`.
- Agent states (`idle`, `working`, `blocked`) are out of this phase: they need
  the herdr integration of each harness. The launcher reads only liveness and
  the screen.
- borant-infra packages the herdr 0.9.5 release binary (`pkgs/herdr.nix`,
  pinned hash), because the boxes' nixpkgs has no `herdr`.

## Rules

- A host qualifies when the current rules pass and: its `mem_avail_mb` is at
  or above `reserve_mem_mb` (or unknown), and it offers the sandbox kind of
  the request (or the request has none). The first failed rule is the reject
  reason of the host.
- The sort key does not change.
- A role with `sandbox = "namespace"` places its agents only on hosts that
  offer it. A spawn with no role has no sandbox.

## Tasks

| Task | Owner | Blocked by | Files |
|---|---|---|---|
| Core interfaces | worker | none | `borant-core` model, proto, rules/schedule (`Reject`, `place` result, `SandboxKind`, `RoleConfig.sandbox`, `HostStats` fields), call sites compile with the old behavior |
| Resources and reject reasons | worker | core | host `stats()`, hub `views()`, `moves.rs` (`placement.refused` from `place`), `spawns.rs` (`last_reject`), `borant ps --queued` and `top` show the reasons |
| Sandbox wrap | worker | core | `borant-launch/src/sandbox.rs`, host `[sandbox]` config and start check, `commands.rs` wrap, the harness dir table |
| herdr launcher | worker | core | `borant-launch/src/herdr.rs`, `launcher_for`, host `[herdr]` config |
| Infra: herdr package, sandbox on borant-2, worker role | orchestrator | sandbox, herdr, release | `borant-infra` |
| Docker e2e: a sandboxed fake agent cannot read the host credentials | worker | sandbox | `tests/e2e`, `docker/` |

Each code task proves itself with a focused test: `just test-crate
borant-core schedule` (each reject reason, the memory rule with and without
stats, the sandbox rule), `just test-crate borant-launch sandbox` (the exact
argv for Claude Code and omp, no `--unshare-pid`, the secrets dir bound),
`just test-crate borant-launch herdr` (the command lines against a fake
`herdr` script on PATH, name shortening), `just test-crate borant-host
sandbox` (a failed start check offers no sandbox).

## Decision records

### No network sandbox in phase 5

Decision: build the file-system sandbox (`Namespace`) now, and move
`BwrapNoNet` with the proxy and the shim to a later phase.
Evidence: see the bubblewrap research, question 5 (proxy honoring of omp,
Claude Code, and git).
Reversible: yes. `SandboxKind` gets a second variant later.

### A pure wrap function, not a port

Decision: the sandbox is `wrap()` in `borant-launch`, not a `Sandbox` port.
Evidence: one implementation, no I/O, and the argv must be exact in tests. A
port adds a contract suite for a pure function. The core names only the kind.
Reversible: yes.

### `pane run`, not `agent start`, for herdr

Decision: the herdr launcher starts the program with `pane run <pane>
<argv>`.
Evidence: herdr research question 3: `agent start` runs the canonical
program of a kind and needs a pristine shell, so it cannot run the sandbox or
the secret shim in front of the harness. `pane run` takes any argv.
Reversible: yes. Agent states can come later through the integrations.
