# Orchestration API, placement, sandboxes, and launchers

Date: 2026-10-24. Status: draft for review. Track 3 of 5.

Related track docs, written at the same time:

- `2026-10-24-secrets-and-host-bootstrap-design.md`: secrets, the NixOS image,
  harness logins, first-run screens, and the host readiness test
- `2026-10-24-elastic-compute-design.md`: hosts that start and stop on
  demand, microVM hosting, and a provisioner port
- `2026-10-24-observability-design.md`: dashboard, metrics, and timelines
- `2026-10-24-durable-orchestration-vision-design.md`: the long roadmap

## Summary

Borant can already place a worker on the least-loaded host, but only an agent
can ask for it, the model cannot be chosen, a queued spawn lives only in hub
memory, and every session runs with full access to the host user. This doc
adds five things, in this order. First, a `borant spawn` CLI and a JSON API
on the existing host socket, so that humans and scripts can spawn, wait,
cancel, and read logs, with idempotency keys stored on the records that the
hub already writes. Second, a `model` field and named model profiles, so
that owners run on Claude Code and workers run on omp with DeepSeek through
LunaRoute. Third, placement with labels, taints, memory headroom, channel
spread, and a persisted spawn queue with priorities, all as pure rules in
`borant-core`. Fourth, a bubblewrap sandbox that wraps the program of any
launcher, first for the file system and then for network egress through an
allowlist proxy in the host daemon. Fifth, a herdr launcher for the VPS hosts,
which gives a better attach view and an agent state (working, idle, blocked)
that Borant can use as a health signal.

## 1. Goals and non-goals

Goals:

1. A human or a script spawns a worker with a harness, model, host or labels,
   channel, prompt, repository, limits, and sandbox.
2. A script can wait, cancel, list the queue, and read logs, with JSON output.
3. A retry of spawn, claim, or close after a lost reply does the work once.
4. Placement is deterministic, has no I/O, and says why a spawn waits.
5. A worker with prompts off cannot read the NATS credentials, the SSH keys,
   or other clones, and later cannot reach unlisted network hosts.
6. A herdr launcher works on the VPS hosts next to tmux.

Non-goals: an HTTP API on the hub, client SDK crates (the JSON shapes are the
SDK), dollar budgets (Borant cannot see token cost in every harness),
automatic rebalancing, sandboxes on macOS laptops (the user watches owners in
Orca), and microVMs (`elastic-compute`).

## 2. Facts from the code today

| Fact | Where |
|---|---|
| The hub handles one request at a time under one lock | `crates/borant-hub/src/lib.rs`, `Hub::handle` |
| `spawn` is refused for a human (`allowed_for_human`) | `crates/borant-core/src/proto.rs` |
| A spawn copies the repository of the calling agent | `crates/borant-hub/src/moves.rs`, `spawn` |
| A spawn that finds no host waits in `Memory.pending_moves`, which is in memory only. A hub restart loses its target and prompt. | `crates/borant-hub/src/moves.rs`, `lib.rs` |
| `schedule::place` filters on online, drain, `max_agents`, repo access, and sorts by harness match, agent count, always-on, name | `crates/borant-core/src/rules/schedule.rs` |
| `LaunchSpec` has `program` and `env`. Each launcher runs `program` as is. | `crates/borant-core/src/ports/launch.rs` |
| The harness command has no model flag. A host can override the whole command only. | `crates/borant-launch/src/harness.rs` |
| `task_claim` already returns success when the same agent claims again. `task_close` of a closed task fails with `TaskClosed`. | `crates/borant-hub/src/tasks.rs`, `rules/tasks.rs` |
| Each agent gets a full clone, not a shared git worktree | `crates/borant-host/src/checkpoint.rs`, `restore` |
| A launched session that never registers, for example at a first-run screen, waits with no error | `crates/borant-host/src/commands.rs` |

## 3. Orchestration API

Recommendation: keep one API path. The CLI and scripts call `POST /v1/call`
on the host socket, the host daemon forwards to `borant.ctl`, and the hub
stays the single writer. Add new `ToolCall` variants and allow them for a
human. Do not add an HTTP server on the hub.

### Options compared

| Option | Gain | Cost |
|---|---|---|
| A. CLI plus the host socket (chosen) | No new server, no new auth. Works on every host that runs a daemon. The outbox and the reconnect logic already exist. | A script must run on a host, or reach one over SSH. |
| B. HTTP/JSON on the hub over Tailscale | Scripts on any tailnet machine | A second protocol on the single writer, a new auth layer, and a second place to version |
| C. Scripts talk NATS directly | No Borant code | Scripts need NATS credentials and the wire format. A bug in a script can write anything. |

A later gateway, `borant api serve --listen <tailscale-ip>:7420`, can serve
the same handler over TCP with `tailscale whois` for the caller name.

### CLI

```sh
borant spawn --channel api --prompt-file task.md \
  [--profile worker | --harness omp --model lunaroute/deepseek-4.1-flash:auto] \
  [--host borant-2 | --require vps --tolerate laptop] \
  [--repo git@github.com:willisrocks/api.git --branch feat/x]   # default: the git remote and branch of the cwd
  [--task <id> | --task-title "Fix the parser"] [--stop-on-close] \
  [--max-turns 40] [--max-minutes 90] [--sandbox bwrap] \
  [--priority 10] [--name parser-1] [--key <idempotency-key>] [--json]
borant wait <agent> | --task <channel>/<id> | --channel <channel> --all-closed [--timeout 2h]
borant cancel <agent> [--close-task wontfix]
borant ps [--channel api] [--queued] [--json]        # agents and the spawn queue, with the reason that a spawn waits
borant logs <agent> [--follow] [--screen]            # session log pages, or the terminal screen
borant run graph.json                                # fan-out of a task graph, phase 5
```

`borant wait` exits 0 when the target is done, 1 when a task closes with a
reason other than `done`, and 2 at the timeout. Thus a shell script can join
work with `&&`.

### Wire shapes

A spawn request from any caller:

```rust
pub struct SpawnSpec {
    pub channel: ChannelName, pub prompt: String, pub name: Option<AgentId>,
    pub profile: Option<String>, pub harness: Option<Harness>, pub model: Option<String>, // section 4
    pub repo: Option<RepoInfo>,          // required for a human caller
    pub placement: PlacementReq,         // section 5
    pub task: Option<TaskBinding>,       // Existing(TaskId) or New(NewTask)
    pub limits: Limits,                  // max_turns, max_minutes, stop_on_close
    pub sandbox: Option<SandboxKind>,    // section 6
    pub priority: i32,                   // default 0, higher first
}
// New ToolCall variants, all allowed for a human:
ToolCall::Spawn(SpawnSpec)
ToolCall::Cancel { agent: AgentId, close_task: Option<CloseReason> }
ToolCall::Queue { channel: Option<ChannelName> }
ToolCall::AgentHistory { agent: AgentId, page: usize, page_size: usize }
// CtlRequest::Call gets an optional key. Old hosts send none.
CtlRequest::Call { actor: Option<Actor>, call: ToolCall, #[serde(default)] key: Option<String> }
```

```json
POST /v1/call
{"session": null, "key": "ci-812-parser",
 "call": {"tool": "spawn", "channel": "api", "prompt": "Fix the parser.", "profile": "worker",
          "task": {"existing": "01J9..."}, "placement": {"require": ["vps"]}, "limits": {"max_turns": 40}}}
→ {"ok": {"agent": "omp-4f2a1c", "state": "queued", "reason": "all hosts are full (4/4, 4/4)"}}
```

`borant wait` uses a new long-poll route on the host socket, `GET
/v1/wait?agent=X` or `?task=C/ID`. The host daemon watches the state keys of
the agent or the task, so the hub gets no new request type for a wait.

### Completion, cancel, and slots

A worker is done when its bound task closes. Today a worker that finished its
task stays idle and holds a slot of `max_agents`. With `stop_on_close`, the
hub sends `HostCommand::Stop` for the session of the agent when its bound
task closes, and marks the agent `retired`. A retired agent is never placed
again by a takeover. A worker with no bound task is done when its session
ends.

`borant cancel` removes a queued spawn, or stops the session and retires the
agent. Its claims go back to `open`, or close with the given reason.

A script also needs a channel with no owner agent. Recommendation: reserve
the agent ID `human` as an owner with no sessions, whose task events go to the
notifier and to `borant wait` (question 2).

### Idempotency

Recommendation: store the key on the record that the operation creates, and
make the other operations safe to repeat by their own rules. No separate key
table.

| Operation | Rule |
|---|---|
| `spawn` with a key | The agent record gets `spawn_key`. If `name` is absent, the hub derives the name from the key, for example `omp-` plus 6 hex characters of a hash. A retry finds the agent with the same `spawn_key` and returns its current state. The same key with a different request returns `conflict`. |
| `task_create` with a key | The task gets `create_key`. A retry scans the tasks of the channel for the key. The hub already reads all tasks of a channel for a close. |
| `task_claim` | Already safe: a claim by the agent that holds it returns success. |
| `task_close` | Change: a close of a closed task with the same reason returns the task. A different reason returns `conflict`. |
| `cancel`, `channel_create` | A repeat of a finished cancel returns success. A `channel_create` of an existing channel with the same owner returns the channel. |

This fits the single writer. The hub runs one request at a time, so the
lookup and the write are one step, and the key lives in the same document as
the result, so a crash cannot write one without the other. The CLI and
`borant mcp` make a key for each call and retry with it on `unavailable`.
This removes the result rebuild that the stress-test driver script needed.

### Projects

Recommendation: add a project record to the hub. A project is a named git
repository, like a project in Orca. A channel belongs to one project, and a
spawn takes its repository from the project of its channel. Today a spawn
copies the `RepoInfo` of the agent that calls it, and a channel has no
repository, so a human or a script must name the repository on every spawn.

The record lives at `project.{name}` in the state store. The hub is its only
writer.

```rust
pub struct Project {
    pub name: ProjectName,            // "api"
    pub remote_url: String,           // "git@github.com:willisrocks/api.git"
    pub default_branch: String,       // "main"
    pub checkpoint_remote: Option<String>,
    pub setup: Option<String>,        // run once in a new clone, e.g. "just bootstrap"
    pub test: Option<String>,         // the focused test for close gates
    pub bundles: Vec<BundleName>,     // secrets for every agent of the project
    pub labels: Vec<String>,          // host labels that the project needs
}
```

Commands:

```sh
borant project add api git@github.com:willisrocks/api.git   # or --from-cwd
borant project list
borant channel create health --project api --branch feat/health
borant spawn --channel health --harness omp --prompt-file brief.md
```

Rules:

- `channel_create` gets an optional `project` and an optional base `branch`.
  A spawn in that channel clones the project and checks out the base
  branch. Without a project, a spawn keeps today's rule: the repository of
  the calling agent.
- The host keeps one bare mirror for each project in
  `~/.borant/projects/<name>.git` and gives each agent a `git worktree`
  from it. A spawn then fetches only new commits, and N agents use one
  object store, not N full clones.
- Placement filters on the labels of the project. The per-agent list of
  hosts that failed a clone (`has_repo_access`) becomes a list for each
  project.
- The Orca launcher opens the agent worktree as a workspace of the Orca
  project with the same remote. This closes the main Orca gap in section 7.
- The `orchestrate` skill in the Claude Code plugin uses the project of its
  channel, so a brief needs no clone or push steps.

Cut: one channel across several repositories (use one channel for each
project), several projects inside one monorepo, and permissions per project.

## 4. Model choice and profiles

Recommendation: add a `model` field to `SpawnSpec`, `Agent`, and
`LaunchOrder`, and add named profiles in `hub.toml`. The agent record stores
the profile name, so a move to another harness can pick the right model for
that harness.

### Model flags for each harness

| Harness | Flag | Notes |
|---|---|---|
| Claude Code | `--model <alias or ID>` | Aliases include `opus`, `sonnet`, `haiku`, `opusplan`. `ANTHROPIC_MODEL` is the environment form. |
| omp | `--model <pattern>` | Fuzzy match, for example `lunaroute/deepseek-4.1-flash:auto`. Roles (`--smol`, `--slow`, `--plan`, `modelRoles` in `~/.omp/agent/config.yml`) set the models of subagent roles. |
| Pi | `--model <provider/id[:thinking]>` | `--provider` needs `--model`. |
| Codex | `--model` | Not tested in Borant. |

`harness::command` gets a `model: Option<&str>` argument and puts the flag
before `--` and the prompt, for example `omp --model
lunaroute/deepseek-4.1-flash:auto -- <prompt>`.

### Profiles

```toml
# hub.toml
[spawn]
default_profile = "worker"   # when a spawn names no profile, harness, or model
[profiles.owner]
harness = "claude-code"
model = "opus"
max_running = 2              # a cost guard across the swarm
spawn_by = ["human"]         # only a human can spawn an owner
[profiles.worker]
harness = "omp"
model = "lunaroute/deepseek-4.1-flash:auto"
sandbox = "bwrap"
models = { claude-code = "haiku" }   # the model if a move lands on another harness
```

A pure function in `borant-core/src/rules/profile.rs` resolves a request:

```rust
pub struct Resolved { pub profile: Option<String>, pub harness: Harness, pub model: Option<String>, pub sandbox: Option<SandboxKind> }
pub fn resolve(spec: &SpawnSpec, caller: &Sender, profiles: &Profiles, running: &HashMap<String, u32>) -> Result<Resolved, CoreError>
```

Rules: explicit fields override the profile. A caller that is not in
`spawn_by` gets `invalid`. A profile at `max_running` makes the spawn wait in
the queue with the reason "profile owner is at its limit (2)". This gives the
policy "owners on Claude, workers on DeepSeek" with no model turn.

## 5. Placement and the spawn queue

Recommendation: extend `schedule::place` with a request struct, labels,
taints, memory headroom from the heartbeat, and channel spread. Keep a fixed
sort key, so tests stay exact. Persist the spawn queue in the state store.

### Inputs

```rust
// HostInfo gets:
pub labels: BTreeSet<String>,     // "vps", "eu", "gpu", "untrusted"
pub taints: BTreeSet<String>,     // "laptop": no worker lands here unless it tolerates it
pub sandboxes: Vec<SandboxKind>,
pub reserve_mem_mb: u32,          // default 1024
// HostHeartbeat gets, with serde default so old hosts still work:
pub resources: Option<HostResources>, // cpus, load1_milli, mem_avail_mb, disk_avail_mb

pub struct PlacementReq { pub host: Option<HostName>, pub require: BTreeSet<String>, pub tolerate: BTreeSet<String> }
pub enum Reject { Offline, Draining, Full, NoRepo, NoHarness, MissingLabel(String), Taint(String), NoSandbox, LowMemory }
pub fn place(req: &Request, hosts: &[HostView]) -> Result<(HostName, Harness), Vec<(HostName, Reject)>>
```

The error value lists each host and its reject reason. `borant ps --queued`
prints it, for example "borant-1: full (4/4), borant-2: missing label gpu".

### Filter and score

A host qualifies if the current rules pass, and if all of these are true: it
has each required label, each of its taints is tolerated, it supports the
sandbox kind, and its reported `mem_avail_mb` is above `reserve_mem_mb`. A
host with no `resources` passes the memory rule, so old hosts keep working.

The sort key, smallest first, is: harness mismatch, active agents, agents of
the same channel on the host, not always-on, then the host name. The channel
term spreads a channel across hosts, so a lost host stops fewer of its
workers. Load average is shown but not sorted on, because memory is the
tighter limit for agents on a CPX31 with 8 GB.

### Persisted spawn queue

Each spawn writes `spawn.{agent_id}` before placement:

```rust
pub struct SpawnRecord {
    pub agent: AgentId, pub spec: SpawnSpec, pub resolved: Resolved, pub key: Option<String>,
    pub state: SpawnState,   // Queued, Launching { host }, Running, Failed { error }, Cancelled
    pub queued_at: Timestamp, pub attempts: u32, pub last_reject: Vec<(HostName, Reject)>,
}
```

The tick reads the queued records, sorts them by priority (high first) and
then by `queued_at`, and places each one that fits. A record that does not
fit does not block the records after it. This replaces the spawn entries in
`Memory.pending_moves`, so a hub restart keeps the queue. Planned moves keep
their current path. `borant hub export` includes the new keys, because the
export reads all state keys.

### Cut from placement

- Affinity to the repository cache. Each agent gets a full clone today, so no
  host has a cache to prefer. It returns if `restore` starts to use a shared
  mirror with `git clone --reference`.
- Affinity to the owner. All traffic goes through the hub, so the host of the
  owner gives no latency gain.
- Rebalancing. A later `borant rebalance --dry-run` can list planned moves of
  idle agents with no claims. Running agents move only when a human asks.

## 6. Sandboxes

Recommendation: a `bwrap` sandbox kind on Linux that Borant builds as a
wrapper around the program of any launcher. Phase one isolates the file
system and keeps the network open. Phase two removes the network and gives
the agent only an allowlist proxy that the host daemon runs.

The agent needs its clone in `~/.borant/work/<agent>/<repo>`, the host socket
(for `borant mcp`, hooks, and the omp extension), its harness directory
(`~/.claude` and `~/.claude.json`, or `~/.omp`), `/nix/store`, a few `/etc`
files, and egress to the LLM APIs, GitHub, and package registries.
Checkpoints run in the host daemon, outside the sandbox, so the agent needs
no git push rights for them.

### Options compared

| Option | Blast radius with prompts off | Egress control | Socket and clone access | Startup | NixOS fit |
|---|---|---|---|---|---|
| None (today) | The whole host user: NATS credentials, SSH keys, all clones, the tmux socket of every agent | None | Full | 0 | Yes |
| Claude Code sandbox mode | Bash commands only. File tools, MCP servers, and hooks run outside it. | Domain allowlist proxy | Yes | Low | Yes |
| `srt` (sandbox-runtime) around the whole process | Read deny list, write allow list | Domain allowlist proxy | On Linux it can allow all Unix sockets or none, not one path | Low | Not packaged (unverified) |
| bubblewrap, Borant-built (chosen) | Only the paths that Borant binds | None, or no network plus a proxy | Bind the socket file and the clone | Milliseconds | `pkgs.bubblewrap` |
| systemd unit sandboxing | Strong, with cgroup limits | IP rules only | `BindPaths=` | Low | Native, but most options need a system unit and root |
| Rootless podman | Container | Proxy or firewall | Volume binds | Up to 1 s plus the image | Module exists, needs subuid setup and an image |
| nsjail | Namespaces plus seccomp | No domain filter | `-B`, `-R` | Low | Unverified package |
| gVisor | User-space kernel | Proxy | Fragile for host sockets in rootless mode | About 100 ms (unverified) | Package exists |
| Firecracker or microvm.nix | A VM: the strongest | Host TAP and nftables | Block devices, no shared folders in Firecracker | 125 ms target, 2 to 3 s with systemd | microvm.nix, needs KVM |
| Landlock (`landrun`) | File rules, one named socket on new kernels | TCP ports only | `--rw`, `--unix` | Near zero | `landrun` 0.1.15 |

Reasons for bubblewrap: it uses an allow list of binds, so a path that Borant
does not name does not exist in the sandbox. That hides `~/.borant/host.creds`,
`~/.ssh`, the other clones, and the tmux and herdr sockets in one step. The
argv is a pure function of the launch, so it is easy to test. omp and Pi have
no sandbox of their own, so a wrapper outside the harness is the only choice
that covers all harnesses. The Claude Code sandbox mode does not cover file
tools, so it does not limit an agent with prompts off. Nested bubblewrap
fails, so the Claude Code sandbox mode stays off inside a Borant sandbox.

A whole VPS is also a sandbox. A host with the label `untrusted` and only
scoped tokens is a strong boundary at no code cost. `elastic-compute` covers
VMs that start for one task.

### Interface

```rust
pub enum SandboxKind { None, Bwrap, BwrapNoNet }   // serde snake_case
// LaunchSpec gets `sandbox: Option<SandboxPlan>`:
pub struct SandboxPlan { pub kind: SandboxKind, pub rw: Vec<PathBuf>, pub ro: Vec<PathBuf>, pub sockets: Vec<PathBuf> }
// borant-launch/src/sandbox.rs: returns the new program argv
pub fn wrap(spec: &LaunchSpec) -> Vec<String>
```

The host daemon calls `wrap` before it calls the launcher. Thus tmux, herdr,
and the process launcher run a sandboxed program with no change. For `Bwrap`,
the argv is:

```sh
bwrap --die-with-parent --unshare-ipc --new-session \
  --ro-bind /nix/store /nix/store --ro-bind /run/current-system /run/current-system \
  --ro-bind /etc/resolv.conf /etc/resolv.conf --ro-bind /etc/ssl /etc/ssl \
  --proc /proc --dev /dev --tmpfs /tmp --tmpfs $HOME \
  --bind $HOME/.borant/work/<agent> $HOME/.borant/work/<agent> \
  --bind $HOME/.claude $HOME/.claude --bind $HOME/.claude.json $HOME/.claude.json \
  --bind $HOME/.borant/host.sock $HOME/.borant/host.sock \
  --chdir <cwd> -- claude --dangerously-skip-permissions --model opus -- <prompt>
```

`--new-session` blocks the TIOCSTI ioctl (typing into the parent terminal).
The argv has no `--unshare-pid`, because the host daemon tests liveness by
the harness PID that the hooks report, and a PID namespace changes that
number. The host configuration names extra paths, for example a read-only
`~/.config/gh`.

### Egress, phase two

`BwrapNoNet` adds `--unshare-net` and binds `~/.borant/proxy.sock`. The host
daemon serves an HTTP CONNECT proxy on that socket, with a domain allowlist
from the host configuration:

```toml
[sandbox.egress]
allow = ["api.anthropic.com", "lunaroute.*", "github.com", "*.githubusercontent.com",
         "registry.npmjs.org", "pypi.org", "files.pythonhosted.org", "crates.io", "static.crates.io"]
```

Inside the sandbox, `borant sandbox-shim --proxy-sock <path> -- <program>`
listens on `127.0.0.1:3128` in the empty network namespace, forwards each
connection to the proxy socket, sets `HTTPS_PROXY` and `HTTP_PROXY`, and then
runs the program. A tool that ignores the proxy variables fails, because no
other route exists. That failure is safe. The proxy writes each denied host
to the session log of the agent, which `observability` can show. Git must use
HTTPS remotes in this mode, because SSH does not go through the proxy.

Why native and not `srt`: the proxy and the shim are about 200 lines of
tokio. They use the same configuration file and log path as the rest of
Borant, and they need no new package. `srt` stays the fallback if the native
proxy fails in tests.

## 7. Launchers

### herdr adapter

Recommendation: add a `herdr` launcher for the VPS hosts, and keep tmux as
the default until herdr runs a full stress test.

herdr is an agent multiplexer in Rust (`herdrdev/herdr`, Apache-2.0, 0.9.5
of 2026-10-14). It has workspaces and panes, a server that keeps panes alive
after the client detaches, a JSON socket API, remote attach over SSH, and a
state for each agent pane: `idle`, `working`, `blocked`, `done`, or
`unknown`. It reads the state from the screen or from an integration. omp
needs the integration (`herdr integration install omp`). The mapping to the
`Launcher` port, with one herdr session named `borant` on each host:

| Port call | herdr command |
|---|---|
| `launch` | `herdr --session borant workspace create --cwd <cwd> --label borant-<agent> --env K=V ... --no-focus`, then `herdr agent start <name> --kind claude --pane <root_pane> -- <args>`. For a sandboxed launch, `herdr pane run <pane> exec <wrapped argv>`, because `agent start` runs the canonical program. |
| `stop` | `herdr workspace close <workspace>` |
| `is_running` | `herdr agent get <name>`, or a pane that still exists |
| `attach_command` | From a laptop: `herdr --remote <ssh> --session borant`. One view shows all agents of the host with their states. |
| New: `state` | `events.subscribe` on the herdr socket for `pane.agent_status_changed` |

Gains for Borant:

- `agent start` returns `agent_not_ready` when the agent does not reach its
  prompt in time. That is a direct signal for a blocked first-run screen.
- A `blocked` state for longer than a few minutes becomes a `needs_human`
  event for the owner of the agent. An `idle` agent with a claim and no turn
  for 30 minutes becomes a hint in `borant ps`.
- Attach from a laptop shows every agent of a host, not one tmux session.

Limits: agent names must match `[a-z][a-z0-9_-]{0,31}`, so the adapter uses
`b-` plus the agent ID, cut and hashed if it is too long. The project is
young and its protocol changes often (protocol 22), so Nix pins the version.
A screen reader can show an unknown prompt as `idle`. A headless herdr server
under systemd is not documented and needs a test.

### Orca adapter gaps

The Orca launcher runs `orca terminal create --worktree path:<cwd> --command
"exec env K=V ..."`. Gaps:

| Gap | Effect | Fix |
|---|---|---|
| `cwd` must be in a workspace that Orca knows. `~/.borant/work/...` is not one. | A move to a laptop fails at launch | Register the clone with Orca before the create. The exact command is unverified. |
| `--command` is typed into the login shell | A slow shell profile or a prompt plugin can eat the first keys | Keep `exec env`. Test with the real shell of the laptop. |
| Handles end when Orca restarts (`terminal_handle_stale`) | `is_running` returns false for a live session | Treat the registered PID as the truth, which the host daemon already does |
| The only state is `terminal wait --for tui-idle` | No `blocked` signal | Accept. Laptops are attended. |
| No idempotent create | A retried launch opens two tabs | `orca agent launch` has `--retry-launch <operationId>`. Use the spawn key, if Borant moves to `agent launch`. |

### Pre-launch hook and launch watchdog

Recommendation: two small host features connect the launcher to
`secrets-and-host-bootstrap`.

1. Pre-launch hook. The host configuration gets `[launch] pre = ["borant",
   "host", "check", "--harness", "{harness}"]`. The host daemon runs it before
   each launch. A failure sends `LaunchFailed` with the output, and the hub
   places the agent on another host. The test itself, and the files that skip
   first-run screens, belong to the bootstrap doc.
2. Launch watchdog. If a launched session does not register within
   `launch_timeout` (default 90 seconds), the host daemon reads the last 40
   lines of the screen, stops the session, and sends `LaunchFailed` with the
   screen text after secret redaction. The `Launcher` port gets one method
   with a default:

```rust
async fn capture(&self, handle: &LaunchHandle, lines: u16) -> PortResult<Option<String>> { Ok(None) }
// tmux: capture-pane -p -t =<name> -S -<lines>; herdr: pane read <pane> --source recent; orca: terminal read
```

This turns "the spawn waits forever" into "launch failed on borant-2: the
screen shows 'Choose a theme'".

## 8. Phased plan

Each phase ships alone and is useful alone.

| Phase | Content | Done when |
|---|---|---|
| 1 | `model` in `Spawn`, `Agent`, `LaunchOrder`, and `harness::command`. `borant spawn` for a human with `--repo` from the cwd. `spawn_key` and the `task_close` repeat rule. The launch watchdog with `capture` for tmux. | A shell script spawns two omp workers on DeepSeek and one Claude Code worker, retries a spawn with the same key, and gets one agent |
| 2 | Persisted `spawn.{agent}` queue with priorities. `borant ps`, `wait`, `cancel`, `logs`. Task-bound spawns with `stop_on_close`. Profiles in `hub.toml`. | A hub restart keeps a queued spawn. A worker frees its slot when its task closes. |
| 3 | Labels, taints, `resources` in the heartbeat, channel spread, reject reasons | `borant ps --queued` names the reason for each host |
| 4 | `Bwrap` sandbox, then `BwrapNoNet` with the proxy and the shim | A sandboxed worker cannot read `~/.borant/host.creds`, and later cannot reach a host outside the allowlist |
| 5 | herdr launcher with `state` and `capture`. `borant run graph.json`: the hub spawns a bound worker for each ready task with a `spawn` block, up to a limit for each channel. | A graph of five tasks runs to the end with no owner agent |
| Later | Tailscale HTTP gateway, `borant rebalance --dry-run`, Orca registration fix | When a need appears |

Cut and named: a hub HTTP API, client SDK crates, dollar budgets, macOS
sandboxes, repository cache affinity, owner affinity, automatic rebalancing,
podman and gVisor sandboxes, and microVMs (in `elastic-compute`).

## 9. Risks

| Risk | Effect | Response |
|---|---|---|
| A `--max-turns` stop in the middle of a task | Work after the last checkpoint is lost, as in a takeover | The host daemon stops the session only at `TurnEnd` |
| bubblewrap needs unprivileged user namespaces | The sandbox fails to start on a kernel that blocks them | NixOS allows them by default (unverified for 26.05). The pre-launch hook tests `bwrap --ro-bind / / true`. |
| Bun (omp) or Node (Claude Code) ignores `HTTPS_PROXY` for some calls | Those calls fail in `BwrapNoNet` | Safe failure. Test both harnesses before phase 4 ships. |
| A broad allowlist, for example `*.githubusercontent.com` | An agent can send data to any repository content host | Keep the list short. The proxy logs every host. |
| herdr protocol changes | The adapter breaks after an update | Pin the version in Nix. tmux stays the fallback. |
| The harness directory is writable in the sandbox | An agent can read or change its own login token | Accepted. The harness needs it. Scoped tokens belong to the bootstrap doc. |

## 10. Open questions for Chris

1. Do you want the Tailscale HTTP gateway soon, for example to start workers
   from a phone or from CI, or is the CLI on a host enough?
2. Is a reserved `human` pseudo-agent as a channel owner acceptable, or do you
   prefer `Channel.owner` as a sender type (agent or human)? The second is
   cleaner but changes a stored type.
3. Must `stop_on_close` be the default for every task-bound worker? It frees
   slots, but the owner cannot ask a stopped worker a follow-up question.
4. Which hosts must take workers: is a laptop always tainted `laptop`, so that
   only owners and explicit spawns land there?
5. Must every worker run sandboxed by default (`profiles.worker.sandbox =
   "bwrap"`), or only workers in channels that you mark as untrusted?
6. Is the LunaRoute endpoint one host name that the egress allowlist can
   name, or does it change?
7. Do you want herdr on the VPS hosts now for attach, before Borant uses its
   agent state?

## Sources

- Claude Code sandboxing (scope, settings keys, proxy, bypass mode): https://code.claude.com/docs/en/sandboxing
- Claude Code model configuration (`--model`, aliases, `ANTHROPIC_MODEL`): https://code.claude.com/docs/en/model-config
- sandbox-runtime (`srt`): https://github.com/anthropic-experimental/sandbox-runtime
- Codex sandbox modes: https://learn.chatgpt.com/docs/agent-approvals-security
- oh-my-pi (omp) models and roles: https://github.com/can1357/oh-my-pi, and local `omp --help` (18.12.6)
- Pi models: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/models.md
- systemd sandboxing options: https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html
- nsjail: https://github.com/google/nsjail
- gVisor rootless: https://gvisor.dev/docs/user_guide/rootless/
- microvm.nix: https://github.com/astro/microvm.nix
- Firecracker startup target: https://aws.amazon.com/blogs/opensource/firecracker-open-source-secure-fast-microvm-serverless/
- landrun and Landlock versions: https://github.com/zouuup/landrun, https://mynixos.com/nixpkgs/package/landrun
- herdr repository and docs: https://github.com/herdrdev/herdr, https://herdr.dev
- herdr socket API, agent states, and remote attach, under https://raw.githubusercontent.com/herdrdev/herdr/v0.9.5/docs/next/website/src/content/docs/: `socket-api.mdx`, `agents.mdx`, `persistence-remote.mdx`
- Local: `herdr --help`, `herdr agent start --help` (0.9.5), `orca terminal create --help`, `orca agent launch --help`, and the Borant code paths in section 2

Not verified:

- Whether `strictAllowlist` in Claude Code blocks unlisted hosts with
  `--dangerously-skip-permissions`. Borant does not depend on it.
- Whether a headless herdr server runs under systemd and keeps panes after
  logout.
- Whether herdr reads the agent state of a program that runs inside
  bubblewrap.
- The Orca command that registers a folder as a workspace.
- Packages for `srt` and nsjail in nixpkgs, and startup times other than the
  Firecracker figure.
- The herdr flake entry. herdr 0.9.5 on this laptop builds from the
  `herdrdev/herdr` v0.9.5 tag, but the flake file that adds it was not found.
