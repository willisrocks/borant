# Borant design

Date: 2026-10-24. Status: draft for review.

Borant is a messaging and task system for coding agents in different
harnesses. Agents in Claude Code, Pi, omp, and later Codex join channels, send
messages to each other, and share a task graph. Agents can move between hosts
when a host sleeps, crashes, or loses its network.

Marks in this document:

- "(Decided)" means that the user made the decision.
- "(Call)" means that the designer made the decision and the user did not
  object. A builder can change a call with a good reason, but must record it.

Related notes:

- `docs/research/2026-10-23-harness-delivery.md`: how each harness can receive
  messages, with sources
- `docs/spikes/claude-asyncrewake/README.md`: the spike that proved the wake
  path for Claude Code

## 1. Intent

The user runs several coding agents at the same time, in different harnesses,
on two laptops and some cloud VPS machines. Today, these agents cannot talk to
each other. The user wants agents to work as a swarm: one agent owns a channel
and directs the work, and other agents do tasks and report back.

The user also moves between machines during the day. For example, the user
closes a laptop and walks to work. The swarm must keep working when that
happens. The agents of the closed laptop, including the owner, must continue
on another host. The user must then be able to take an agent back to a laptop.

## 2. Goals, non-goals, and anti-goals

### Goals

1. Agents in Claude Code, Pi, and omp register automatically and join channels
   by choice. (Decided)
2. An agent can send a direct message to one agent or a broadcast to a
   channel. (Decided)
3. A message reaches its target at once, also when the target is idle at its
   prompt. (Decided)
4. Each channel has one owner and a task graph (a DAG) with a status enum like
   kata and typed task properties from a schema. (Decided)
5. Agents on other hosts keep working, and their messages stay durable, while
   the host of the owner sleeps. (Decided)
6. Any agent, owner or worker, can move to another host. A move can be planned
   or unplanned. Both kinds are in v1. (Decided)
7. The user can move the hub to another host with a short downtime. (Decided)

### Non-goals for v1

- Codex support. It is a bonus after the required harnesses. (Decided)
- Search or analytics across sessions. (Decided)
- A live hub move or a highly available hub. (Decided)
- Adapters that load at runtime. Adapters are compile-time crates. (Decided)
- Host setup. The user sets up each host by hand. (Call)
- Files that git ignores, such as `.env` or build output. Checkpoints include
  untracked files that git does not ignore. (Call)
- Permissions for each channel or each agent. (Call)
- A TUI or web UI. The CLI comes first. (Call)

### Anti-goals

The design fails if any of these happen, even if it works otherwise:

- A message disappears with no error.
- Two live sessions of one agent run at the same time and both act.
- The owner agent must spend a model turn on each status change of a task.
- Borant depends on the native transcript format of a harness.

## 3. Constraints

- Language: Rust for all Borant binaries. The Pi and omp extension is
  TypeScript, because those harnesses load only TypeScript extensions.
  (Decided)
- Broker and storage: NATS JetStream behind ports, so another adapter, such as
  Postgres, can replace it. (Decided) NATS 2.11 or later, for a TTL on each KV
  key. (Call)
- Hosts: macOS laptops and Linux VPS machines.
- Harness versions on the research machine: Claude Code 2.1.310, Pi 1.4.0,
  omp 18.12.6. The Claude Code wake path needs the `asyncRewake` hook field.

## 4. Approaches

The main open choice was where the control-plane decisions happen.

| | Hub writer (chosen) | Peer writers |
|---|---|---|
| How it works | The hub daemon is the only writer for channels, tasks, and leases. Host daemons send requests to it. | Each host daemon writes to the shared NATS KV with compare-and-set. |
| Rules across keys | Safe, because the hub handles one request at a time | Each write can change only one key. Two edges added at the same time can make a cycle. |
| Lease expiry | The hub decides on its own clock. | Daemons race to declare a lease dead, each on its own clock. |
| Hub outage | Control writes wait in local outboxes. | Control writes fail, because the NATS hub holds the KV. |

The user chose the hub writer. Peer writers give no real gain in availability,
because both designs depend on the NATS hub, and peer writers add races.

Other options that the designer rejected:

- Omnara: it runs its own agent loop, so Claude Code and Pi sessions do not run
  in it.
- Pi Durable as the base: it supports Pi only.
- Built-in messaging of Claude Code: it covers Claude Code on one machine only.
- Redis: the user first chose Redis, then picked NATS JetStream for leaf nodes,
  durable consumers, and KV with compare-and-set.
- Go: Go can embed the NATS server, but the user prefers Rust for binaries.
- Native transcript import for moves: it depends on undocumented formats. The
  user chose a neutral session log instead.

## 5. Domain language

Use these terms in code, API, CLI, and docs. The "Do not use" words mixed
meanings during design.

| Term | Definition | Do not use |
|---|---|---|
| Swarm | One hub, its hosts, and all their agents and channels | Network, cluster |
| Hub | The always-on host that runs the NATS hub, the stores, and the hub daemon | Server, leader |
| Host | A machine that runs a host daemon and can run agents | Node, machine |
| Host daemon | `borant` in host role | Agent daemon, sidecar |
| Hub daemon | `borant` in hub role. It holds the hub lease and is the single writer. | Coordinator |
| Agent | A durable identity with an inbox, a lease, a session log, and claims | Logical agent |
| Session | One run of an agent in one harness on one host | Instance |
| Harness | Claude Code, Pi, omp, or Codex | Runtime, client |
| Harness session ID | The native ID that a harness gives a session | Session ID alone |
| Owner | The agent that created a channel. One for each channel. | Orchestrator (in the model) |
| Worker | A member of a channel that is not the owner | Child, subagent |
| Channel | A named group of agents with one owner, one task schema, one task graph, and one message stream | Room, topic |
| Member | An agent that joined a channel | Participant |
| Message | Text from an agent or a human. A direct message goes to one agent. A broadcast goes to all members. | Notification |
| Event | A notice that Borant makes, such as "task X is stuck" | Notification, alert |
| Inbox | The durable queue of messages and events for one agent | Mailbox |
| Delivery | The step that puts a message into a session | Push, injection |
| Wake | A delivery that starts a turn in an idle session | Rewake |
| Outbox | The local queue on a host for writes while the hub is away | Buffer |
| Task | A unit of work in a channel | Issue, ticket, job |
| Task graph | The DAG of tasks in a channel | Plan, pipeline |
| Ready | Derived: an `open` task whose prerequisites are all closed | Unblocked |
| Claim | The link from a task to the agent that works on it | Lock, assignment |
| Properties | Typed values on a task | Metadata, fields |
| Task schema | The JSON Schema for task properties in a channel | Property schema |
| Lease | A right with a time limit: an agent lease or the hub lease | Lock |
| Fencing token | A number that rises with each new lease | Epoch, generation |
| Heartbeat | The periodic renewal of a lease and of presence | Ping, keepalive |
| Presence | Whether a session is online | Status |
| Session log | The neutral log of session events for an agent | Transcript, history |
| Turn | One prompt or message and the work of the agent until it stops | Run, round |
| Checkpoint | A matched pair: a position in the session log and a WIP ref | Snapshot |
| WIP ref | A git ref that holds the working tree at a checkpoint | Shadow branch |
| Move | A change of host or harness for an agent. A planned move starts from a final checkpoint. A takeover starts from the last checkpoint after a lease ends. | Migration, failover |
| Drain | A planned move of all agents away from a host | Evacuate |
| Placement | The choice of a host for an agent | Routing |
| Port | A trait in the core | Interface, plugin |
| Adapter | A crate that implements a port | Plugin, driver |
| Harness adapter | The hooks, plugin, or extension that connects a harness to its host daemon | Integration |
| Control plane | Registry, channels, tasks, and leases | |
| Data plane | Messages and events | |
| Session data | Session logs, and WIP refs in git | Checkpoint data |

The user uses "orchestrator" for the role of an owner. The word can appear in
prompts and user docs, but not in the model or the API. (Decided)

## 6. Architecture

### Topology

```
                 +---------------------------------------+
                 | Hub host (always on, for example VPS) |
                 |  nats-server (JetStream)              |
                 |  borant hub daemon (single writer)    |
                 +-------------------+-------------------+
                                     | NATS client connections (TLS, credentials per host)
          +--------------------------+---------------------------+
          |                          |                           |
+---------+-----------+   +----------+----------+   +------------+--------+
| Laptop A            |   | Laptop B            |   | VPS 2               |
|  borant host daemon |   |  borant host daemon |   |  borant host daemon |
|  (local outbox)     |   |  (asleep)           |   |  (local outbox)     |
|  Claude Code, Pi    |   |                     |   |  Pi workers         |
+---------------------+   +---------------------+   +---------------------+
```

- The hub host runs `nats-server` with JetStream, and `borant` in hub role.
  No agent has to run on the hub. (Decided)
- Each other host runs `borant` in host role. The host daemon connects to the
  hub NATS server as a client, with a list of URLs. (Call, changed after the
  spikes: no leaf nodes. See section 23.)
- Rust cannot embed `nats-server`, so it is always a separate process on the
  hub host.
- A host can run the hub role and the host role together.

### One binary

`borant` is one binary with these roles (Call):

- `borant hub`: the hub daemon
- `borant host`: the host daemon
- `borant mcp`: a stdio MCP server that a harness starts. It talks to the
  local host daemon.
- `borant hook <event>`: the hook handler for Claude Code
- `borant <command>`: the CLI for the user and for agents

### Local API

The host daemon serves REST, SSE, and the backend of `borant mcp` on a Unix
socket, for example `~/.borant/host.sock`, with mode 0600. (Call)

The socket permission is the only local authentication. Any process of the
same OS user can call the daemon as any local agent. A token for each session
gives no real gain, because any process of the same user can read the token.

### Ports and adapters

The core holds the domain rules. Each port is a Rust trait. Each adapter is a
crate that a Cargo feature turns on. (Decided)

| Port | Purpose | v1 adapter |
|---|---|---|
| State store | Key-value documents with revisions, compare-and-set, and watch | NATS KV |
| Message bus | Publish, durable subscribe with a cursor for each agent, ack, broadcast, request and reply | NATS JetStream and core NATS |
| Session store | Append to a session log, read the last turns, trim old turns | NATS JetStream |
| Launcher | Start, stop, and attach to a session of a harness on a host | Orca, tmux, zellij, headless |
| Notifier | Tell the user about events that need a human | macOS desktop notification |

The state store port (Call):

```rust
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Entry>>;
    /// Fails if the key exists.
    async fn create(&self, key: &str, value: Bytes) -> Result<Revision>;
    /// Compare-and-set: fails if the current revision is not `expected`.
    async fn update(&self, key: &str, value: Bytes, expected: Revision) -> Result<Revision>;
    async fn delete(&self, key: &str, expected: Option<Revision>) -> Result<()>;
    async fn list(&self, prefix: &str) -> Result<Vec<Entry>>;
    async fn watch(&self, prefix: &str) -> Result<BoxStream<'static, Change>>;
}
```

The other ports follow the same style. Their exact functions are left to the
builder. The ports must not expose NATS types or NATS behavior. A Postgres
adapter must be possible for the state store, the message bus, and the
session store.

### Three kinds of data

| Kind | Content | Writer | Port |
|---|---|---|---|
| Control plane | Agents, sessions, channels, members, task schemas, tasks, leases | The hub daemon only | State store |
| Data plane | Messages and events | Any agent, through its host daemon | Message bus |
| Session data | Session logs, and WIP refs in git | The host daemon of the session that holds the lease | Session store, and git |

A host daemon never writes a control-plane key. It sends a
request to the hub daemon over the message bus, for example "claim task X".
The hub daemon checks the rules, writes, publishes the events, and replies.
(Decided)

A control-plane write often makes an event. The hub daemon writes first and
then publishes. A crash between the two loses the event. An outbox table can
fix this later. (Call)

### Key layout

| Key | Value |
|---|---|
| `agent.{agent_id}` | Name, preferred harness, branch, current session, claims |
| `session.{session_id}` | Agent, harness, harness session ID, host, working directory |
| `lease.agent.{agent_id}` | Holder session, host, fencing token, expiry |
| `lease.hub` | Holder host, fencing token, expiry |
| `host.{host}` | Harnesses (installed and logged in), launcher adapter, `max_agents`, always-on flag, drain flag |
| `chan.{channel}` | Owner, task schema, created time, closed time |
| `chan.{channel}.member.{agent_id}` | Join time |
| `chan.{channel}.task.{task_id}` | The task document (see section 9) |

Keys use dots, which fit NATS KV and a text primary key in Postgres. (Call)

## 7. Agents and sessions

### Identity

An agent is a durable identity. A session is one run of an agent. An agent
has at most one live session. (Decided)

- Agent IDs are names that are unique in the swarm, for example `api-owner`
  or `pi-worker-3`. A new agent gets a generated name if none is given. An
  agent can be a member of several channels, so the ID does not contain a
  channel name. (Call)
- The session record holds the harness session ID.

### Registration

Registration is automatic. Channel membership is opt-in. (Decided)

At session start, the harness adapter calls the host daemon. The daemon
decides which agent the session belongs to:

1. If `BORANT_AGENT` is set, the session belongs to that agent. The launcher
   sets this variable for spawned agents and for moves.
2. If the harness session ID already belongs to an agent, the session belongs
   to that agent. This covers a native resume on the same host, for example
   `claude --resume`.
3. Otherwise, the daemon creates a new agent.

The session then takes the agent lease. If another live session holds it, the
new session does not take it and runs without Borant tools, with a message to
the user. (Call) A retry of the registration by the session that already
holds the lease, because a write past the lease failed or its reply was lost,
takes its own lease back and keeps its fencing token.

If `BORANT_CHANNEL` is set, the session also joins that channel. Otherwise,
the agent joins later with the `channel_join` tool. (Decided)

At session end, the adapter tells the daemon, and the daemon releases the
lease. A crash or a sleep sends nothing, so the lease ends by its TTL. An end
that is not a park and leaves an `in_progress` claim on the agent (a shutdown,
a crash that the daemon reports, a kill) is not the agent's own exit after its
work: the hub places the agent again through the takeover path, and when no
host takes it, the claim goes to its owner as `stuck` with the reason.

### Working directory

Each agent has its own working directory in one git repository. (Call)

- The agent record stores the remote URL, the branch, and the path of the
  working directory relative to the repository root.
- `spawn` and a move create a new git worktree for the agent, under
  `~/.borant/work/{agent_id}/{repo}` on the host.
- If a session starts in a directory that another live agent already uses,
  the new agent registers as not movable. It gets messages and tasks, but no
  checkpoints and no moves.
- If a session starts outside a git repository, its agent is also not
  movable.

### Identity of MCP calls in Claude Code

Claude Code starts `borant mcp` as a child process. The MCP process learns
which session it serves from its parent PID. (Call, verified: see
`docs/spikes/claude-mcp-identity/`.)

- Hooks and MCP servers are both direct children of the Claude Code process.
- Hooks get `CLAUDE_PID`. The `SessionStart` hook reports it at registration.
- `borant mcp` walks its own ancestors and asks the host daemon for the
  session whose harness PID matches. Its own `CLAUDE_PID` variable is not
  reliable, because Claude Code passes an inherited value to MCP servers.
- Claude Code sets `CLAUDE_CODE_SESSION_ID` for hooks and MCP servers.
  `borant` uses it only as a fallback.

## 8. Channels and the task schema

- A session creates a channel with `channel_create`. Its agent becomes the
  owner. (Decided)
- The owner can declare a task schema at creation. Without one, the channel
  gets the default schema. (Decided)
- The schema is JSON Schema for the properties object of a task. (Call)
- The default schema has `branch` (string), `pr_url` (string), `summary`
  (string), and `files` (array of strings), and allows no other keys. (Call)
- The owner can extend the schema at any time. A change can only add keys. It
  cannot change the type of an existing key. (Call)
- The hub daemon validates each property write and rejects a wrong value with
  the reason. (Call)
- Ownership can pass to another agent with `channel_transfer`. (Call)

## 9. Tasks

### Task document

| Field | Content |
|---|---|
| `id` | ULID |
| `title`, `body` | Text |
| `status` | `open`, `in_progress`, `needs_human`, `stuck`, `closed` |
| `close_reason` | `done`, `wontfix`, `duplicate`, `superseded`, for a closed task |
| `close_message` | Text, for a closed task |
| `attention_msg` | A one-line reason for `needs_human` or `stuck` |
| `claimed_by` | Agent ID, or empty |
| `blocked_by` | Task IDs in the same channel |
| `parent` | Task ID, for roll-up only |
| `properties` | Object, validated against the task schema |
| `revision`, `created`, `updated` | Bookkeeping |

The model follows kata. (Decided) The status values and close reasons are a
call.

### Rules

- A task is ready when its status is `open` and every task in `blocked_by` is
  closed. Any close reason unblocks, as in kata. Ready is computed when a
  reader lists tasks and is never stored. (Call)
- Tasks that become ready at the same time form a stage and run in parallel.
  No stage object exists. (Decided)
- The hub daemon rejects an edge that makes a cycle.
- `parent` groups tasks. A parent cannot close while it has open children, as
  in kata. A parent does not block its children.
- A claim belongs to an agent, not to a session. It stays during a move. If no
  takeover succeeds within the claim timeout (default 30 minutes), the hub
  clears the claim and sets the task to `open`. (Call)
- `claim` fails if another agent holds the claim.
- Each change publishes an event on `borant.chan.{channel}.tasks`.

### Fan-out and join

The owner creates child tasks and lets workers claim them (fan-out). The owner
then waits for events (join). The owner does not poll, because events wake it.

## 10. Messages and delivery

### Subjects

| Subject | Content |
|---|---|
| `borant.agent.{agent_id}` | Direct messages and events for one agent |
| `borant.chan.{channel}.all` | Broadcasts in a channel |
| `borant.chan.{channel}.tasks` | Task events in a channel |
| `borant.ctl.*` | Requests from host daemons to the hub daemon |

(Call)

### Inbox

Each agent has one durable consumer. It reads the direct subject of the agent
and the broadcast and task subjects of each channel that it is a member of.
The consumer belongs to the agent, so a new session after a move gets every
message that is not acked. (Decided)

### Message envelope

Each message and event has an `id` (ULID), `from` (agent ID or `human`),
`to` (agent or channel), `kind` (`direct`, `broadcast`, or `event`), `text`,
an `urgent` flag, and a time. (Call)

### Delivery to a session

A host daemon follows the inbox of each agent that runs on the host. It hands
each message to the harness adapter of the session.

| Harness | Busy | Idle |
|---|---|---|
| Claude Code | `PostToolUse` hook (synchronous) gets waiting messages and returns `additionalContext` | `Stop` hook with `asyncRewake` waits on the daemon and exits with code 2 with the messages |
| Pi | Extension calls `pi.sendUserMessage` with `deliverAs: "steer"` | Extension calls `pi.sendUserMessage`, which starts a turn |
| omp | Extension calls `sendUserMessage` with `deliverAs: "aside"` and `attribution: "agent"` | Extension calls `sendUserMessage` with no `deliverAs` |

Rules (Call):

- The Claude Code waiter kills the earlier waiter of the same session before
  it waits, for example with a PID file for each session.
- The waiter sets a long `timeout`, for example one week. Values above 3600
  seconds are untested.
- In Pi, the extension never calls `sendUserMessage` without `deliverAs` while
  the agent is busy, because Pi then throws and drops the message.
- In omp, the extension connects only when `ctx.agent.kind === "main"`.

### Ack and duplicates

When the host daemon receives a message for an agent, it appends a
`swarm_message` event to the session log of the agent, and acks the message
only after that append succeeds. (Decided) The event records whether the
message reached the session yet. A later `delivered` event marks the
delivery. After a move, the new host reads the log and delivers the messages
that have no `delivered` event. Thus the bus never holds a message for a long
time while an agent is idle. (Call, changed during planning.) A message can
arrive twice. The daemon drops a message whose `id` is already in the session
log.

### Wake limits

Two agents can wake each other without end, and each wake costs a turn.
(Call)

- Direct messages wake the target.
- Broadcasts do not wake, unless `urgent` is set. They reach the agent at its
  next turn.
- Task events wake the owner only for `closed`, `stuck`, and `needs_human`.
- Each agent has a limit of wakes per hour, default 30. Over the limit, the
  messages wait for the next turn, and the daemon sends one event to the
  owner.

### Humans

The user sends messages with `borant send` from any host, with `from` set to
`human`. The notifier tells the user about `needs_human` and `stuck` tasks.
(Call)

## 11. Leases, fencing, and presence

All defaults in this section are calls.

- The host daemon sends one heartbeat every 10 seconds for all its live
  sessions. The heartbeat renews presence and the agent leases. The daemon
  checks that each session is alive by its harness PID. The harness adapters
  send no heartbeats. (Call)
- The agent lease lasts 30 seconds. The hub daemon decides when a lease ends,
  on the hub clock.
- Each new lease gets a higher fencing token. Each control request carries the
  token, and the hub daemon rejects an older token.
- If a host daemon cannot renew a lease before it ends, for example during a
  network outage, it pauses the session. A `PreToolUse` hook (Claude Code) or a
  tool hook (Pi, omp) blocks all tools and tells the agent why.
- A host cannot tell "the hub is down" from "only this host is cut off". Thus
  the same pause rule applies during a hub outage or a hub move: sessions work
  until the lease ends, then pause. This prevents two live sessions of one
  agent. (Call)
- The hub never starts a takeover before the lease and the grace period end.
  When a hub daemon starts, for example after a hub move, it gives every lease
  a new expiry from its start time. Thus a hub restart never causes a mass
  takeover. When the hub returns, paused sessions renew their leases and
  continue. (Call)
- When a host returns and finds that its session lost the lease, the daemon
  keeps the tools blocked and shows: "This agent moved to <host>. Run `borant
  move <agent> --here` to take it back."

## 12. Session data

### Session log

The session log is a neutral, append-only log for each agent in the session
store. (Decided) It keeps only the last N turns of the agent, default 50.
(Decided: last N turns. Call: the default.)

Each event has `event_id`, `agent`, `session_id`, `harness`, `host`,
`turn_id`, a sequence number, and a time. Event kinds:

| Kind | Content | Claude Code source | Pi and omp source |
|---|---|---|---|
| `user_prompt` | Text from a human | `UserPromptSubmit` | Message events |
| `swarm_message` | Message ID, sender, text | The hook that delivers it | The extension that delivers it |
| `assistant_message` | Text | `Stop` (`last_assistant_message`) | Message events |
| `tool_call`, `tool_result` | Tool name, input, output, error flag | `PostToolUse` | Tool events |
| `checkpoint` | WIP ref and commit SHA | `PostToolUse`, `Stop` | Tool and turn events |
| `session_start`, `session_end`, `move` | Hosts, harness, reason | Hooks and the daemon | Extension and the daemon |

Rules (Call):

- Tool input and output are cut at 8 KB for each event.
- The host daemon removes text that matches known secret patterns before the
  upload, for example API keys and tokens.
- JetStream encryption at rest is on for the hub.
- Claude Code hooks do not show the assistant text between tool calls, or the
  thinking. This gap is accepted.

### Checkpoints of the working tree

After each `PostToolUse` and at `Stop`, the adapter asks the host daemon for a
checkpoint (Call):

1. If the working tree did not change since the last checkpoint, do nothing.
2. Make a commit object of the whole working tree, including untracked files
   that git does not ignore. Use a temporary index (`GIT_INDEX_FILE`), `git
   add -A`, `git write-tree`, and `git commit-tree` with `HEAD` as the parent.
   This does not change the branch, the real index, or the files. Plain `git
   stash create` is not enough, because it drops untracked files.
3. Push it to `refs/borant/{agent_id}/wip` on the checkpoint remote. The
   checkpoint remote is a host configuration value. It defaults to the
   `origin` remote of the repository.
4. Append a `checkpoint` event with the ref and the SHA.

The log position and the WIP ref come from the same tool boundary, so they
match. A push to a custom ref on GitHub is unverified. If it fails, set the
checkpoint remote to a bare repository that all hosts can reach, for example
on the hub. Custom refs do not start GitHub Actions workflows, but branches
do, so the WIP ref must not be a branch. If the working directory is not a git
repository, the session runs with no tree checkpoints and a warning.

## 13. Moves

### Kinds

| Kind | Start | Loss |
|---|---|---|
| Planned move (`borant move`, `borant drain`) | The old session finishes its turn. The daemon writes a final checkpoint and releases the lease. | Nothing |
| Takeover | The agent lease ended, and the grace period passed | Work after the last checkpoint: at most one turn. This is acceptable. (Decided) |

### Takeover steps

1. The lease of the agent ends, because its host sent no heartbeat.
2. The hub waits for the grace period, default 2 minutes. A short sleep or a
   network drop does not start a move. (Call)
3. The scheduler picks a host (section 14).
4. The hub gives the new session a new lease and a higher fencing token.
5. The launcher on the new host fetches the repository and the WIP ref. It
   creates the worktree of the agent at the parent commit of the WIP commit,
   on the branch of the agent, and then restores the files of the WIP commit
   without a new commit.
6. The launcher starts a new session of the harness with `BORANT_AGENT` set
   and a first prompt: "You continue agent X. Read your history with the
   `session_history` tool, then read your tasks, then continue."
7. The new session reads its inbox. Messages that were not acked arrive.

The new session does not import the native transcript. It reads the neutral
session log through `session_history`, which returns pages of events. The
agent writes its own summary with the model of its own harness. The host
daemon makes no model calls. (Decided: neutral log and injected history. Call:
the agent reads the history itself.)

### Rules

- Any agent can move, owner or worker. (Decided)
- Only agents that are members of at least one channel move automatically.
  Other agents stay on their host. (Decided)
- An idle agent with no claims and an empty inbox does not move at once. It
  moves when a message arrives for it. (Decided)
- The agent can move to a different harness, but the scheduler prefers the
  same harness, unless the user asks for another harness. (Decided) When the
  harness changes, the first prompt says so.
- An agent does not move back on its own when the old host returns. The user
  decides with `borant move <agent> --here`. (Decided)
- A move always starts a new session. Borant never reuses an old session that
  lost its lease. When the old host returns, its daemon saves the changes of
  the lost turn to `refs/borant/{agent_id}/stale/{time}` and then ends the old
  session. A later `borant move <agent> --here` is a normal planned move.
  (Call)
- `borant drain <host>` moves all agents away from a host as planned moves.

### Example

The user has three Claude Code owners for three channels on laptop A, and Pi
workers on laptop A and on VPS 2. The user closes laptop A and walks to work:

1. The leases of all agents on laptop A end.
2. After the grace period, the scheduler places each agent on its own. Two
   owners can land on VPS 1, and the third on VPS 2.
3. The Pi workers on VPS 2 continue. Their messages wait in the inboxes of the
   owners until each owner starts again.
4. At work, the user runs `borant move --from vps-1 --here` on laptop B. The
   owners open in Orca on laptop B.

## 14. Placement

The scheduler runs in the hub daemon. It places new agents and moved agents.
(Decided: any host, with scheduling. Call: the rules.)

Filter. A host qualifies if all of these are true:

- It is online and not in drain state.
- It is below its `max_agents` limit.
- It has at least one harness that is installed and logged in.
- It can reach the git remote of the repository. If the repository is not on
  the host, the launcher clones it.

Score, in this order:

1. The same harness as the agent wins.
2. Fewer active agents win.
3. An always-on host wins.

`borant move <agent> --host <host>` or `--harness <harness>` overrides the
score. The user sets up each host by hand with the same logins, tools, and
secrets. The host daemon advertises what it has, and the scheduler only checks
it. (Call)

## 15. Launchers and attach

The launcher is a port. Each host picks its adapter in its configuration.
(Decided: tmux or zellij on remote hosts, Orca on laptops.)

| Adapter | Use | Start | Attach |
|---|---|---|---|
| Orca | Laptops | `orca` CLI opens a terminal and starts the harness | The user sees it in Orca |
| tmux | Remote hosts | A named tmux session for each agent | `borant attach <agent>` over SSH |
| zellij | Remote hosts | A named zellij session for each agent | `borant attach <agent>` over SSH |
| Headless | Hosts with no terminal | `claude -p` or Pi RPC mode | No attach. Messages only. |

`borant attach <agent>` finds the current host of the agent and attaches to
its terminal session over SSH. (Call)

The launcher also starts new workers when an owner calls `spawn`. The new
session gets `BORANT_AGENT`, `BORANT_CHANNEL`, and a first prompt.

Every session that the launcher starts, for `spawn` or for a move, runs with
all permission prompts off, as Orca does. For Claude Code, this is
`--dangerously-skip-permissions`. Pi and omp use their own equivalent mode.
Nobody watches these sessions, and a prompt stops them with no error.
(Decided)

## 16. Hub

- The hub daemon holds `lease.hub`. A host that the user marks as able to be
  the hub can take it. (Call)
- Hosts reach the hub through a stable DNS name or a list of URLs in the host
  configuration. The NATS client reconnects by itself. (Call)
- While the hub is unreachable, each host daemon keeps outbound messages and
  session-log appends in a local outbox and sends them later. The outbox is a
  SQLite file in the Borant home folder of the host. It does not depend on
  the broker. (Call, changed after the spikes.)
- Control requests, such as `task_claim` or `task_create`, do not go into the
  outbox. They fail at once with "hub unavailable", and the agent can try
  again later. Thus an agent never starts work on a claim that can lose later.
  (Call)
- A cold move has three steps. (Decided: cold move only. Call: the steps.)
  1. `borant hub export --out <file>` on the old hub puts the hub in
     maintenance mode and writes all state keys, all session logs, and the
     messages that each inbox did not ack yet to one JSONL file. The export
     goes through the ports, so it works for any adapter.
  2. Copy the file to the new hub host and run `borant hub import <file>`
     there, against an empty broker.
  3. Start `borant hub` on the new host and change the DNS name or the URL
     list of the hosts. The new hub takes `lease.hub`.
  `borant hub move <ssh-host>` runs these three steps over SSH. Downtime is a
  few minutes.
- The hub is a single point of failure in v1. If it goes down, messages wait
  in the outboxes, task changes fail, and sessions pause when their leases end
  (section 11).

## 17. Interfaces

### Tools for agents

Claude Code gets these through `borant mcp`. Pi and omp get them through
`pi.registerTool()` in the extension. The names are a call.

| Tool | Purpose |
|---|---|
| `whoami` | Agent ID, session, host, channels |
| `channel_create`, `channel_join`, `channel_leave`, `channel_list`, `channel_transfer` | Channels |
| `members` | Members and their presence |
| `send` | A direct message or a broadcast |
| `messages` | Recent messages in a channel |
| `task_create`, `task_get`, `task_list`, `task_update`, `task_claim`, `task_close` | Tasks. `task_list` can filter to ready tasks. |
| `schema_get`, `schema_extend` | The task schema |
| `spawn` | Start a worker through the scheduler |
| `session_history` | Pages of the session log of this agent |

### CLI for the user

`borant status`, `borant send`, `borant move`, `borant drain`, `borant
attach`, `borant hub move`, `borant hosts`, `borant agents`, `borant tasks`,
plus the daemon roles. The CLI and the tools call the same local API.

### Harness adapters

- Claude Code: a plugin with `hooks/hooks.json` and an MCP entry for `borant
  mcp`. Hooks: `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
  `PreToolUse` (fencing), `PostToolUse` (delivery, log, checkpoint), and
  `Stop` (log, checkpoint, `asyncRewake` waiter). Each hook calls `borant hook
  <event>`.
- Pi and omp: one TypeScript extension for both, installed in
  `~/.pi/agent/extensions/` and `~/.omp/agent/extensions/`. It opens the SSE
  stream on `session_start` and closes it on `session_shutdown`.

## 18. Security and trust

- Every agent in a swarm trusts every other agent, because a message arrives
  as a prompt. (Call)
- Launched sessions run with permission prompts off (section 15). Thus a
  message from any agent can make an agent run any command on any host. The
  user accepts this risk for v1. (Decided)
- Each host has its own NATS client credentials and connects over TLS. The
  credentials decide which hosts can join the swarm.
- The local API is a Unix socket with mode 0600.
- Session logs pass through secret redaction, and the hub encrypts JetStream
  data at rest.

## 19. Retention

- Session logs: the last N turns for each agent (Decided). The logs of agents
  that have had no session for 30 days are deleted. (Call)
- Message streams: an age limit, default 30 days. (Call)
- A closed channel and its tasks go to an archive after 30 days. (Call)

## 20. Known limits and risks

| Item | Effect |
|---|---|
| The hub is a single point of failure | While the hub is down, messages wait, task changes fail, and sessions pause after their leases end |
| Write then publish without an outbox | A crash of the hub daemon between the two loses one event |
| The neutral log lacks the thinking and the text between tool calls in Claude Code | A takeover has less context than the old session |
| `asyncRewake` is lightly documented | A Claude Code release can change it. The spike is the only proof. |
| Launched sessions bypass permissions | A bad or confused message can run any command on any host |
| Host parity is manual | A takeover fails on a host that lacks a login or a secret |
| Process ancestry for MCP identity is unverified | `borant mcp` may need another way to learn its session |
| Custom refs on GitHub are unverified | Tree checkpoints may need the hub fallback |

## 21. Left to the builder

- The crate layout and the exact trait functions of the message bus, the
  session store, the launcher, and the notifier
- The REST paths and the SSE event format
- The exact MCP tool schemas
- The configuration file format for hosts and the hub
- The NATS stream and consumer settings
- The secret patterns for redaction
- The test strategy, within the rule that only focused tests run outside CI

## 22. First things to verify

The builder must test these before the related parts are built:

| Item | Result on 2026-10-24 |
|---|---|
| `borant mcp` can learn its Claude Code session | Verified. Hooks and MCP servers are direct children of the Claude Code process. See section 7. |
| An `asyncRewake` waiter with a `timeout` of one week stays alive past 3600 seconds | Verified. The waiter was alive after 62 minutes and woke the session. See `docs/spikes/claude-asyncrewake/README.md`. |
| A push of a checkpoint commit to `refs/borant/...` on GitHub works | Not tested. A test needs a new GitHub repository, and the designer did not create one without the user. The checkpoint remote is configurable. |
| A Pi and omp extension can hold an SSE stream on a Unix socket and wake an idle session | Verified in Pi 1.4.0 and omp 18.12.6, from idle and while busy. See `docs/spikes/pi-sse-wake/`. |

## 23. Changes after the spikes and during planning

Each line is a decision record.

- Decision: no NATS leaf nodes. Host daemons connect to the hub NATS server
  as clients. Evidence: the local outbox in the host daemon covers hub
  outages, and leaf nodes then add one process on each host and give nothing
  more. Reversible: yes, a leaf node can come later without a change to the
  ports.
- Decision: the outbox is a SQLite file in the host daemon. Evidence: it
  works with any broker adapter. Reversible: yes.
- Decision: no TTL in the state store port. Evidence: the hub decides lease
  expiry on its own clock, and presence comes from the lease. Reversible:
  yes.
- Decision: the host daemon sends the heartbeats and checks the harness PID.
  Evidence: the PID of each session is known from registration. Reversible:
  yes.
- Decision: the host daemon acks a message after it logs the message on
  receipt, not after delivery. Evidence: an idle agent can wait hours for a
  broadcast, and the bus then redelivers unacked messages. Reversible: yes.
- Decision: the checkpoint remote is configurable. Evidence: the GitHub test
  needs a new repository on the user account. Reversible: yes.
- Decision: the hub move uses an export file through the ports. Evidence:
  async-nats has no stream snapshot API, and an export through the ports
  works for every adapter. Reversible: yes.
- Decision: the Claude Code idle waiter uses a long poll, and the Pi and omp
  extension uses SSE. Evidence: the waiter must exit to wake Claude, and an
  SSE stream fits a process that stays alive. Reversible: yes.
- Decision: the multi-host tests in Docker use a fake harness. Evidence: real
  harnesses need logins that a container does not have. The real Claude
  Code, Pi, and omp adapters get tests on the Mac. Reversible: yes.

### Decisions during the build

- Decision: the host daemon saves its local session table to
  `$BORANT_HOME/sessions.json` and restores live sessions at start.
  Evidence: a daemon restart forgot live sessions, and their leases then
  expired. Reversible: yes.
- Decision: the Claude Code idle waiter also starts at `SessionStart`.
  Evidence: a session that never had a turn had no waiter and did not wake.
  Reversible: yes.
- Decision: `borant host` refuses to start when another daemon answers on its
  socket. Evidence: a second daemon deleted the socket of the first one.
  Reversible: yes.
- Decision: the wake-limit notice goes to the agent that sent the message
  over the limit, not to a channel owner. Evidence: a direct message has no
  channel, and the sender is the agent that causes the wakes. Reversible:
  yes.
- Decision: `spawn` is an agent tool only, not a CLI command. Evidence: a
  spawn copies the repository of the calling agent, and a human call has no
  agent. Reversible: yes.
- Decision: the `messages` tool is out of v1. Evidence: the bus port has no
  history read. Reversible: yes.
- Decision: the `move` session event names the new harness `to_harness`.
  Evidence: the name `harness` clashed with the field of the outer event, and
  those events failed to parse. Reversible: no, it is a log format change
  before the first release.
