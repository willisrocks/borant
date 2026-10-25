# Durable orchestration vision for Borant

Date: 2026-10-24. Status: draft for review. Track 5 of 5.

This document collects ideas that can make Borant the one durable
orchestration layer that the user runs all agent work on. It rates each idea
and ends with a short roadmap. It does not change the v1 design in
`docs/superpowers/specs/2026-10-24-borant-design.md`. Section numbers in the
form "design section 16" point to that document.

## 1. Summary

Borant already holds most parts of a durable execution engine. The inbox is
durable, the task graph is durable, leases and fencing tokens stop double
work, and checkpoints let an agent continue on another host. The gaps are
not a missing workflow engine. They are a few small rules in the hub: time,
idempotency, humans, limits, and outside events.

The top picks, in order:

1. Hub backup and restore. Today a lost hub host loses all control state,
   because the cold move needs the old hub to be alive.
2. Durable primitives: idempotency keys, task deadlines, stall detection,
   retry counts, and agent waits ("wake me when X or at time T").
3. Governors: a kill switch, a pause for a channel, and caps on agents,
   turns, and session hours. Dollar caps come later.
4. A human inbox with phone answers through ntfy.
5. Close gates: a test command that must pass before `task_close`.
6. A GitHub PR watch that turns CI results and reviews into task events.

The ideas that this document rejects are in section 12, with reasons.

## 2. Related tracks

Four other documents cover topics that this vision depends on. Their file
names follow the pattern of this document. If a sibling document has a
different name, correct the names here.

| Track | Expected document | What this vision takes from it |
|---|---|---|
| 1 | `2026-10-24-secrets-and-host-bootstrap-design.md` | Tokens for GitHub, ntfy, and Slack. Host parity for takeovers. Secrets never in logs. |
| 2 | `2026-10-24-elastic-compute-design.md` | Hosts that start on demand. The price of a third small VPS for a NATS cluster. |
| 3 | `2026-10-24-orchestration-api-and-sandboxes-design.md` | A spawn API outside agent tools, a model for each spawn, placement rules. |
| 4 | `2026-10-24-observability-design.md` | Cost data for each agent, the timeline, the dashboard, alerts. |

## 3. Principles for every idea

These rules come from the v1 design and from the prior art in section 4.
Each verdict in this document applies them.

1. The hub stays the single writer. A bridge, a timer, or a gate sends a
   control request like a host daemon does. It never writes a key.
2. A rule that the hub can apply must not cost the owner a model turn. This
   is an anti-goal of the v1 design. Deadlines, gates, and CI events are hub
   or host rules, and they wake an agent only with a result.
3. No new always-on service if the hub tick or a host daemon can do the
   work. The hub already runs a loop each second
   (`crates/borant-hub/src/lib.rs`, the `interval` of one second).
4. Borrow patterns, not engines. Temporal, Restate, and Inngest solve
   durable code. In Borant the owner agent is the code, and the task graph
   is its durable state.
5. One person runs this. An idea that needs a team to operate it is a skip.

## 4. Prior art

| Source | What it does | What Borant takes | What Borant skips |
|---|---|---|---|
| Temporal [1][2] | Durable workflows. Signals bring a human decision into a workflow. Durable timers wait for days and use no compute. Schedules have overlap policies such as Skip and BufferOne. | Durable timers in the hub. Signals as messages to an agent. The overlap policies for recurring work. | The Temporal server and its database. Deterministic workflow code. |
| Restate [3] | A log of each step, replay after a crash, durable waits for a human with timeouts. | The idea that a wait is a stored record with a timeout. | The Restate server. A Rust SDK exists for Restate, but this document did not confirm the details. |
| DBOS [4] | Workflows and queues in Postgres, with timeouts, rate limits, and deduplication of queue items. | Idempotency keys. The proof that one database can be enough. | A second store. Borant already has one. |
| Inngest [5] | `step.waitForEvent` with a timeout and a match key. A timeout returns `null`, and the docs say that you must handle it. | The "wait" primitive in section 6, and the rule that every wait has a timeout. | A hosted function runtime. |
| LangGraph [6] | Checkpoints of graph state, interrupts for a human, and replay. Steps after the checkpoint run again, with new LLM results. | The warning: a replay of agent work does not give the same result. | Replay as a feature (section 12). |
| Claude Code agent teams [7] | A lead, teammates, a shared task list with dependencies, and mailboxes. `TaskCompleted` and `TeammateIdle` hooks can block completion. One team for each session, no resume of teammates, one machine. | The close gate (a hook that blocks completion). | Nothing. Borant already covers the multi-host, multi-harness case that teams do not. |
| Claude Code cross-session messaging [8] | Messages between sessions of one user, across machines through Remote Control. A message from another session can never approve a permission prompt. | The rule that an agent can never give a human approval (section 8). | It is Claude only. It holds messages for approval when the receiver skips permissions. |
| Claude Code channels [9] | An MCP server that pushes events, such as CI results or chat, into a live session. Research preview. Claude only. | The idea of outside events as messages. | A Claude-only path. Borant bridges go through the inbox, so every harness gets them. |
| GitHub Copilot coding agent [10] | Assign an issue, get a branch and a draft PR, work in GitHub Actions, then a review request. | Issue in, PR out, as the shape of the GitHub bridge. | Runs in GitHub Actions only. |
| OpenHands resolver [11] | A `fix-me` label starts an agent. It opens a draft PR or pushes a branch and comments with the result. | A label as the trigger for an issue import. | A new agent for each issue. |
| Codex cloud [12] | Cloud tasks in containers. Best-of-N runs up to four attempts. (Third-party sources only.) | Best-of-N as a later option. | The cost of N attempts for each task. |
| Linear Agents API [13] | Delegate an issue to an app. Sessions, activities (`thought`, `action`, `elicitation`, `response`, `error`). Developer preview. | The session and activity shape for a later Linear bridge. | It needs an OAuth app and a public webhook URL. |
| Omnara [14], Claude Remote Control [8] | Phone approvals and push notices for live sessions. | Phone answers for `needs_human`. | Control of one session. Borant needs one inbox for the whole swarm. |
| Sculptor [15], Conductor [16], Terragon [17] | Parallel agents in containers, worktrees, or microVMs, with a UI. | Nothing new. Track 3 covers sandboxes. | Their UIs. |
| Pi Durable [18] | An experimental Pi package for agents that survive crashes and dropped clients. (Secondhand sources only.) | Nothing new. | Pi only, as the v1 design already said. |
| Ralph loop [19] | A `Stop` hook re-feeds the prompt until the output has a completion phrase, with an iteration cap. | "Run until done" with a cap, for every harness (section 6). | A loop with no external test of "done". |
| Multi-agent research [20][21][22] | Anthropic: parallel subagents help on research that splits well, at about 15 times the tokens. Cognition: agents that cannot see each other's context make clashing work. MAST: many failures come from system design and missing verification. | Close gates and cross-model review. Channel notes for shared decisions. | Large teams on coding work that is tightly linked. |
| LLM judges [23][24] | Judges prefer their own outputs and outputs of their own model family. | Review by a different model family (Claude reviews DeepSeek work). | Self-review as the only gate. |

## 5. Hub high availability

Today the hub is a single point of failure (design section 16). A hub move
is cold: the old hub exports to a file, and the new hub imports it. The
export needs the old hub to be alive. If the hub host dies, or its disk is
lost, no path brings back the channels, tasks, and leases.

### Options

| Option | How it works | Protects against | Cost | Size | Verdict |
|---|---|---|---|---|---|
| A. Hub backup and restore | A timer runs a snapshot export every 5 minutes and copies it to the second VPS. `borant hub restore` imports the newest snapshot on a new host. | Loss of the hub host or its disk | One timer, one new flag on export, one host-side replay rule | S | Do next |
| B. Standby hub daemon on the same NATS server | A second hub daemon waits for `lease.hub` to end. | A crash of the hub daemon only. systemd already restarts it. | Low | S | Skip |
| C. NATS cluster of three servers, replicas R3 | Three NATS servers form a Raft group. Streams and KV keep three copies. A hub daemon runs next to each server and takes `lease.hub` by compare-and-set when it ends. | Loss of any one server, with no human action | A third always-on machine, cluster TLS and routes, a slower write (majority ack), Docker tests with three servers | M (code S, operations M) | Later |
| D. Laptop-only mode | One command starts `nats-server`, the hub, and a host on one laptop. | Work with no VPS, for example on a plane | Mostly documentation | S | Later |

### Why the backup comes first

NATS needs a majority to write. With two servers, the loss of one server
stops all writes, so a cluster of the two current VPS machines gives no
gain [25]. A laptop cannot be the third server, because it sleeps. Option C
thus needs a third VPS, and track 2 owns its price.

Option A covers the real risk for a small fleet with a few lines of code.
It has one hard problem. Messages and session-log appends that reached the
dead hub after the last snapshot are not in the snapshot. A lost message
with no error is an anti-goal of the v1 design. The fix uses parts that
exist:

1. Each host daemon keeps each message and session-log append that it sent
   in its SQLite outbox for one hour after the send, not only until the
   send.
2. Each hub writes a new instance ID at start. When a host sees a new
   instance ID with an older snapshot time, it sends its last hour again.
3. Receivers already drop a message whose `id` is in the session log, so
   the second send makes no duplicates.
4. Control writes after the snapshot, for example a task close, are lost.
   The restored hub broadcasts one event to every channel: "The hub was
   restored to time T. Changes after T are lost. Read your tasks again."

The snapshot must not use maintenance mode, because maintenance stops all
work. The hub is the single writer, so it can hold its request loop for the
time that it reads the control keys. That gives a consistent copy of the
control plane. Session logs and inboxes are append-only, and step 1
repairs any gap.

### When to build the cluster

Build option C when one of these is true:

- A hub outage of 15 minutes costs real work, for example during a night
  run with no human awake.
- Track 2 adds hosts that start on demand, and the hub then also runs the
  provisioner.

Option C needs few code changes. The election already exists in
`lease.hub` with a fencing token. A hub on the minority side of a split
cannot write, because its NATS server has no quorum. The quorum is thus the
fence for the old hub. The cost is operations: one more machine and one
more failure mode to learn.

## 6. Durable work above the task graph

The owner agent plans and the task graph holds the plan. What the hub lacks
is time and memory of requests. This section adds both as data in the
state store, so a hub restart or a hub restore keeps them.

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Idempotency keys | A retry after "hub unavailable" does not make a second task or message. Bridges and schedules can retry safely. | An optional `idem` field on `CtlRequest::Call`. The hub keeps `idem.{key}` with the reply for 24 hours and returns the stored reply for a repeat. | S | None | Do next |
| Task deadlines | Work that runs too long becomes visible without a human looking. | A `deadline` field on a task. The hub tick sets `stuck` with `attention_msg` "deadline passed". The owner already wakes on `stuck`. | S | None | Do next |
| Stall detection | A worker that is alive but makes no progress gets noticed. | Host heartbeats add the time of the last session-log event for each session. The hub sets `stuck` after `stall_after` (default 20 minutes) with no event. | S | None | Do next |
| Retry count | A task that fails three times goes to a human, not to a fourth worker. | `attempt` and `max_attempts` on a task. A claim timeout or a reopen adds one. At the limit, the hub sets `needs_human`. | S | None | Do next |
| Agent waits | An agent can stop and wake later on an event or a time, at no token cost. Example: "wake me when task X closes or in 2 hours". | A new tool `wait` with a match (task event, PR event, message from an agent) and a required timeout. The hub stores `wait.{agent}.{id}`. On a match or a timeout, it sends a direct message, which wakes the agent. The inbox belongs to the agent, so a wait survives a move. | S to M | Idempotency keys | Do next |
| Schedules (recurring work) | Nightly or hourly work with no human: a morning report, a dependency update, a review of nightly test results. | `sched.{name}` holds a cron rule, a time zone, an overlap policy (`skip` or `buffer_one`, from Temporal), and a target: a task from a template in a channel, or a message to an agent. The hub tick fires it. The idempotency key is the name plus the fire time, so a hub restart never fires twice. | M | Idempotency keys, deadlines, track 3 for spawns | Later. First item after the roadmap. See question 2. |
| Run until done | A goal keeps going across turns, harnesses, and moves until a test passes or a cap is hit. | Per agent: `until` (a close gate or "all claimed tasks closed") and `max_continues`. When the agent goes idle and `until` is false, the host daemon sends "continue". This is the Ralph loop for every harness, with a real test. | S | Close gates, governors | Later. It is unsafe before governors exist. |
| Pause and resume of a channel | Stop a whole swarm without losing its state, for example over the weekend. | See governors in section 7. A pause ends each session with a final checkpoint and keeps claims and inboxes. A resume starts the agents again from their history. | S | None | Do next, as part of governors |
| Checkpoints of a plan | Go back to an earlier task graph. | The graph is already durable in KV, and the export can write one channel. | S | None | Skip. No use case beyond the export. |
| Compensation (saga style) | Undo the effects of failed work. | The effects of coding work are git commits and PRs. An agent or a human reverts them. | M | None | Skip |
| Fix the lost event after a write | Timers and bridges depend on events, so a lost event matters more. | At start, the hub compares each task revision with the last event for it and publishes the missing events. | S | None | Later |

## 7. Budgets and governors

Launched sessions run with all permission prompts off (design section 15).
Thus a confused owner can spawn workers with no end, and nothing stops it.
A cap in the hub is the only brake. Most caps need no cost data, so they
can come before track 4.

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Kill switch | One command stops an agent, a channel, or the whole swarm at once. | `borant stop <agent\|#channel\|--all>`. The hub sends `HostCommand::Fence`, which exists, and marks the docs as paused. Claims stay. Wakes wait. | S | None | Do next |
| Pause and resume | Stop a swarm for hours with no token cost. | A pause is a planned move to nowhere: final checkpoint, end session, keep claims and inbox. `borant resume` places the agents again. | S | Kill switch | Do next |
| Caps with no cost data | A runaway loop stops by itself. | Per channel: `max_agents`. Per agent: `max_turns_per_day` and `max_session_hours`. Host heartbeats add the turn count. At a cap, the hub pauses the agent and puts an item in the human inbox. | S | None | Do next |
| Rules for workers | A worker cannot spawn or create channels unless the owner allows it. | A role rule in the hub for `spawn`, `channel_create`, and `channel_transfer`. | S | None | Later |
| Dollar caps for each channel and agent | A hard limit on API spend. | Track 4 gives spend for each agent. The hub pauses at a soft cap. A proxy enforces the hard cap: LiteLLM has `max_budget` for each virtual key [26]. LunaRoute records usage [27], but this document did not find a budget feature in it. Claude Code has `--max-budget-usd` in print mode only (third-party sources [28]). | M | Track 4, track 1 for a key for each agent | Later. See question 3. |
| Model routing rules | Cheap models for workers, a strong model for owners and reviewers. | A rule in the channel: harness and model for each role. The spawn API applies it. | S | Track 3 | Later. Track 3 owns the model for each spawn. |

## 8. Human in the loop from anywhere

Today a human hears about a `needs_human` task through a macOS
notification, and answers with `borant send`. Both need a laptop. This
section makes `human` a member with an inbox, and adds a phone path.

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Human inbox | One list of every question, approval, and stuck task in the swarm. | A durable consumer for the agent ID `human`. `borant inbox` lists open items. `borant answer <task> <text>` sets the task to `in_progress` and sends a direct message to the claimer. | S | None | Do next |
| ntfy notifier | Answers from the phone with one tap. | A new Notifier adapter. The port gets an optional list of actions. ntfy supports up to three buttons, and an `http` button sends a POST from the phone [29]. The hub host runs ntfy, or uses ntfy.sh with a secret topic. | S | Track 1 for the ntfy token | Do next |
| Answer endpoint | The target of the phone buttons, and a one-page form for a free-text answer. | A small HTTP role, `borant bridge http`, on the hub host. It listens on the Tailscale address only, because the phone is on the tailnet. It turns a POST into a control call with actor `human`. | S | Track 1 for the token in the URL | Do next, with ntfy. If track 4 builds a dashboard, the form goes there. |
| Escalation rules | A question with no answer does not block a swarm all night. | Per item: `remind_after`, `escalate_after`, and `on_timeout` (`wait`, `default:<answer>`, or `close:wontfix`). The timers of section 6 run them. | S | Deadlines, human inbox | Later. First version: one reminder only. |
| Approval gates | A human must approve before a risky step, for example a merge or a deploy. | A task property `needs_approval`. The hub refuses `task_close` until a human call approves it. An agent can never approve, the same rule as Claude Code [8]. Local callers can still act as `human`, so this stops confusion, not malice. | S | Human inbox | Later |
| Slack bridge | Questions in a Slack thread, with free-text answers in the reply. | Socket Mode needs no public URL [30]. One thread for each inbox item. A reply in the thread is an answer. | M | Track 1 for the app tokens | Later. See question 4. |
| Slack channels mirror Borant channels | Watch a swarm in Slack. | A mirror of broadcasts. | M | Slack bridge | Skip. It is noise. The dashboard of track 4 serves this need. |

## 9. Integration bridges

A bridge is a process that is not an agent. It reads an outside system and
sends control calls with an actor such as `bridge:github`. It uses
idempotency keys, so a restart never makes duplicates. The fleet is on
Tailscale only, so bridges poll. A webhook needs a public URL, for example
Tailscale Funnel [31], and then signature checks on each request.

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| GitHub PR watch | A worker opens a PR. CI fails. The worker wakes with the failed job names and fixes them. No agent polls, and the owner spends no turn. | `borant bridge github` on an always-on host. It reads tasks that have `pr_url`, which is in the default schema. Every 60 seconds it reads `check_runs`, review state, and merge state with the GitHub API. It sends events to the claimer: checks failed, checks passed, changes requested. On merge, it can close the task with `done`. Idempotency key: PR, head SHA, state. | M | Idempotency keys, track 1 for a GitHub token | Do next |
| GitHub issues as a task source | Label an issue `borant`, and it becomes a task in a mapped channel. This is the Copilot and OpenHands shape [10][11]. | The same bridge. One task for each labeled issue. A close of the task comments on the issue. | S after PR watch | PR watch | Later |
| One PR for each task | A clean review unit for each piece of work. | A prompt rule for workers and the `branch` and `pr_url` properties. The close gate can ask for a `pr_url`. | S | Close gates | Later, as a channel template |
| kata sync | Run a kata epic on Borant workers across hosts. | One-way import: a kata epic becomes a channel, and its cards become tasks with `blocked_by`. On close, the bridge writes the status back to kata. kata stays the source of truth. | M | Idempotency keys | Later. See question 5. |
| Linear sync | Delegate a Linear issue to Borant. | The Linear Agents API fits well [13]. It needs an OAuth app in the work Linear workspace and a public webhook URL. | M | Track 1, Funnel | Later. See question 6. |
| Two-way sync of any tracker | Edit a task in either system. | Two writers for one record. | L | All | Skip. Conflicts with no clear winner. |

## 10. Quality loops

Cheap DeepSeek workers make more mistakes than a strong model. A test that
the hub enforces costs no tokens and catches the most common failure that
the MAST study names: work that nobody verified [22].

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Close gates | A task cannot close as `done` while its tests fail. | A gate command in the channel or a `gate` property on a task, for example `just test-crate borant-hub tasks`. At `task_close`, the host daemon of the claimer runs it in the worktree of the agent, with a timeout of 10 minutes. It sends the exit code, the HEAD SHA, and the last lines of output with the close request. The hub stores the result. A failure refuses the close and returns the output to the agent. Only a human can skip a gate. | S | None | Do next |
| Cross-harness review | Claude reviews DeepSeek work. Judges favor their own model family, so the reviewer must be a different family [23][24]. | First as a channel recipe with no code: the owner creates a review task for each implement task and spawns a reviewer in the other harness. Later the hub can create the review task by rule. | S as a recipe, M in the hub | Track 3 for a model for each spawn | Later. Start the recipe now. |
| Evals of worker output | Data for model routing: which model passes gates on the first try for which kind of task. | Count gate passes, reopens, and review findings for each harness, model, and task kind. Track 4 shows them. | M | Close gates, track 4 | Later |
| Best-of-N | Several attempts on one hard task, and the gate picks the winner. Codex cloud does this with up to four attempts [12]. | Spawn N workers on copies of one task. | M | Track 2, track 3, dollar caps | Later. It multiplies cost. |

## 11. Memory, mobility, and safety

### Memory and knowledge

The repository is the long-term memory: code, `AGENTS.md`, and docs.
Borant adds only the part that the repository cannot hold: shared state
that a swarm needs during the work.

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Channel notes | A short list of decisions that every member sees, also a new worker or an owner after a move. Cognition shows why agents need shared decisions [21]. | `chan.{channel}.notes` in KV, written through the hub with a size limit of 8 KB. The first prompt of a spawn or a move includes it. | S | None | Later |
| Artifact store | Screenshots, QA reports, and logs that tasks make and that do not belong in git. | A new port, ArtifactStore. The NATS adapter uses the JetStream object store. Tasks link artifacts by ID. | M | Track 4 to show them | Later |
| Long-term project memory | Memory past the last 50 turns. | Each harness has its own memory, and the repository holds the rest. | L | None | Skip |

### Mobility extras

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Follow me on lid close | Agents leave the laptop at once, with no lost turn and no wait of 2.5 minutes for lease and grace period. | The host daemon listens for the sleep notice of macOS and runs a fast drain. A checkpoint after each tool call already exists, so the drain does not wait for the end of the turn. A spike must find out how many seconds macOS gives before sleep. This document did not confirm that. | M | None | Later |
| Handoff note on a planned move | The new session gets the thinking of the old one, which the Claude Code log lacks (design section 20). | Before a planned move, the host daemon asks the old session for one last turn: "Write a handoff note". The note goes into the session log. | S | None | Later |
| Live handoff between harnesses | Move from Claude Code to omp in the middle of a task. | A planned move with `--harness` already does this. | 0 | None | Skip. It exists. |
| Warm standby sessions | Faster takeovers. | Takeover time comes from the lease and the grace period, not from the start of a harness. | M | None | Skip |

### Safety

| Idea | User value | Fit with Borant | Size | Depends on | Verdict |
|---|---|---|---|---|---|
| Audit log | Who did what and when: each control write with actor, fencing token, and revisions. | An append-only stream `borant.audit` that the hub writes after each control write, like events. | S | None | Later. Track 4 can use it for its timeline. Build it once, in whichever track comes first. |
| Replay of a channel | Run a channel again from its event log. | Model output is not deterministic. LangGraph runs all steps after a checkpoint again, with new results [6]. | L | None | Skip. A timeline (track 4) and the WIP and stale refs give the useful part. |
| Secrets never in logs | Safe logs. | Redaction exists. Track 1 owns the rest. | None | Track 1 | See track 1 |
| Permissions for each agent | A worker cannot use tools that it does not need. | The role rule in section 7. Harness-level tool limits need flags for each harness. | M | Track 3 | Later |

## 12. More ideas, and what this document cuts

### The goal API

A goal is the top layer: `borant goal "<text>" --repo <url> --budget
<hours or dollars> --deadline <time>`. Borant creates a channel, spawns an
owner with a planning prompt, applies the governors, sets close gates, and
puts a final report in the human inbox. Each part is a primitive from this
document, so the goal API is mostly a prompt and one CLI command. Size: S
after the roadmap, M before it. Verdict: later. It depends on the spawn
API of track 3, because v1 allows `spawn` only from an agent.

### Channel templates

A template is a file in the repository, for example
`.borant/channels/review-loop.toml`. It holds a task schema, a gate
command, a review rule, caps, and a model for each role. `channel_create`
reads it. Size S. Verdict: later. It is the place for the recipes of this
document once two of them exist.

### What this document cuts

| Cut | Reason |
|---|---|
| Temporal, Restate, DBOS, or Inngest as a dependency | Each adds a server or a database for one person to run. The hub tick and the state store give timers and waits with no new service. |
| Deterministic workflow code above the task graph | The owner agent is the workflow code. A second layer duplicates it. |
| A standby hub on the same NATS server | It protects only against a daemon crash, and systemd covers that. |
| A NATS cluster of two servers | Two servers have no majority after a loss [25]. |
| Saga compensation | The effects are git commits and PRs. A revert is the compensation. |
| Replay of a channel | Model output is not deterministic. |
| Long-term memory in Borant | The harness and the repository own memory. |
| Warm standby sessions | They do not shorten the part of a takeover that takes time. |
| Two-way tracker sync | Two writers for one record. |
| Slack mirror of channels | Noise. The dashboard serves this need. |
| Full event sourcing of the hub | It fixes the lost-event gap, but it changes the store design. The small fix in section 6 covers the gap. |
| More than one person | Out of scope. One person runs the swarm. |

## 13. Roadmap

These six items are the "do next" verdicts. The order puts risk reduction
first and makes each item a base for the next. Each item names its first
step, which fits in one pull request.

1. Hub backup and restore (S). First step: add `borant hub export
   --snapshot`, which holds the request loop while it reads the control
   keys and does not enter maintenance mode. Then add a systemd timer on
   the hub host that copies the file to the second VPS every 5 minutes.
   The replay from host outboxes is the second pull request.
2. Durable primitives (S to M). First step: the `idem` field on control
   calls and the `idem.{key}` store with a lifetime of 24 hours. Then
   `deadline` and `attempt` on tasks in the hub tick, then stall detection,
   then the `wait` tool.
3. Governors (S). First step: `borant stop` and `borant resume` for one
   agent, built on `HostCommand::Fence` and the planned move. Then
   channels and `--all`, then the caps on agents, turns, and hours.
4. Human inbox and phone answers (S). First step: `borant inbox` and
   `borant answer` on a durable consumer for `human`. Then the ntfy adapter
   for the Notifier port with action buttons, then `borant bridge http` on
   the Tailscale address.
5. Close gates (S). First step: a `gate` property on a task, run by the
   host daemon at `task_close`, with the result stored on the task and a
   refusal on failure. Channel-wide gates come second.
6. GitHub PR watch (M). First step: `borant bridge github` that reads one
   channel, polls `check_runs` for each `pr_url`, and sends "checks failed"
   with the job names to the claimer. Reviews and merge state come second.

After these six, the next items are schedules, run until done, cross-harness
review in the hub, and the goal API.

Test plan for each item: focused tests in the crate that changes, and one
Docker scenario in `tests/e2e/` where the item crosses hosts (backup and
restore, governors, PR watch with a fake GitHub server). CI runs the full
suite.

## 14. Open questions for the user

1. Hub risk. Is a hub outage of about 15 minutes with a manual restore
   acceptable for now? If not, the NATS cluster moves into the roadmap and
   needs a third VPS.
2. Schedules. The steve bot already runs schedules. Do you want recurring
   work in Borant too, or does steve stay the scheduler and send messages
   to Borant agents?
3. Spend. Do the Claude Code sessions run on a flat subscription? If yes,
   dollar caps matter only for DeepSeek through LunaRoute, and caps on
   turns and hours are enough for Claude.
4. Phone path. Is ntfy on your phone acceptable as the first path, or do you
   want Slack threads first? Slack gives free-text answers, but it needs an
   app in a workspace.
5. kata or Borant. For a kata epic that runs on Borant, is kata the source of
   truth with a one-way import? This document assumes yes.
6. Linear. Can you install an agent app in the work Linear workspace, or is
   Linear out of scope for a personal tool?
7. Close gates. Is a gate that runs focused tests on the host of the worker
   enough, with CI as the full test run through the PR watch?

## 15. Sources

Each source was read on 2026-10-24. "Official" means a page of the vendor
or project that owns the feature. "Secondhand" means that this document did
not confirm the claim on an official page.

1. Temporal, human-in-the-loop cookbook (official):
   https://docs.temporal.io/ai-cookbook/human-in-the-loop-python
2. Temporal, Schedules (official): https://docs.temporal.io/schedule
3. Restate, durable agents and workflows (official):
   https://docs.restate.dev/use-cases/ai-agents. The awakeable API was not
   confirmed.
4. DBOS with Pydantic AI (official integration docs):
   https://pydantic.dev/docs/ai/integrations/durable_execution/dbos/
5. Inngest, wait for an event (official):
   https://www.inngest.com/docs/features/inngest-functions/steps-workflows/wait-for-event
6. LangGraph, time travel (official):
   https://docs.langchain.com/oss/python/langgraph/use-time-travel
7. Claude Code, agent teams (official):
   https://code.claude.com/docs/en/agent-teams
8. Claude Code, cross-session messaging (official):
   https://code.claude.com/docs/en/cross-session-messaging
9. Claude Code, channels (official):
   https://code.claude.com/docs/en/channels
10. GitHub blog, Copilot coding agent (official):
    https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/
11. OpenHands resolver on PyPI (project page):
    https://pypi.org/project/openhands-resolver/0.3.1
12. Codex cloud best-of-N (secondhand, not confirmed on an OpenAI page):
    https://codex.danielvaughan.com/2026/04/01/codex-cloud-exec-best-of-n-attempts/
13. Linear Agents API (official, developer preview):
    https://linear.app/developers/agents
14. Omnara (vendor listing, claims not confirmed):
    https://apps.apple.com/app/id6748426727
15. Sculptor pairing mode (official): https://docs.imbue.com/features/pairing-mode
16. Conductor (vendor): https://www.conductor.build
17. Terragon (secondhand directory listing):
    https://skillsindex.dev/tools/terragon-labs-terragon-oss/
18. Pi Durable (secondhand):
    https://letsdatascience.com/news/pi-10-adds-mcp-and-durable-harness-6241c6f7
19. Ralph loop plugin (community repository and blogs, not confirmed on an
    official page): https://github.com/Glucksberg/ralph-loop-plugin
20. Anthropic, multi-agent research system (official post, figures from
    secondhand summaries): https://www.anthropic.com/engineering/multi-agent-research-system
21. Cognition, "Don't Build Multi-Agents" (official post, read through
    summaries): https://cognition.ai/blog/dont-build-multi-agents
22. Cemri et al., "Why Do Multi-Agent LLM Systems Fail?" (paper):
    https://arxiv.org/html/2503.13657v3
23. Wataoka et al., "Self-Preference Bias in LLM-as-a-Judge" (paper):
    https://arxiv.org/html/2410.21819v1
24. Pombal et al., self-preference bias in rubric-based evaluation
    (preprint): https://www.alphaxiv.org/abs/2604.06996
25. NATS, JetStream clustering (official):
    https://docs.nats.io/running-a-nats-service/configuration/clustering/jetstream_clustering
26. LiteLLM, budgets and rate limits (official):
    https://docs.litellm.ai/docs/proxy/users
27. LunaRoute (secondhand, not confirmed on the repository):
    https://deepwiki.com/erans/lunaroute
28. Claude Code `--max-budget-usd` (secondhand, not confirmed on an official
    page): https://linuxjedi.co.uk/when-the-docs-fall-short-investigating-claude-codes-budget-cap/
29. ntfy, publish and action buttons (official): https://docs.ntfy.sh/publish/
30. Slack, Socket Mode (official):
    https://docs.slack.dev/apis/events-api/using-socket-mode/
31. Tailscale Funnel (official): https://tailscale.com/kb/1223/tailscale-funnel
