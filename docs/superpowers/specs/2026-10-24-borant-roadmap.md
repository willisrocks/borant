# Borant roadmap after v0.2.0

This document orders the work from five design documents into one plan. It
also settles the conflicts between them and lists the decisions that only
Chris can make. Each design document holds the detail. This document holds
the order and the reasons.

## The five design documents

| Document | What it designs | Size |
|---|---|---|
| [Secrets and host bootstrap](2026-10-24-secrets-and-host-bootstrap-design.md) | A `Borant` vault in 1Password with a read-only service account, `op inject` at boot, long-lived tokens for Claude Code, LunaRoute, and GitHub, unattended first-run setup, a NixOS golden image, `borant doctor`, and secret refs for agents | M |
| [Elastic compute](2026-10-24-elastic-compute-design.md) | Hetzner scale to zero (delete and create from a snapshot), a `Provisioner` port, a cost model against Fly.io and a dedicated box | M |
| [Orchestration API and sandboxes](2026-10-24-orchestration-api-and-sandboxes-design.md) | `borant spawn` with a `SpawnSpec`, model profiles, idempotency keys, a persisted spawn queue, placement labels, a bubblewrap sandbox, a herdr launcher | L |
| [Observability](2026-10-24-observability-design.md) | A swarm event stream, `borant events` and `borant top`, ntfy alerts, LLM usage counts, a web page on Tailscale | M |
| [Durable orchestration vision](2026-10-24-durable-orchestration-vision-design.md) | About 50 ideas with verdicts: hub backup, durable primitives, governors, a human inbox, close gates, a GitHub PR watch, and more | Reference |

## Facts that changed during the review

- The Hetzner API for the `borant` project bills in USD with no VAT. A CPX31
  costs $73.49 per month in `hil` and $20.49 per month in `fsn1`. The CPX
  line runs in the EU too. The elastic compute document now has the API
  prices.
- NATS on borant-1 has no authentication, and the tailnet policy allows
  all traffic. Any device on the tailnet can act as a hub or a host. A
  sandboxed agent that can reach port 4222 can do the same. The fix is in
  phase 0.
- Hetzner bills a powered-off server at the full price. "Auto-suspend" on
  Hetzner means: delete the server, and create it again from a snapshot.

## Conflicts between the documents

| Conflict | Resolution |
|---|---|
| The compute document fills elastic hosts first, so that empty hosts can be deleted. The orchestration document spreads the workers of a channel across hosts. | Fill first is the default, because it saves money. Spread becomes a placement hint that a channel can ask for. The pure rule in `borant-core` gets both, with tests. |
| The compute document deletes borant-2 and moves the hub to a CPX11. The vision document copies hub backups to "the second VPS". The observability document lets borant-2 alert when the hub dies. | The backup target becomes Hetzner Object Storage or the laptop, not a second VPS. The hub-down alert goes from the laptop host daemon, which sees the hub vanish when the laptop is awake. No public check is used, because the fleet has no inbound internet access. Chris decides (question 2). |
| Idempotency keys appear in the vision and orchestration documents. ntfy appears in the vision and observability documents. | The orchestration document owns idempotency (`spawn_key`, `create_key`, the `task_close` repeat rule). The observability document owns the ntfy notifier. The vision document refers to both. |
| `borant doctor` (secrets) and the launch watchdog (orchestration) both detect a stuck launch. | They are two layers. `doctor` stops a bad host from getting work. The watchdog catches a session that hangs anyway and reports its screen. |
| The vision document puts a stall check in the hub. The observability document has an "agent stalled" alert. | One rule in the hub tick. It feeds both the task state and the alert. |

## Roadmap

Each phase is small enough for one or two pull requests, and each phase
makes the next one easier. The order puts safety first, then the end of
manual work, then visibility, then cost, then new power.

### Phase 0: safety fixes (S)

1. Restrict NATS. Add a tailnet ACL rule that opens port 4222 only to
   `tag:borant` and Chris's devices. Then add NATS user credentials for the
   hub and for each host (`creds_file` already exists in `host.toml`).
2. Hub backup. Add `borant hub export --snapshot` (no maintenance mode) and
   a systemd timer that copies the file off the hub host every 5 minutes.
3. Log the silent failures at `warn`: a failed ack, a failed session-log
   append, a failed hub request. Add `borant agents prune --dry-run`.

### Phase 1: zero-touch hosts (M), secrets document phases 0 and 1

1. Run the phase 0 fact checks of the secrets document on borant-2.
2. Create the `Borant` vault and the `borant-hosts` service account. Add
   `modules/secrets.nix` with `op inject` at boot.
3. Seed the first-run settings of Claude Code and omp, the LunaRoute key
   and catalog, the git identity, and `gh` as the git credential helper.
4. Done when `just create borant-3` gives a working host with no login.

### Phase 2: see the swarm (M), observability phases 1 to 3

1. The swarm event stream and `borant events --follow`.
2. `borant top` and `borant status --json` from one hub snapshot.
3. ntfy alerts for needs-human tasks, stuck agents, hosts offline, and the
   hub down.

### Phase 3: drive the swarm (M), orchestration phases 1 and 2

1. Projects: a named git repository in the hub, channels that belong to a
   project, and one bare mirror per project on each host with a worktree
   for each agent (orchestration document, "Projects").
2. `model` on spawn, and profiles in `hub.toml`: owners on Claude Code,
   workers on omp with DeepSeek.
3. `borant spawn`, `wait`, `cancel`, and `logs` for humans and scripts.
4. Idempotency keys, the persisted spawn queue, and `stop_on_close` for
   workers bound to a task.
5. The launch watchdog.
6. Ship the `orchestrate` skill in the Claude Code plugin, and change it as
   each item above lands.

### Phase 4: pay for what runs (M), compute phases 0 to 3

1. Measure the time to create a box from a snapshot. Build the golden image
   (secrets document phase 2).
2. Move the hub to a small always-on box. Delete the idle worker box.
3. Add the `Provisioner` port with the Hetzner adapter, fill-first
   placement, and parking of idle agents.
4. Decide on EU workers (question 3). EU boxes cost 3.6 times less.

### Phase 5: contain the agents (M), orchestration phases 3 to 5

1. Placement labels and taints, resources in the heartbeat, and reject
   reasons in `borant ps`.
2. The bubblewrap sandbox, then the network allowlist.
3. The herdr launcher on the VPS hosts.

### Phase 6: durable work (M), vision roadmap items 2 to 6

1. Task deadlines, retries, stall detection, and the `wait` tool.
2. `borant stop`, `borant resume`, and caps on agents, turns, and hours.
3. The human inbox: `borant inbox`, `borant answer`, and phone answers.
4. Close gates that run focused tests at `task_close`.
5. The GitHub PR watch.

### Later

LLM usage counts and dollar budgets (observability phase 4), the web page
(observability phase 5), agent secret refs (secrets phase 4), schedules,
run until done, cross-harness review, the goal API, and a NATS cluster for
hub high availability.

## What this roadmap cuts

- A self-hosted Fly clone. Hetzner Cloud does not allow nested
  virtualization, and the savings come from the price of a dedicated box,
  not from microVMs.
- Fly.io Machines as the first elastic path. They cost about the same as
  Hetzner. They stay an option if a 2 to 4 minute cold start is too slow.
- Temporal, Restate, DBOS, or Inngest as dependencies. The hub tick and the
  state store cover the durable primitives that Borant needs.
- sops-nix, agenix, Vault, OpenBao, and Infisical as the secrets store.
  1Password stays the store.
- Grafana and VictoriaMetrics until weeks of history matter.

## Decisions for Chris

These questions came from several documents. Each one changes what gets
built.

1. Claude Code billing. Do unattended hosts keep the Max subscription token
   from `claude setup-token`, or do they move to an `ANTHROPIC_API_KEY`?
   The subscription terms assume ordinary individual use. This decides
   whether dollar budgets matter for Claude, or only for DeepSeek.
2. Fleet shape. Is one small always-on hub box plus elastic workers
   acceptable, with hub backups in object storage and a hub outage of about
   15 minutes with a manual restore? The other choice is a NATS cluster,
   which needs three always-on boxes.
3. Region. May workers run in the EU (3.6 times cheaper, about 150 ms to
   `borant attach`), or must they stay in `hil`?
4. Volume. How many agent-hours per month, and how many agents at the peak?
   Below about 2,150 agent-hours, elastic Hetzner beats two always-on
   boxes. Above about 1,200, a dedicated EU box beats elastic.
5. LunaRoute. Is the LunaRoute extension your code? If yes, a
   `LUNAROUTE_API_KEY` environment variable and a catalog fetch on omp
   remove the import step and the catalog file. Does LunaRoute report usage?
6. 1Password plan. On Teams, the whole account gets 5,000 service-account
   reads a day. Is one shared token for all hosts acceptable?
7. Git identity for agents: you with the noreply address, or a bot account?
8. Phone path: hosted ntfy.sh, ntfy on the hub, or Slack threads?
9. Laptops: may unattended spawns land on an awake laptop? The documents
   propose no, with a `laptop` taint.
10. Sandboxes: every worker by default, or only workers in channels that you
    mark as untrusted?

Each design document lists more questions that matter only for its own
phase.
