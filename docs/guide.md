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

Delivery rules:

- A direct message wakes the agent.
- A broadcast reaches each member at its next turn, unless it is urgent.
- A task event wakes the owner only when a task closes, gets stuck, or needs
  a human.

## Commands

| Command | Purpose |
|---|---|
| `borant status` | The local host, all hosts, and all agents |
| `borant send <agent\|#channel> <text> [--urgent]` | Send a message as a human |
| `borant tasks <channel> [--ready]` | List tasks |
| `borant move <agent> [--host H] [--harness X] [--here]` | Move one agent |
| `borant move --from <host> --here` | Move all agents of a host to this host |
| `borant move --channel <channel> --here` | Move all members of a channel |
| `borant drain <host>` | Move all agents away from a host |
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
