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

## Design rule: pluggable backends

The open-source core stays neutral about where agents run and where
secrets live. The `Provisioner` and `SecretStore` ports live in
`borant-core` with neutral types. A backend plugs in two ways:

- A compiled adapter crate behind a Cargo feature, for a public and generic
  backend such as Fly.io.
- The built-in `exec` adapter. It runs an outside program and speaks one
  versioned JSON protocol over stdin and stdout, described in the elastic
  compute document ("Exec plugin protocol"). Both ports use the same
  protocol and the same `describe` handshake.

The private `borant-infra` repo ships the Hetzner provisioner and the
1Password secrets plugin as exec plugins. The hub never resolves agent
secrets: only host daemons call `SecretStore`, so secret values never
travel through NATS.

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

Run order, as Chris set it: phase 0, phase 1, phase 1b, phase 4, phase 2,
phase 3, phase 5, phase 6. Phase 1b needs the secret bundles of phase 1.
Phase 4 comes early because fast provisioning of VMs lets the swarm grow.
Phase 3 items 1, 2, and 6 (projects, `model` on spawn, and the `orchestrate`
skill) shipped in v0.3.0.

### Phase 0: safety fixes (S)

1. Restrict NATS. Add a tailnet ACL rule that opens port 4222 only to
   `tag:borant` and Chris's devices. Then add NATS user credentials for the
   hub and for each host (`creds_file` already exists in `host.toml`).
2. Hub backup. Add `borant hub export --snapshot` (no maintenance mode) and
   a systemd timer that copies the file off the hub host every 5 minutes.
3. Log the silent failures at `warn`: a failed ack, a failed session-log
   append, a failed hub request. Add `borant agents prune --dry-run`.

### Phase 1: zero-touch hosts (M), secrets document phases 0 to 2

1. Run the phase 0 fact checks of the secrets document on borant-2.
2. In `borant`: the `SecretStore` port, the `env`, `file`, and `exec`
   adapters, bundles in `host.toml`, and resolver mode with the
   `borant secret-exec` shim. Harness logins become default bundles for
   each harness.
3. In `borant-infra`: the `borant-secrets-1password` plugin, the `Borant`
   vault and its read-only service account as secret zero, and the seed
   unit for the first-run screens only.
4. Done when `just create borant-3` gives a working host with no login, and
   no secret value shows in `ps`, the session log, or NATS.

### Phase 1b: collections, roles, and host tags (L)

Chris made these decisions. The spec for this phase settles the rest.

1. Collections. Borant stays one swarm. A collection groups projects and
   owns an identity: secret bundles, the `gh` account and the git author,
   and a pool of Claude Code accounts. The first two collections are
   `personal` and `work`. The accounts are ModelDeck profiles in
   `~/Library/Application Support/ModelDeck/claude-profiles/` (`fentonit`,
   `fentonit-2`, `personal-1`, `personal-2`, `work`). Each profile is a
   `CLAUDE_CONFIG_DIR`. Borant assigns one account from the pool to each
   agent. When an account reaches its quota, Borant can start the next
   launch on another account. The hub keeps each agent inside its
   collection. Phase 1b adds no stronger isolation.
2. Roles. The agent record and `spawn` get a role:
   - `chief`: one for each collection. It prefers the laptop and is pinned.
   - `orchestrator`: one for each project. It requires a `vps` host.
   - `worker`: requires a `vps` host, avoids the laptop, and is bound to a
     task.
   - `helper`: runs on the host of its parent.

   A role sets the default harness and model pair, the placement, the tools
   that the agent can call, and the skill that its brief loads. Only a chief
   spawns orchestrators. Workers cannot spawn.
3. Host tags. `host.toml` gets tags (`laptop`, `vps`, `oregon`, `eu`), and
   each role has `require`, `prefer`, and `avoid` rules over them. Host tags
   replace the labels and taints of the orchestration document. Elastic
   pools (phase 4) carry tags too.
4. Two chiefs, `chief-personal` and `chief-work`, run as Orca tabs on the
   laptop. Channel names get a collection prefix.
5. Orchestrators run on the remote boxes.
6. The Claude Code plugin gets a `borant:chief-of-staff` skill.

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
3. In `borant`: the `Provisioner` port, pools in `hub.toml`, the pure pool
   rules, fill-first placement, parking of idle agents, and the `exec`
   adapter. A Docker e2e scenario tests them with a fake plugin and no
   cloud account.
4. In `borant-infra`: the `borant-provision-hetzner` plugin and its pools.
   The open-source repo holds no Hetzner code.
5. Decide on EU workers (question 3). EU boxes cost 3.6 times less, and
   with pools they are one more pool entry.
6. Fly.io Machines come later as a compiled `borant-fly` crate or an exec
   plugin. The port already has the optional stop, start, and suspend
   operations that they need.

### Phase 5: contain the agents (M), orchestration phases 3 to 5

1. Resources in the heartbeat, and reject reasons in `borant ps`. Host tags
   and roles (phase 1b) replace the placement labels and taints.
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
- sops-nix, agenix, Vault, and OpenBao as the first secrets backends.
  1Password is the default plugin. Doppler, secretspec, and Infisical plug
  in through the same port, as plugins or wrapper presets, when someone
  needs them.
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
7. Git identity for agents. Answered: each collection owns its `gh` account
   and git author (phase 1b).
8. Phone path: hosted ntfy.sh, ntfy on the hub, or Slack threads?
9. Laptops. Answered: a chief prefers the laptop, and a worker avoids it,
   through host tags and role rules (phase 1b).
10. Sandboxes: every worker by default, or only workers in channels that you
    mark as untrusted?
11. Hetzner token for the hub: a root-only token file that `just
    hub-credential` pushes to the hub box, or a hub-side secret store with
    its own vault? The documents propose the file.
12. Store token on hosts: the host daemon runs as the same user as the
    agents, so an agent without a sandbox can most likely read the
    1Password token. Is that acceptable until sandboxes land, or does phase
    1 need a separate plugin user?

Each design document lists more questions that matter only for its own
phase.
