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

[roles.chief]
approves = true            # may approve a saga's gate

[collections.work]
mirror_issues = true       # copy objects out to an issue tracker

[mirror]
adapter = "exec"                    # the exec plugin protocol, kind "issues"
program = "/nix/store/.../bin/borant-mirror-linear"
credential = "linear-token"         # a file under CREDENTIALS_DIRECTORY
every_secs = 60                     # one sweep per minute
max_writes = 50                     # tracker calls per sweep
```

Then run `borant hub`. A stable `instance` name lets a restarted hub take its
own lease at once.

Only a person, or an agent whose role sets `approves = true`, closes a saga
gate as done. `mirror_issues` on a collection is the default for its
projects, and the `--mirror-issues` flag of a project overrides it. The
`[mirror]` table is absent when the mirror is off; `borant-infra` adds it,
pinned to a release that has the mirror.

### Secrets of a host that the hub creates

A fresh box that the hub creates carries only two secrets: a single-use join
token and the vars of bundle `host-boot`, for example the reusable tailnet
key. Once on the tailnet, the box trades the token at `POST /v1/join` for the
vars of bundle `host-join`, the store token and the NATS credential.

Give the hub the same `[secrets]` table that a host takes:

```toml
[secrets.stores.op]
kind = "exec"
command = ["borant-secrets-1password"]
credential = "op-store-token"   # a file in $CREDENTIALS_DIRECTORY

[secrets.bundles.host-boot]
store = "op"

[secrets.bundles.host-boot.vars]
TAILSCALE_AUTHKEY = "op://Borant/tailscale/key"

[secrets.bundles.host-join]
store = "op"

[secrets.bundles.host-join.vars]
STORE_TOKEN = "op://Borant/store/token"
NATS_PASSWORD = "op://Borant/nats/pool"
```

The hub opens those stores at start and resolves only the two bundle names. A
store that fails logs an error and leaves join off: pool hosts are created as
before, without a join. It stores only the SHA-256 of each token, with the host
name and an expiry of the pool's `boot_timeout`, and deletes it at the join,
at the host's delete, or at that expiry. `POST /v1/join` on the `[http] listen`
address takes `{"host": "<name>", "token": "<token>"}` and answers
`{"vars": {...}}` once, or 403 with one generic body for every refusal. Keep
the listener on the tailnet: this route is the only way a value leaves the hub.

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

### The chief-of-staff board

A session that starts with `BORANT_ROLE=chief` also draws the board: a band
above the prompt with what needs the person and one row per project, a pane
with the tabs Needs you and Projects (`/cos pane`, or `[1]` and `[2]` and up
while the prompt is empty), a toast when a new item needs the person, and
`/cos` for the digest as text, with `/cos <project>` for one project. Every
other session registers no timer, no command, and draws nothing.

The band, `/cos`, and the Projects tab list only the projects with open work:
an open saga, an open epic, or an open plain task. The header counts the rest
as idle ("2 active · 1 idle"), and `/cos <project>` still shows an idle
project. A project with plain tasks and no saga or epic shows its count of
open tasks ("3 open tasks") in place of the saga and epic.

The band's title row carries a dim hint of the keys and a `[ ? ]` button. The
help view, a third tab beside Needs you and Projects, opens from that button
and from `/cos help`, which prints the same text in the transcript without
starting a turn. `?` on its own at the prompt is Claude Code's own shortcuts
panel, and a band hotkey may only be a digit or a lowercase letter, so `?` is
the button's label and `/cos help` is the typed form. The keys are the real
bindings of Claude Code 2.1.310, which `~/.claude/keybindings.json` can change:

|Key|What it does|
|---|---|
|`1`–`9`|open the band's rows: `1` is what needs you, then one per project; the pane takes the keyboard|
|`0`|give the pane the keyboard, and open it if it is closed|
|Ctrl+X Tab|focus the band, then each pane, then back to the prompt; Tab must follow within 3 s|
|Ctrl+X Ctrl+A|collapse the band to one row, and expand it again|
|`j` `k`|move the selection through the rows, in the band and in the pane|
|Ctrl+X ← →|widen or narrow the docked pane; dragging its edge works too|
|`h` `l`|switch tabs; fold or unfold the selected node; `l` also opens a collapsed node or a leaf|
|Enter|open the selected project, saga, epic, or task: a task opens its detail|
|Esc|go back one level, and at the top close the pane|

Every row, tab, and button also works with the mouse: a click selects a row, a
second click (or a click on its `▸`) opens it. Drilling in keeps a breadcrumb in
the pane title and in a dim line at the top of the pane, for example
`borant › Elastic compute › Hub backup`. A task's detail shows its title,
status, owner agent, blockers with their titles and states, its PR link, and the
time it last changed. On Claude Code 2.1.310 a pane's Up and Down arrows scroll
the drawing, so `j` and `k` (or the footer's `↑` and `↓` buttons) move the
selection. Project, epic, and task views read the channel with
`borant tasks <channel>`; when that read fails the pane says `tasks unavailable`
quietly. (The board itself names no channel for a project's plain tasks, so
those appear only in the counts line unless the board names their channel.)

The band and the pane follow `borant events --follow`: a task's status, a
lease, an agent, a spawn, or a move reads the board again within about a
second. A read every 5 s catches what sends no event, such as a new task. The
title row says how old the board is (`updated just now`, `updated 12s ago`).
Done on a needs-you item marks the row `marking done…` at once and submits
"Chris marked done: …" to the chief, which closes a saga gate itself and asks
the channel's owner to close anything else. The row goes when the task
closes, and shows `still open` after two minutes.

The band reads `borant board --json`. A call from inside an agent's session is
the agent's read, so the band of a chief shows only its own collection; a
person's shell sees every collection. A launched session carries
`BORANT_AGENT`, and when its session does not resolve the CLI refuses the
board instead of reading it as the person, so a daemon that restarted shows
`board unavailable` with that reason and no rows rather than another
collection's projects. For a session without a hub, set
`BORANT_COS_BOARD=fixture` and it draws the fixture under
`integrations/claude-code/mod/fixtures` instead; any failed read shows
`board unavailable` with its reason quietly. The Done and Ask orchestrator
buttons in the pane submit a prompt to the chief session and call nothing
else.

To pick up a new version, run `claude plugin update borant`, then
`/reload-plugins` in a session that is already open.

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

The clone and the checkpoints of a launch run with the launch's bundle
variables, so a collection bundle can carry the git credential of its
repositories; the host has no git-specific config. The values reach the git
commands of the clone, the mirror fetch, the project `setup`, and each
checkpoint push of the session only as the environment of those child
processes, and a launch whose values do not resolve fails before it clones.
The host keeps a session's values in memory only. After a daemon restart it
resolves them again from the session's bundles, as it does for the redactor.

A session never inherits the host daemon's own credentials. Every launcher
removes `BORANT_NATS_USER`, `BORANT_NATS_PASSWORD`, and
`CREDENTIALS_DIRECTORY` from the program's environment, also when an older
tmux server still holds them. The session log redacts the value after any
name that contains `PASSWORD`, `PASSWD`, `SECRET`, `TOKEN`, `CREDENTIAL`,
`API_KEY`, `ACCESS_KEY`, or `PRIVATE_KEY`, or that ends in `_KEY`, such as
`BORANT_NATS_PASSWORD=` or `"password":`.

Delivery rules:

- A direct message wakes the agent.
- A broadcast reaches each member at its next turn, unless it is urgent.
- A task event wakes the owner only when a task closes, gets stuck, or needs
  a human.

### Sagas and epics

A channel has a kind, set at the create and never changed. `project` is the
default, and old records parse as this one. A `saga` must name a project:
the hub creates the gate task "Approve the scope and the epics", its epics
are tasks of kind `epic` with no parent, and every other task except the
gate has a chain of parents that ends at an epic. An `epic` channel must
name a project: the hub creates one root epic task with the channel title, a
task created without a parent gets the root, and a second epic task is
refused.

While the gate is not closed as `done`, `task_claim` and `spawn` are
refused with `saga_not_approved`, and `task_list(ready_only)` returns
nothing. The orchestrator creates the epics, then sets the gate
`needs_human` with an `attention_msg` that states the scope and lists the
epics.

A person (`borant task close`) or an agent whose role has `approves = true`
in `hub.toml` approves. The channel owner cannot approve its own scope
unless its role has the flag. When a gate goes `needs_human`, the hub sends
a direct message to each agent of the saga's collection whose role approves.
A close as `wontfix` rejects the saga, and the orchestrator then closes the
channel.

An epic is not work: no agent claims it, no spawn binds to it, `ready` never
returns one, and the board derives its status from its children.

Blocks flow down: a task is ready only when its own `blocked_by` tasks and
the `blocked_by` tasks of each parent are closed, so an epic that is blocked
by another epic holds back every child of it.

A spawn bound to a task starts on the `branch` property of the task's epic,
if it has one, otherwise on the channel branch. The orchestrator keeps it at
the top branch of the epic's stack, and the branch must be on `origin`
before the spawn.

A parent with open children cannot close, and when the root epic of an epic
channel closes the hub closes the channel. `channel_close` (MCP) and
`borant channel close <name>` close a channel, only for its owner or a
person, and only when every task of it is closed. A closed channel refuses
`task_create`, `task_claim`, and `spawn`.

The board is a computed read that stores nothing:
`borant board [--collection C] [--json]`, the MCP tool `board`, and
`GET /api/board?collection=C`. It shows the projects, sagas, epics, and the
questions that wait for a person, and the mod of a chief session draws it.

### Issue mirror

A project with `mirror_issues` on copies its items to an issue tracker, here
Linear, one way only: Borant is the source, and an edit in Linear never flows
back. The hub keeps the link in `issue` on the item, and a KV record beside
it. An issue that the mirror did not create keeps its own title and body.

What maps to what:

| Borant | Issue |
|---|---|
| A saga | an issue that holds the saga's epics |
| An epic task of a saga | an issue under the saga issue |
| A standalone epic channel | an issue that holds the issues of the root epic's tasks |
| Any other task with `pr_url` or `issue` | an issue under its nearest mirrored ancestor |

The root epic of an epic channel and a saga gate are never mirrored, and
neither is a task without a PR and without an issue. A task is a child of its
nearest mirrored ancestor, so a task under an epic task hangs off the epic's
issue.

The state of a task follows the task: `open` is `todo`; `in_progress`,
`needs_human`, and `stuck` are `started`; a close as `done` is `done`; any
other close is `canceled`. A container is `backlog` before its saga gate
approves, `started` once a child moves, and `todo` otherwise; a rejected saga
is `canceled`.

A create needs a team. The mirror takes the item's `issue_target`, then the
nearest ancestor's, then the team of the parent issue. A plain project
channel has no anchor task, so its tasks inherit nothing from the channel:
each one needs its own `issue_target` or a parent task that has one.
`issue_target` is the issue tracker team for this item and the items under
it, for example `ENG`; an empty string clears it. With no team, the item
goes `needs_human` with "Issue team needed: set issue_target on this task".
Answer it with `borant task update <channel>/<id> --issue-target ENG`, and
the next sweep restores the status.

Setting `issue` adopts an existing issue: the mirror writes the state only,
and never the title, the body, or the parent. Setting `issue` to `""` stops
the mirror for that item.

When several items link the same issue, one of them owns it, and only the
owner writes its state. The owner is a saga or epic channel before a task,
then the shallower item, then the older task. The other items roll up into
that issue: the mirror writes nothing for them, and their mirrored children
become sub-issues of it. To give such a child its own issue, clear its
`issue`.

The KV record is `mirror.<channel>` for a container and
`mirror.<channel>.<task>` for a task. It holds `issue` (the key), `hash`
(what the mirror last wrote), `was` (the state from before the mirror raised
`needs_human`), `owned`, `stopped`, `linked`, and `refused`.

A call that answers `unavailable`, times out, or returns `internal` is retried
on the next sweep. `not_found` tells the owner once. `invalid` skips the item
until its hash changes. `denied` pauses the mirror until the next hub start
and raises the `mirror_refused` alert.

A sweep runs every `every_secs` and makes at most `max_writes` tracker calls.
It writes parents first, then the oldest first. An item whose hash is
unchanged costs no call, a project with the mirror off makes no call, and a
closed history is not backfilled.

The adapter is an exec plugin of `kind: "issues"`. The hub writes one JSON
line per call and reads one back. `describe` answers
`{"ok":{"v":1,"kind":"issues"}}`; `create` and `update` take the write:

```
{"v":1,"op":"create","config":{},"write":{"key":"<uuid>","target":"ENG"|"null","parent":"ENG-1"|"null","title":"..","body":"..","state":"backlog|todo|started|done|canceled"}}
  -> {"ok":{"key":"ENG-123","url":"https://..."}}
{"v":1,"op":"update","config":{},"issue":"ENG-123 or URL","write":{...},"full":true|false}
  -> {"ok":{"key":"ENG-123","url":"https://..."}}
```

`target` and `parent` are always present, as `null` when unset, so a plugin
reads one key set for every write. `full` is false for an adopted issue, which
writes the state only. The private `borant-mirror-linear` plugin in
`borant-infra` speaks this format against Linear.

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
  for example `host-1: full (1/1)`, then the last error of a try that failed,
  for example `borant-1: full (6/6) | last error (borant-2): fatal: repository
  'x.git' not found`. The hub stores the queue, so a queued
  spawn survives a hub restart and starts on the next free slot. `borant ps`
  also shows a spawn that gave up, with its reason, for example `3 launches
  failed (borant-1, borant-2, borant-1): last error: fatal: repository 'x.git'
  not found`, or `3 launches did not register within 90s (borant-1, borant-2,
  borant-1)` for tries that only timed out, and the hub
  sends that reason to the agent that spawned it. A spawn that gave up retires
  its agent, so nothing launches it again, and stays in the list until a person
  cancels it. An agent that retired or was cancelled otherwise leaves the list.
- `borant cancel <agent>` stops a running agent or cancels a queued spawn;
  with no live session there is no turn end to wait for, so it retires the
  agent at once. `--close-task <reason>` also closes the task it is bound to.
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
  the spawn waits for a person. The host reports each launch that waits to
  register in its heartbeat, and stops the ones that the hub closed: a retired
  or paused agent, a failed or cancelled spawn, or a released lease, so no
  harness loops on a registration that cannot succeed.
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
| `borant board [--collection C] [--json]` | The board: projects, sagas, epics, and waiting questions, computed from the records; stores nothing |
| `borant answer <channel>/<id> <text>` | Answer a task's question; the claimer gets the text, or the channel owner when nobody holds it |
| `borant tasks <channel> [--ready]` | List tasks |
| `borant task close <channel>/<id> --reason R --message M` | Close a task as a person |
| `borant task update <channel>/<id> [--issue-target T] [--issue I]` | Change the issue link or the issue team of a task; an empty string clears it |
| `borant channel close <name>` | Close a channel as its owner or a person; every task of it must be closed first |
| `borant move <agent> [--host H] [--harness X] [--here] [--member M]` | Move one agent, or switch it to another token-pool member |
| `borant move --from <host> --here` | Move all agents of a host to this host |
| `borant move --channel <channel> --here` | Move all members of a channel |
| `borant drain <host>` | Move all agents away from a host |
| `borant undrain <host>` | Place agents on a drained host again, and place the spawns that waited for it |
| `borant host remove <host>` | Delete the record of an offline host that runs no agent, so `borant hosts` stops naming a box that is gone |
| `borant project add <name> <remote_url> [--branch B] [--setup CMD] [--test CMD] [--mirror-issues true\|false]` | Add or replace a project |
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
