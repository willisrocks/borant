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
launcher = "orca"        # orca, tmux, zellij, or process
always_on = false
max_agents = 4
ssh = "me@laptop-a"      # for `borant attach` from other machines
harnesses = ["claude-code", "pi", "omp"]
```

Then run `borant host`. Every key is optional. Without `harnesses`, the
daemon uses the harness programs that it finds on `PATH`.

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

`spawn` takes an optional `model`, for example `opus`. The host adds
`--model <model>` to the Claude Code, Pi, or omp command, after the command
in `host.toml` and before the prompt. Moves and takeovers start the agent
with the same model. If a move starts the agent in another harness, the
host starts it without the model. Without `model`, the harness uses its own
default.

Delivery rules:

- A direct message wakes the agent.
- A broadcast reaches each member at its next turn, unless it is urgent.
- A task event wakes the owner only when a task closes, gets stuck, or needs
  a human.

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

## Commands

| Command | Purpose |
|---|---|
| `borant status` | The local host, all hosts, and all agents |
| `borant events [--follow] [--since 1h] [--kind lease.*] [--agent A] [--channel C] [--host H] [--severity warn] [--json]` | The swarm event stream, one line per event or JSON |
| `borant send <agent\|#channel> <text> [--urgent]` | Send a message as a human |
| `borant spawn --channel C --prompt-file F [--harness H] [--model M] [--role R] [--host H] [--task C/ID \| --task-title T] [--stop-on-close] [--priority N] [--name N] [--key K] [--json]` | Start an agent in a channel |
| `borant wait <agent> \| --task C/ID [--timeout 2h]` | Wait for work: exit 0 done, 1 closed otherwise, 2 at the timeout |
| `borant cancel <agent> [--close-task REASON]` | Stop an agent and cancel its queued spawn |
| `borant ps [--queued] [--json]` | The spawns that wait for a host |
| `borant logs <agent> [--follow]` | The session log of an agent |
| `borant tasks <channel> [--ready]` | List tasks |
| `borant move <agent> [--host H] [--harness X] [--here]` | Move one agent |
| `borant move --from <host> --here` | Move all agents of a host to this host |
| `borant move --channel <channel> --here` | Move all members of a channel |
| `borant drain <host>` | Move all agents away from a host |
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
