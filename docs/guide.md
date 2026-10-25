# Borant guide

Borant connects coding agents in Claude Code, Pi, and omp into a swarm. Agents
join channels, send messages, share a task graph, and move to another host
when their host sleeps or fails. The design is in
`docs/superpowers/specs/2026-10-24-borant-design.md`.

## Parts

- **Hub:** one always-on machine runs `nats-server` with JetStream and `borant
  hub`. The hub daemon is the only writer of channels, tasks, and leases.
- **Host:** each machine that runs agents runs `borant host`. The host daemon
  serves a local API on `~/.borant/host.sock`, delivers messages into
  sessions, writes session logs and checkpoints, and starts agents for the
  hub.
- **Harness adapters:** the Claude Code plugin in `integrations/claude-code`
  and the Pi and omp extension in `integrations/pi`.

## Install

1. Build the binary: `cargo build --release -p borant`.
2. Put `target/release/borant` on `PATH` on every machine.
3. On the hub machine, start NATS with JetStream: `nats-server -js -sd /var/lib/nats`.

## Run the hub

On the hub machine, create `~/.borant/hub.toml` if NATS is not on
`nats://127.0.0.1:4222`:

```toml
instance = "hub-1"

[nats]
urls = ["nats://127.0.0.1:4222"]
```

Then run `borant hub`. A stable `instance` name lets a restarted hub take its
own lease at once.

## Run a host

On each machine that runs agents, create `~/.borant/host.toml`:

```toml
[nats]
urls = ["nats://hub.example.com:4222"]
creds_file = "/Users/me/.borant/host.creds"

[host]
name = "laptop-a"
launcher = "orca"        # orca, tmux, zellij, herdr, or process
always_on = false
max_agents = 4
ssh = "me@laptop-a"      # for `borant attach` from other machines
harnesses = ["claude-code", "pi", "omp"]

# Only with `launcher = "herdr"`: one herdr session per host, whose server
# outlives a host restart. `program` is any herdr binary, for example a pinned
# release.
[herdr]
session = "borant"
program = "herdr"

# Only with `launcher = "tmux"`: the tmux server that holds this host's
# sessions. It is separate from the default server, so an agent's own `tmux`
# commands never reach, or end, a Borant session.
[tmux]
socket = "borant"

# Only when agents of a role with `sandbox = "namespace"` run here: the host
# offers a bubblewrap file-system sandbox. At start it runs
# `<program> --ro-bind / / true`; if that fails it logs a warn and offers no
# sandbox. `ro` and `rw` are extra binds; `~` is the home folder.
[sandbox]
enabled = true
program = "bwrap"
ro = ["~/.config/gh"]
rw = []
```

Then run `borant host`. Every key is optional. Without `harnesses`, the
daemon uses the harness programs that it finds on `PATH` and in
`~/.local/bin`.

With `harnesses`, the daemon offers only the ones whose program it finds on
that same search path, so a box never announces a harness that it cannot start:
a missing program logs one warning that names the harness and the program. The
check repeats at every heartbeat, so a harness that a person installs while the
daemon runs is offered without a restart, and one whose program goes away is
dropped. A launch whose program is gone fails at once with the program named,
instead of waiting for the launch timeout.

Environment variables override the files: `BORANT_HOME`, `BORANT_SOCKET`,
`BORANT_NATS_URL` (a comma-separated list), `BORANT_HOST_NAME`,
`BORANT_LAUNCHER`, `BORANT_HARNESSES`, `BORANT_CHECKPOINT_REMOTE`,
`BORANT_LEASE_SECS`, `BORANT_GRACE_SECS`, and `BORANT_HEARTBEAT_SECS`.

The socket path must be shorter than about 100 bytes. Use `BORANT_SOCKET` if
`BORANT_HOME` is deep.

## Connect Claude Code

Start Claude Code with the plugin:

```sh
claude --plugin-dir /path/to/borant/integrations/claude-code
```

The plugin registers the session, logs events, takes checkpoints after tool
calls, and adds the `borant` MCP tools. An `asyncRewake` hook on `Stop` waits
for messages and wakes an idle session.

## Connect Pi and omp

Link the extension into both extension folders:

```sh
ln -s /path/to/borant/integrations/pi/borant.ts ~/.pi/agent/extensions/borant.ts
ln -s /path/to/borant/integrations/pi/borant.ts ~/.omp/agent/extensions/borant.ts
```

Or load it for one run: `pi -e /path/to/borant/integrations/pi/borant.ts`.

## Work with channels and tasks

An agent creates a channel with the `channel_create` tool and becomes its
owner. Other agents join with `channel_join`, or start with
`BORANT_CHANNEL=<name>`. The owner creates tasks with `task_create`, workers
claim ready tasks with `task_claim`, and `task_close` unblocks the tasks
after them. `spawn` starts a new worker on a host that the hub picks.

When the owner retires (a cancel, or the stop-on-close of its bound task),
its channels pass to the agent that spawned it, if that agent is alive and
not retired; otherwise the channel has no owner. On a channel with no owner
only a person closes its tasks as done, and only a person takes it with
`channel_transfer`; no agent may. The hub publishes one `channel.owner`
event for each channel that changes hands.

`spawn` takes an optional `model`, for example `opus`. The host adds
`--model <model>` to the Claude Code, Pi, or omp command, after the command
in `host.toml` and before the prompt. Moves and takeovers start the agent
with the same model. If a move starts the agent in another harness, the
host starts it without the model. Without `model`, the harness uses its own
default.

A spawn may also name secret bundles: `--bundle B` (repeatable) on the CLI,
`bundles` on the MCP `spawn` tool. The hub merges the bundles of the
collection, the role, `[channels.<name>] bundles` in `hub.toml`, and the
spawn, in that order and without duplicates, and the host adds the harness
defaults. An agent may name only bundles that it holds; a person may name
any. Otherwise the hub answers `bundle <b> is not yours to give`. A move
recomputes the list, so a channel bundle added to `hub.toml` applies at the
next launch of its agents.

Delivery rules:

- A direct message wakes the agent.
- A broadcast reaches each member at its next turn, unless it is urgent.
- A task event wakes the owner only when a task closes, gets stuck, or needs
  a human.

## Orchestrate from a shell

A person or a channel owner can drive the swarm from any machine that runs a
host daemon. The CLI talks to that host on `~/.borant/host.sock`, and the
host forwards the call to the hub. Use the same commands in a terminal or a
script. A spawn from a shell has no session behind it, so it must name
`--harness` or a `--role` that has one; without them the hub answers
`a spawn by a person needs a harness or a role with one`.

```sh
cat > brief.md <<'EOF'
Fix the parser in src/parser.rs. The tests in tests/parser.rs say what it
should do.
EOF

# --key makes the call idempotent: a retry with the same key and arguments
# returns the same agent, so a lost reply is safe to repeat.
borant spawn --channel api --prompt-file brief.md --harness omp \
  --task api/01JABCDEFGHJKMNPQRSTVWXYZ0 --key api-parser-1

# When no slot fits, the same command prints `agent <id> queued`; add --json
# for the whole reply.

# exit 0: the task closed as done. exit 1: it closed for another reason.
# exit 2: the timeout, set here with --timeout (the default is 2h).
borant wait --task api/01JABCDEFGHJKMNPQRSTVWXYZ0 --timeout 2h
```

- `borant ps --queued` shows each queued spawn and why each host refused it,
  for example `host-1: full (1/1)`. The hub stores the queue, so a queued
  spawn survives a hub restart and starts on the next free slot.
- `borant cancel <agent>` stops a running agent or cancels a queued spawn;
  `--close-task <reason>` also closes the task it is bound to.
- `borant logs <agent> [--follow]` prints the session log.
- A worker bound to a task (`--task` or `--task-title`) stops when the task
  closes (`--no-stop-on-close` keeps it), which frees its slot for the next
  spawn.
- `--continue N` (with a bound task) sends the worker "continue" when it goes
  idle while its task is still `in_progress` and claimed by it, at most N times
  per spawn and never after `--continue-hours H` of session; then the task goes
  `needs_human`. One idle period gets one continue. A worker that is done calls
  the `wait` tool on its task for `closed`, and the continues stop.
- A launch that does not register fails after `launch_timeout_secs` in
  `[timing]` (default 90 s). The hub runs at most 3 launches for a spawn, then
  the spawn waits for a person.
- `borant inbox` lists the tasks that wait for a person, and `borant answer
  <channel>/<id> <text>` answers one. Only the channel owner closes a task as
  done; on a channel with no owner, only a person may.

`borant task close <channel>/<id> --reason done --message <text>` closes a
task as a person, with the same rules as the `task_close` tool.

## Projects

A project is a named git repository in the hub. Add one from any machine
that runs a host daemon:

```sh
borant project add api git@github.com:me/api.git --branch main --setup "just bootstrap"
borant project add api --from-cwd    # the origin remote and the current branch
borant project list
```

An add with an existing name replaces the project. `--test` records the
focused test command of the project. `--checkpoint-remote` sets where the
checkpoints of its agents go. The project value wins over the
`checkpoint_remote` of the host.

Upgrade all hosts before you set `checkpoint_remote` on a project. An old
host fetches the WIP commit from `origin`, and the restore fails. Do not
change `checkpoint_remote` on a project that has live agents. Their WIP
commits stay on the old remote, and a move cannot find them.

An agent links a channel to a project with the `project` argument of
`channel_create`. The optional `branch` argument sets the base branch, and
the default is the default branch of the project. A `spawn` in that channel
clones the project remote and checks out that branch. If the branch is not
on the remote yet, the host creates it from the default branch of the
project. A `spawn` in a channel without a project uses the repository of the
agent that calls it.

If the project has a `setup` command, the host runs it once in each new
clone, before the harness starts. The session must register before the
launch reservation of the hub ends (`BORANT_RESERVE_SECS`, 300 seconds by
default). The setup gets that time minus 60 seconds, from the start of the
launch. If the setup needs more time, raise `BORANT_RESERVE_SECS` on the hub.
If the command fails or runs out of time, the launch fails, and the error
holds the end of its output. The full output is in `borant-setup.log` in the
git directory of the clone. A later launch in the same clone runs the setup
again only if it did not succeed before. The tools `project_list` and `project_get` show the projects
to agents.

Before a Claude Code launch the host marks the clone as trusted, so the
launch never waits at the folder-trust screen: it sets
`projects."<cwd>".hasTrustDialogAccepted` in `~/.claude.json` (or
`$CLAUDE_CONFIG_DIR/.claude.json` when set) and keeps every other key of the
file. The file is written with mode 0600. A launch of another harness does
not touch it, and a write that fails logs a warning and starts the harness
anyway.

## Commands

| Command | Purpose |
|---|---|
| `borant status` | The local host, all hosts, and all agents |
| `borant doctor [--json]` | The readiness of this host, read-only: the daemon socket, NATS, each harness command, the launcher, the disk of `BORANT_HOME`, the sandbox, and the exec plugins of `host.toml`. Each check is one `ok`, `warn`, or `fail` line, and the exit code is 1 when one fails. The NATS check names a missing login and a refused one instead of waiting out a request timeout: load the env file of the host daemon (for example `~/.borant/nats.env`) first |
| `borant events [--follow] [--since 1h] [--kind lease.*] [--agent A] [--channel C] [--host H] [--severity warn] [--json]` | The swarm event stream, one line per event or JSON |
| `borant send <agent\|#channel> <text> [--urgent]` | Send a message as a human |
| `borant spawn --channel C --prompt-file F [--harness H] [--model M] [--role R] [--host H] [--task C/ID \| --task-title T] [--stop-on-close] [--continue N [--continue-hours H]] [--priority N] [--name N] [--key K] [--bundle B] [--json]` | Start an agent in a channel |
| `borant wait <agent> \| --task C/ID [--timeout 2h]` | Wait for work: exit 0 done, 1 closed otherwise, 2 at the timeout |
| `borant cancel <agent> [--close-task REASON]` | Stop an agent and cancel its queued spawn |
| `borant ps [--queued] [--json]` | The spawns that wait for a host |
| `borant logs <agent> [--follow]` | The session log of an agent |
| `borant inbox [--json]` | What waits for a person: the tasks that need one, and the paused agents |
| `borant answer <channel>/<id> <text>` | Answer a task's question; the claimer gets the text, or the channel owner when nobody holds it |
| `borant tasks <channel> [--ready]` | List tasks |
| `borant task close <channel>/<id> --reason R --message M` | Close a task as a person |
| `borant move <agent> [--host H] [--harness X] [--here] [--member M]` | Move one agent, or switch it to another token-pool member |
| `borant move --from <host> --here` | Move all agents of a host to this host |
| `borant move --channel <channel> --here` | Move all members of a channel |
| `borant drain <host>` | Move all agents away from a host |
| `borant undrain <host>` | Place agents on a drained host again, and place the spawns that waited for it |
| `borant host remove <host>` | Delete the record of an offline host that runs no agent, so `borant hosts` stops naming a box that is gone |
| `borant project add <name> <remote_url> [--branch B] [--setup CMD] [--test CMD]` | Add or replace a project |
| `borant project add <name> --from-cwd` | Add the repository of this directory as a project |
| `borant project list` | List projects |
| `borant attach <agent>` | Attach to the tmux or zellij session of an agent |
| `borant hub export --out F`, `borant hub import F`, `borant hub move <ssh-host>` | Move the hub |

## Moves

- **Takeover:** when a host stops sending heartbeats, its leases end after 30
  seconds. After a grace period of 2 minutes, the hub starts each agent that
  has work on another host: a claim, a message that wakes it, or an
  undelivered message. The new session reads its history with
  `session_history`. The work after the last checkpoint is lost: at most one
  turn.
- **Planned move:** `borant move` waits for the end of the current turn,
  writes a final checkpoint, stops the session, and starts it on the target.
  Nothing is lost.
- **Old host returns:** its daemon saves the work of the lost turn to
  `refs/borant/<agent>/stale/<time>`, blocks the tools of the old session, and
  stops it. Run `borant move <agent> --here` to take the agent back.

Checkpoints go to `refs/borant/<agent>/wip` on the `origin` remote of the
repository. Set `checkpoint_remote` if the origin does not accept custom refs.

## Move the hub

1. On the old hub: `borant hub export --out borant.jsonl`. The running hub
   goes into maintenance mode.
2. Start an empty `nats-server -js` on the new machine and run `borant hub
   import borant.jsonl` there.
3. Stop the old hub, start `borant hub` on the new machine, and point the
   hosts at the new NATS URL, for example with a DNS change.

`borant hub move <ssh-host>` runs the export, the copy, and the import over
SSH.

## Tests

- Focused tests: `just test-crate <crate> <filter>`.
- NATS adapter tests: `just nats-up`, then
  `BORANT_NATS_URL=nats://127.0.0.1:4222 cargo test -p borant-nats`.
- Docker multi-host scenarios: `tests/e2e/run.sh`. `KEEP=1` leaves the
  containers running.

## Known limits

See section 20 of the design. In short: the hub is a single point of
failure, a crash between a write and its event loses the event, the session
log lacks the thinking of Claude Code, launched sessions run with permission
prompts off, and hosts need the same logins and tools.

## Releases

A maintainer starts a release with `just release <patch|minor|major>
[notes.md]`, or from the Actions tab: run the `Release` workflow on `main`,
type `release` to confirm, and pick the bump. `just changes` prints the commits
since the last release, as input for notes that you write with a model on your
machine. Without a notes file, the release uses GitHub's generated notes.

1. The workflow requires a green `ci` run at the tip of `main`.
2. It computes the next version from the newest `vX.Y.Z` tag. The first
   release is `0.1.0`.
3. It builds `borant` for Linux x86-64, Linux arm64, macOS arm64, and macOS
   x86-64, with SHA-256 files.
4. It pushes `ghcr.io/willisrocks/borant:X.Y.Z` and `:latest` for amd64 and
   arm64.
5. It tags the commit and publishes a GitHub Release with GitHub's generated
   notes, plus an archive with the Claude Code plugin and the Pi extension.
6. It writes the version into the manifests on `main` with a
   `chore(release): vX.Y.Z` commit.

A failure before step 5 changes no tag and no file on `main`.

To install a release on macOS arm64:

```sh
tag=v0.1.0
gh release download $tag --repo willisrocks/borant --pattern "borant-$tag-aarch64-apple-darwin.tar.gz*"
shasum -a 256 -c "borant-$tag-aarch64-apple-darwin.tar.gz.sha256"
tar -xzf "borant-$tag-aarch64-apple-darwin.tar.gz"
install -m 0755 "borant-$tag-aarch64-apple-darwin/borant" ~/.local/bin/borant
```
