# Phase 1: zero-touch hosts

This spec builds roadmap phase 1. A new box comes up with every harness login
and no human step, and no secret value shows in `ps`, a session log, or NATS.
It also builds token pools as the
[token pools design](2026-10-25-token-pools-design.md) decides. The detailed
design is in the
[secrets and host bootstrap design](2026-10-24-secrets-and-host-bootstrap-design.md)
("the secrets doc"). This spec says which parts of it ship now, and settles
the conflicts between the documents.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Scope

The roadmap says "secrets document phases 0 to 2". The numbering of the
secrets doc does not match. Phase 1 of the roadmap is:

| Secrets doc part | In phase 1 | Notes |
|---|---|---|
| Phase 0, fact checks | Yes | Owner and Chris tasks |
| Phase 1, the port, adapters, bundles, resolver mode | Yes, with the cuts below | Code in `borant` |
| Phase 2, wrapper mode | No | Cut until a backend needs it |
| Phase 3, the 1Password plugin, vault, seed unit | Yes | Code in `borant-infra` |
| Phase 5, the golden image | No | Roadmap phase 4 |
| Token pools phases 0 to 2, with the Decisions | Yes | Named "bundle members" here |

## Goals

1. `just create` and `just install` of a new box give a host that registers,
   and launches a Claude Code worker and an omp worker with no login.
2. No secret value shows in `ps`, in a session log, in NATS, or in the Nix
   store.
3. A bundle can have several members, for example three Claude Code accounts.
   The hub gives each agent one member, by the rule of the token pools
   decisions. A Claude Code session that reaches a usage limit restarts on the
   next free member.
4. Outside tools can push usage data with `borant usage report`. Borant ships
   no usage adapter.

## Cuts

- Wrapper mode (`wrap`, `params`, `Wrap`). No backend needs it yet.
- Bundles on spawn, profiles, and channels, the four-level merge, merge rule 4
  (a child names only bundles of its parent), and the scheduler filter on
  bundles. Phase 1b gives collections their own bundles, and these levels
  belong there. In phase 1, bundles come only from the harness defaults in
  `host.toml` (`[secrets.harness]`), and every host of a harness defines them.
- Refresh of files on a TTL (`refresh`, `ttl`). Values are read at each
  launch.
- `HostInfo.problems` and `borant doctor`. A failed store at host start logs
  an error, and the host does not offer the harnesses whose bundles fail.
- The alert when every member is limited. The hub has no notifier yet
  (roadmap phase 2). Phase 1 logs a `warn`, and `borant members` shows the
  state.
- `borant move --member`, file members, Pi and omp limit detection, and
  LunaRoute pools.
- Laptop setup. The laptop host keeps its interactive logins.

## Constraints

- Strict ports and adapters (AGENTS.md). No core type, rule, or config key
  names 1Password, Claude, LunaRoute, or a vendor. `claude-auth`,
  `CLAUDE_CODE_OAUTH_TOKEN`, and `op://` refs exist only in `host.toml`, which
  `borant-infra` generates. Claude Code behavior (the `StopFailure` hook, the
  `autoContinueAtUsageLimit` setting) lives in the Claude Code harness adapter
  and the Claude Code plugin, not in `borant-core`.
- Only the host daemon calls `SecretStore`. The hub stores bundle and member
  names, never refs or values.
- Hooks must exit 0 with no output when the daemon is down.
- `host.toml` uses `deny_unknown_fields`. `borant-infra` adds `[secrets]` only
  after a Borant release that parses it.
- `MoveReason` and `SessionEventBody` have no `#[serde(other)]`. A new variant
  needs a release that reaches every host and the hub before it is sent. All
  of them update together from one release, so this is acceptable.
- The brief forbids new cloud hosts before roadmap phase 4. The done test for
  a new box needs Chris (see "Questions for Chris").

## Names

| Concept | Name | Why |
|---|---|---|
| Several accounts behind one bundle | Bundle members | `pools` already means elastic host pools (`[[pools]]` in the elastic compute doc) |
| Rules in `hub.toml` | `[secrets.members]` | Not `[pools]` |
| The pure rule | `borant-core::rules::members` | Not `rules::pools` |
| The CLI | `borant members` | Not `borant pools` |
| Store capabilities | `SecretCapabilities` | `Capabilities` is also in the elastic compute doc |

## Interfaces

These are the shared names. Every task body repeats the part it uses.

### Port (`crates/borant-core/src/ports/secrets.rs`)

As in section 2.1 of the secrets doc, without wrapper mode:

```rust
pub struct SecretRef(pub String);   // serde(transparent)
pub struct BundleName(pub String);  // name_type!, safe to log
pub struct MemberName(pub String);  // name_type!, safe to log
pub struct Secret(zeroize::Zeroizing<String>); // no Serialize, no Clone, Debug prints "Secret(..)", expose(&self) -> &str

pub struct Bundle {
    pub name: BundleName,
    pub store: String,
    #[serde(default)] pub vars: BTreeMap<String, SecretRef>,
    #[serde(default)] pub files: BTreeMap<String, SecretRef>,
    /// Commands that run before the harness, each one argv, with the vars set.
    #[serde(default)] pub pre: Vec<Vec<String>>,
    /// Env vars of each member. Member vars win over `vars`.
    #[serde(default)] pub members: BTreeMap<MemberName, BTreeMap<String, SecretRef>>,
}

pub struct SecretCapabilities { pub resolve: bool }
pub struct Resolved { pub values: BTreeMap<SecretRef, Secret> }

#[async_trait]
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn kind(&self) -> &'static str;
    fn capabilities(&self) -> SecretCapabilities;
    /// All refs or an error. One missing ref fails the whole call.
    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved>;
}
```

`PortErrorKind` gets `Invalid` and `Denied` (the exec runner task adds them,
with the mapping in `crates/borant-core/src/proto.rs`). `borant-core::testing`
gets `secret_store_contract(store, known, missing)` with rules 1 to 4 of
section 2.3 of the secrets doc (rule 5 is wrapper mode, cut).

### Records and protocol (all new fields `#[serde(default)]`)

- `HostInfo.members: BTreeMap<BundleName, MemberBundle>`, where
  `MemberBundle { members: Vec<MemberName>, harnesses: Vec<Harness> }`: the
  bundles of this host with two or more members, and the harnesses whose
  defaults in `[secrets.harness]` include the bundle (`"*"` expanded). The
  hub picks a member only for a bundle whose `harnesses` hold the harness of
  the launch.
- `LaunchOrder.members: BTreeMap<BundleName, MemberName>` and
  `Agent.members: BTreeMap<BundleName, MemberName>`: the member of each such
  bundle for this agent.
- `MemberState { bundle, member, limited_until: Option<Timestamp>, waiting: Vec<AgentId> }`
  at KV key `member.<bundle>.<member>`, written only by the hub. `waiting`
  holds the agents that wait for this member to reset, so a hub restart
  still wakes them.
- `MemberRules { unknown_reset_mins: u64, max_restarts_per_hour: u32 }` in
  `borant-core::rules::members`, with `Default` 60 and 3. The limit restart
  task reads them from `[secrets.members]` in `hub.toml`.
- `UsageWindow { name: String, used: f64, resets_at: Timestamp }`, where
  `used` is a share from 0 to 1.
- `ToolCall::UsageReport { bundle, member, windows: Vec<UsageWindow> }`,
  allowed for humans. The hub keeps the latest report of each member in
  memory only. A report is fresh for 15 minutes.
- `ToolCall::ListMembers`: the members of each bundle, with their state and
  their latest report.
- `CtlRequest::MemberLimited { agent, session, token, bundle, member, resets_at: Option<Timestamp> }`
  from the host.
- `MoveReason::Limit`.
- `SessionEventBody::SecretsIssued { bundles: Vec<BundleName>, members: BTreeMap<BundleName, MemberName> }`:
  names only.

### Exec protocol

The exec plugin protocol of the elastic compute doc ("Exec plugin protocol")
is the one source of truth. The secrets doc restates it with differences, and
those lose. The secrets ops, exactly:

```json
{"v": 1, "op": "describe"}
{"ok": {"v": 1, "kind": "secrets", "resolve": true}}

{"v": 1, "op": "resolve", "refs": ["op://Borant/github/token"]}
{"ok": {"values": {"op://Borant/github/token": "..."}}}
```

The caller ignores unknown fields in an `ok` value, such as `backend` or
`ttl_secs`. When the store config names a `credential`, the runner sets
`BORANT_CREDENTIALS_DIRECTORY` (the folder) and `BORANT_CREDENTIAL` (the file
name in it). The plugin reads `$BORANT_CREDENTIALS_DIRECTORY/$BORANT_CREDENTIAL`. One crate, `borant-exec`, holds the runner: it starts the program
with an empty environment plus the `env` table of its configuration, writes one
JSON request to stdin, reads one JSON reply from stdout, applies the timeout,
and runs the `describe` handshake. The secrets adapter is one user of the
runner. The provisioner of roadmap phase 4 is the next one.

### The secret directory and the shim

As in section 4.1 of the secrets doc. The directory is
`/run/borant/agents/<agent>/` (0700) on Linux and `$TMPDIR/borant/agents/<agent>/`
on macOS. It holds an `env` file (`KEY=VALUE` lines, values without newlines,
0600), a `pre.json` file (a JSON list of argv lists, the `pre` commands of the
bundles in order, no values), and one file for each `files` entry (0600). The launch sets only
`BORANT_SECRETS_DIR` and runs `borant secret-exec <dir> -- <program> <args>`.
The shim reads `env`, deletes it, sets the vars, runs each `pre` command with
`{dir}` replaced by the directory, then calls `exec` on the program.

### `host.toml` and `hub.toml`

```toml
# host.toml, generated by borant-infra
[secrets.stores.op]
kind = "exec"
command = ["borant-secrets-1password"]
credential = "store-token"     # a file in $CREDENTIALS_DIRECTORY
timeout_secs = 10

[secrets.bundles.claude-auth]
store = "op"
[secrets.bundles.claude-auth.members]
a = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-a/oauth-token" }
b = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-b/oauth-token" }
c = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-c/oauth-token" }

[secrets.harness]
"*" = ["github"]
claude-code = ["claude-auth"]
omp = ["lunaroute"]

# hub.toml
[secrets.members]
unknown_reset_mins = 60     # when a limit gives no reset time
max_restarts_per_hour = 3   # for each agent, then the agent waits
```

Store kinds: `env` (reads the host environment, a ref is a variable name),
`file` (`root = "<dir>"`, a ref is a file name under it), and `exec`
(`command`, `env`, `credential`, `timeout_secs`). The crate `borant-secrets`
owns the store config types (`StoreConfig`, a serde enum tagged by `kind`)
and `async fn open_store(&StoreConfig) -> PortResult<Arc<dyn SecretStore>>`.
For `exec`, `open_store` runs `describe` and fails if the plugin is not a
secrets plugin with `resolve`. `crates/borant/src/config.rs` uses these types
and calls `open_store` at host start. A failed store logs an error, and the
host leaves out the harnesses whose bundles use it.

## Member rules (`borant-core::rules::members`)

The hub picks a member of each bundle in `HostInfo.members` of the target host,
at each launch: spawn, move, takeover, and limit restart.

1. A bundle with one member needs no pick. It is not in `HostInfo.members`.
   Claude Code waits for its own reset (`autoContinueAtUsageLimit` keeps its
   default).
2. Keep only the members that the host defines and that are not limited now.
3. If the current member of the agent is in the list, keep it.
4. If some kept members have a fresh usage report, pick the one with the
   earliest next reset (the earliest `resets_at` over its windows). Members
   without a fresh report come after them.
5. Otherwise, pick the first free member in name order.
6. If no member is free, the agent waits. At a limit restart, its session
   stays alive and idle, and the hub adds the agent to `waiting` of the
   member with the earliest reset. When that `limited_until` passes, the hub
   sends the agent a direct message: "Your account limit reset. Continue
   your task." The hub logs a `warn` once: "every member of <bundle> is
   limited until <time>".

A pure function with tests holds rules 2 to 6:
`pick(members: &[MemberName], current: Option<&MemberName>, states: &[MemberState], reports: &[(MemberName, Vec<UsageWindow>)], now: Timestamp) -> Pick`
where `Pick` is `Member(MemberName)` or `WaitUntil(Timestamp)`.

## Limit restart

1. For a launch with a member of a bundle that has two or more members, the
   Claude Code harness adapter (`crates/borant-launch/src/harness.rs`) adds
   `--settings '{"autoContinueAtUsageLimit":false}'`. The session then stops
   at a limit instead of waiting.
2. The Claude Code plugin adds a `StopFailure` hook with matcher `rate_limit`
   that runs `borant hook stop-failure`. The hook parses the reset time from
   the hook input (format from the fact check), and sends it to the host
   daemon. It exits 0 with no output in every case.
3. The host daemon adds the bundle and the member of the session and sends
   `MemberLimited` to the hub.
4. The hub marks the member limited until `resets_at`, or for
   `unknown_reset_mins`. If another member is free, the hub restarts the agent
   on the same host with that member, through the planned move path
   (`PrepareMove`, then `MoveReady`, then a launch) with `MoveReason::Limit`.
   `place_agent` refuses a held lease (`crates/borant-hub/src/moves.rs:96`),
   so the old session must end through that path first. `move_agents` skips a
   move to the same host (`moves.rs:227`). The limit restart has its own entry
   point and allows the same host.
5. Loop guard: the hub counts the limit restarts of each agent in memory
   (`Memory`). After `max_restarts_per_hour` restarts in one hour, the agent
   waits as in rule 6. A hub restart resets the count.
6. The new session continues from `session_history`, as after a move. The lost
   turn and the lost native context are accepted (token pools decision 5).

## Tasks

Code tasks build in parallel where they touch different files. The interface
task goes first because the others compile against it, but the tasks that do
not use its types start at the same time.

| Task | Who | Blocked by | Files |
|---|---|---|---|
| Core interfaces for secrets and members | worker | none | `crates/borant-core` (ports, model, proto, testing) |
| Exec runner crate | worker | none | new `crates/borant-exec`, `PortErrorKind` in `crates/borant-core/src/ports/mod.rs` and its mapping in `proto.rs` |
| The `borant secret-exec` shim | worker | none | `crates/borant/src` (new module, cli, main) |
| Env and file secret stores | worker | Core interfaces | new `crates/borant-secrets` |
| Exec secret store | worker | Core interfaces, Exec runner | `crates/borant-secrets/src/exec.rs` |
| Secrets in host.toml | worker | Core interfaces, Env and file secret stores | `crates/borant/src/config.rs`, `crates/borant-host/src/lib.rs` (config, HostInfo) |
| Resolver mode in the host launch | worker | Core interfaces, Secrets in host.toml | `crates/borant-host/src/{commands.rs,sessions.rs,persist.rs,delivery.rs}` |
| Member rules and hub assignment | worker | Core interfaces | `crates/borant-core/src/rules/members.rs`, `crates/borant-hub/src/{moves.rs,members.rs}`, `crates/borant/src/cli.rs` (`members`, `usage report`) |
| Limit restart | worker | Member rules, Resolver mode, Token facts | `crates/borant/src/hook.rs`, `integrations/claude-code/hooks/hooks.json`, `crates/borant-launch/src/harness.rs`, host relay, hub `MemberLimited` |
| Secrets facts on borant-2 | orchestrator | none | none |
| Token facts | Chris and orchestrator | Chris's tokens | none |
| 1Password plugin, vault, and seed unit | orchestrator or helper | Exec runner (protocol only) | `borant-infra` |
| Phase 1 release and deploy | helper, orchestrator | every code task | none |
| New-box test | orchestrator | release, plugin, Chris's answer | none |

### Core interfaces for secrets and members

Acceptance: the types, trait, error kinds, records, and protocol variants of
"Interfaces" exist with `#[serde(default)]` on new fields. `Secret` has no
`Serialize`, and its Debug prints no value. `secret_store_contract` holds
rules 1 to 4. Old JSON of `Agent`, `HostInfo`, and `LaunchOrder` still parses.
Proof: `just test-crate borant-core secrets` with a test for the Debug output,
one for old JSON, and the contract run against a test store in the crate.

### Exec runner crate

Acceptance: `borant-exec` runs a program by the exec plugin protocol of the
elastic compute doc: an empty environment plus the configured `env`, one JSON
request on stdin, one JSON reply on stdout, a timeout that kills the program,
and `describe`. A non-zero exit, bad JSON, or a timeout gives a `PortError`
with the plugin's error code mapped. No secret type is used. Proof:
`just test-crate borant-exec` with a shell fixture plugin for each case.

### The `borant secret-exec` shim

Acceptance: `borant secret-exec <dir> -- <program> [args]` reads `<dir>/env`,
deletes it, sets the vars, runs each `pre` command from `<dir>/pre.json` (a
JSON list of argv lists) with `{dir}` replaced and the vars set, and calls
`exec` (Unix) on the program. A missing `env` file
fails with a clear error. A failed `pre` command fails before the `exec`.
Proof: `just test-crate borant secret_exec` with a fixture directory and a
program that prints its env.

### Env and file secret stores

Acceptance: a new crate `borant-secrets` with `EnvStore` and `FileStore`, both
pass `secret_store_contract`. `FileStore` reads only under its root and
refuses `..`. The crate also holds `StoreConfig` and `open_store` (see "The
secret directory and the shim" and the store kinds), with the `exec` variant
returning `Invalid` until the exec store task adds it. Proof: `just test-crate borant-secrets contract`.

### Exec secret store

Acceptance: `ExecStore` in `borant-secrets` uses `borant-exec` with the
`resolve` operation of the protocol, passes `secret_store_contract` against a
shell fixture plugin, and puts no value in its Debug output or its errors.
Proof: `just test-crate borant-secrets exec`.

### Secrets in host.toml

Acceptance: `[secrets.stores]`, `[secrets.bundles]` (with `members`), and
`[secrets.harness]` parse into the host config. `HostInfo.members` lists the
bundles with two or more members. A host with no `[secrets]` works as today.
A store that fails at start logs an error, and the host leaves out the
harnesses whose bundles use it. Proof: `just test-crate borant config` and
`just test-crate borant-host secrets_config`.

### Resolver mode in the host launch

Acceptance: a launch resolves the bundles of its harness (with the member of
`LaunchOrder.members` for a member bundle), writes the secret directory, sets
only `BORANT_SECRETS_DIR`, and wraps the program in `borant secret-exec`. A
session end deletes the directory. A host start deletes directories with no
live session. The session log gets `SecretsIssued` with names only. The
exact-value redactor of section 6 of the secrets doc removes each resolved
value from session-log text. The bundle and member of each session persist
across a host restart. Proof: `just test-crate borant-host resolver` with an
`EnvStore`: the tmux arguments, the session log, and the `LaunchSpec.env`
contain no value, and the directory is gone after the session ends.

### Member rules and hub assignment

Acceptance: `rules::members::pick` holds rules 2 to 6 with one test each. The
hub assigns `LaunchOrder.members` and `Agent.members` at each launch, keeps
`MemberState` in KV, keeps usage reports in memory, and serves
`ToolCall::UsageReport` and `ToolCall::ListMembers`. `borant usage report
--bundle B --member M --window NAME:USED:RESETS_AT` (repeatable) and
`borant members` work. Proof: `just test-crate borant-core members` and
`just test-crate borant-hub members`.

### Limit restart

Acceptance: the five steps of "Limit restart", with the loop guard. Proof: a
borant-hub test with a fake `MemberLimited`: the agent restarts on the same
host with the next member, and with every member limited it waits, then
restarts after the reset. A hook test: a `StopFailure` input with a reset time
gives one `MemberLimited`, and with the daemon down the hook exits 0 with no
output.

### Orchestrator and Chris tasks

| Task | Done when |
|---|---|
| Secrets facts on borant-2 | The six checks of secrets doc phase 0 have a result in `.orchestrator/notes.md` |
| Token facts | The four checks of token pools phase 0 have a result. They need three `claude setup-token` tokens from Chris. |
| 1Password plugin, vault, and seed unit | `borant-secrets-1password` passes the exec protocol tests in `borant-infra`. The `Borant` vault and the read-only `borant-hosts` service account exist (Chris creates them). The host module generates `[secrets]` and loads the store token with `LoadCredential`. |
| New-box test | A new box registers and launches a Claude Code and an omp worker with no login. `ps`, the session logs, and a NATS capture show no secret value. |

## Questions for Chris

1. The brief forbids new cloud hosts before phase 4. The done test needs a new
   box. Default: create `borant-3` for the test only, and destroy it the same
   day (less than $1). The other choice is to reinstall borant-2.
2. The `Borant` vault and the read-only `borant-hosts` service account: only
   you can create them. Where does its token live, so that `borant-infra` can
   push it to the boxes? Default: an item in the `Agents` vault, which the
   orchestrator's service account can read.
3. The store token is readable by agents on the box until sandboxes land
   (roadmap question 12). Default: accept it until phase 5.
4. Three `claude setup-token` tokens, one for each account in the pool, and the
   LunaRoute and GitHub tokens for the boxes, saved in the `Borant` vault.

## Settled after the builder check

The builder check asked six questions. The answers are in the sections above:
member picks only for the harnesses of a bundle (`MemberBundle`), the exact
secrets ops and the credential file (`BORANT_CREDENTIAL`), `open_store` in
`borant-secrets`, the limit restart through the planned move path with an
idle wait, the restart count in hub memory and `waiting` in KV, and `pre` as
a list of argv lists. Three tasks edit `crates/borant/src/cli.rs` (the shim,
the member commands, the hook). They run in parallel and rebase.

## Decision records

Decision: cut bundles on spawn, profiles, channels, the merge, and the bundle
scheduler filter to phase 1b. Evidence: profiles and channel bundles do not
exist (the research found no profile code), and phase 1b gives collections
their own bundles. Reversible: yes. Friend: none, scope cut. Blocker: none.

Decision: rename token pools to bundle members. Evidence: `[[pools]]`,
`rules::pool`, and `HostInfo.pool` belong to elastic compute. Reversible: yes.
Friend: none, naming. Blocker: none.

Decision: one exec runner crate, with the elastic compute protocol as the
source of truth. Evidence: the secrets doc restates the protocol with
differences (no empty env, capability strings). Reversible: yes. Friend: none.
Blocker: none.

Decision: usage reports live in hub memory only, and limits live in KV.
Evidence: reports go stale in 15 minutes and outside tools send them again. A
limit lasts hours, and a hub restart must not relaunch on a limited member.
Reversible: yes. Friend: none. Blocker: none.

Decision: a one-member bundle needs no pick and keeps the Claude Code wait.
Evidence: token pools decision 2. Reversible: yes. Friend: none. Blocker:
none.
