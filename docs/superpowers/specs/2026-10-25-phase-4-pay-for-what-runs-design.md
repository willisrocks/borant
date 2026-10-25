# Phase 4: pay for what runs

This spec builds roadmap phase 4. The hub creates worker hosts from a pool
when work waits, and deletes them when they are idle. The detailed design is
the [elastic compute design](2026-10-24-elastic-compute-design.md) ("the
elastic doc"). This spec says which parts ship now, settles the conflicts
with phase 1, and splits the work into parallel tasks.

The orchestrator wrote it in auto-pilot. The decision records are at the end.

## Scope

| Elastic doc part | In phase 4 | Notes |
|---|---|---|
| Provisioner port, contract, memory fake | Yes | `ProvisionerCapabilities`, not `Capabilities` |
| Pools in `hub.toml`, `rules::pool`, the `elastic.*` state | Yes | Without stop and start (see Cuts) |
| Fill-first placement, `HostInfo.pool` | Yes | Plus `HostInfo.tags` for phase 1b |
| Parking idle agents | Yes | See "Parking" |
| Exec provisioner adapter | Yes | On the phase 1 `borant-exec` runner |
| Docker e2e scenario with a fake plugin | Yes | The proof that needs no cloud account |
| Hetzner exec plugin in `borant-infra` | Yes | Orchestrator or helper |
| Golden image, snapshot timing, hub move, deletes of boxes | No code; operations | Need Chris, see "Questions for Chris" |
| Fly.io | No | Later |

## Goals

1. When an agent cannot be placed, the hub creates a host in a pool, and the
   agent starts there.
2. When a pool host is idle past `idle_timeout` and near the end of its
   billing step, the hub drains and deletes it. No work is lost: the agent
   was parked first, with a final checkpoint.
3. The hub never goes above `max_hosts` or a budget, and never deletes a host
   with a lease.
4. A Docker e2e scenario proves 1 to 3 with a fake plugin and no cloud
   account.
5. No core type, rule, or config key names Hetzner or a size or region of it.

## Cuts

- Stop and start of hosts, `keep_stopped`, and `StopMode` use in the rules.
  Hetzner bills a stopped server at the full price, so stopping saves
  nothing. The port keeps the optional `stop` and `start` methods with their
  default `Invalid` errors, so Fly can use them later.
- `min_warm`. Every pool starts with zero hosts.
- Notifications. The hub has no notifier until roadmap phase 2. A budget stop
  and a `denied` or `invalid` error log a `warn`.
- `accept_spawns`. Phase 1b gives the laptop a tag and the roles their
  `avoid` rules.
- Placement labels and taints. Phase 1b replaces them with host tags. Phase 4
  adds `HostInfo.tags` and pool `tags` only, and placement does not use them
  yet.
- The spread hint of the orchestration doc. Fill first is the only rule.

## Constraints

- Strict ports and adapters. The Hetzner plugin lives in `borant-infra` as an
  exec plugin. Examples in core code and tests use neutral names, for example
  `size = "small"` and `region = "r1"`.
- The exec plugin protocol of the elastic doc is the source of truth. The
  phase 1 runner (`borant-exec`) implements it. Provisioner requests carry
  `config`, the `config` table of the provisioner in `hub.toml`. The adapter
  sets the timeout for each op (10 s `describe`, 30 s `list`, 120 s `create`,
  60 s for the others) and runs one op at a time for each instance.
- `PortErrorKind::Invalid` and `Denied` come from the phase 1 runner task.
  Phase 4 adds no error kind.
- A create can take 120 s. The hub must not hold its `mem` mutex across a
  provisioner call (see `crates/borant-hub/src/lib.rs`, the lock rule at the
  top).
- New `HubFile` sections (`[elastic]`, `[provisioners]`, `[[pools]]`) use
  `deny_unknown_fields`. `borant-infra` adds them only after a release that
  parses them.
- New `MoveReason` or `HostCommand` variants need a release on every host
  before the hub sends them.
- The brief forbids new cloud hosts until fast provisioning lands. The Docker
  scenario is the proof. The first real pool host waits for Chris (see
  "Questions for Chris").

## Interfaces

### Port (`crates/borant-core/src/ports/provision.rs`)

As in the "Provisioner port" section of the elastic doc, with these changes:

- `Capabilities` is named `ProvisionerCapabilities`.
- `HostSpec.labels` is named `HostSpec.provider_labels` (backend metadata,
  not placement tags). `HostRecord.labels` is named
  `HostRecord.provider_labels`.
- `HostSpec` gets `tags: Vec<Tag>`, the pool tags, which the new host
  reports in `HostInfo.tags`.

`Tag` and `PoolName` are new `name_type!` names in `ids.rs`.
`borant-core::testing` gets `provisioner_contract` with the six assertions of
the elastic doc ("Tests"). `borant-memory` gets `MemoryProvisioner` with a
start delay, a create-failure switch, and a call log.

### Records

- `HostInfo.pool: Option<PoolName>` and `HostInfo.tags: Vec<Tag>`, both
  `#[serde(default)]`. The host daemon reads them from `host.toml` (`pool`,
  `tags`) or from `BORANT_POOL` and `BORANT_TAGS` (comma-separated).
- `ElasticHost` at KV key `elastic.<host>`: `pool`, `provider_id`, `phase`
  (`Requested`, `Booting`, `Ready`, `Draining`, `Failed`, `Lost`,
  `Deleting`), `created`, `last_busy`, `billing_step_end`. Written only by the
  hub.
- `Spend` at KV key `spend.<yyyy-mm>`: `usd: f64` and the estimate of live
  hosts.

### `hub.toml`

As in "Pools in `hub.toml`" of the elastic doc, without `keep_stopped` and
`min_warm`, and with `tags` in place of `labels`:

```toml
[elastic]
tick = "10s"
lost_timeout = "10m"
create_backoff = "5m"
monthly_budget_usd = 80

[provisioners.cloud]
adapter = "exec"
command = ["borant-provision-hetzner"]
config = { firewall = "borant-locked", ssh_key = "borant" }
credential = "provider-token"

[[pools]]
name = "workers"
provisioner = "cloud"
size = "small"
region = "r1"
image = "worker"
name_prefix = "borant-e"
tags = ["vps"]
harnesses = ["claude-code", "omp"]
agents_per_host = 4
max_hosts = 2
idle_timeout = "20m"
boot_timeout = "10m"
monthly_budget_usd = 60
# price_hour_usd = 0.1   # when the backend reports no price
```

## Rules

### Placement (`crates/borant-core/src/rules/schedule.rs`)

The sort key changes to fill first: a static host (`pool = None`) wins over a
pool host, and among pool hosts the host with more active agents wins. Among
static hosts the order stays as today. The existing test that expects the
host with fewer agents changes for pool hosts only.

### Pools (`crates/borant-core/src/rules/pool.rs`)

`choose` and `plan` as in "Placement and choice of pool" and "State of each
pool host" of the elastic doc, without the stop and start paths and
`min_warm`:

- `pub fn choose(pools: &[PoolConfig], hosts: &[ElasticHost], spend: &Spend, need: &PlacementNeed, now: Timestamp) -> Option<PoolName>`
- `pub fn plan(pools: &[PoolConfig], hosts: &[ElasticHost], live: &[HostView], unplaced: &[PlacementNeed], records: &BTreeMap<PoolName, Vec<HostRecord>>, spend: &Spend, now: Timestamp) -> Vec<Action>`
- `enum Action { Create { pool, name }, Drain { host }, Undrain { host }, Delete { host, id }, Adopt { pool, record }, Forget { host }, MarkLost { host }, MarkFailed { host } }`

`PlacementNeed` holds the harness and the agent of one unplaced agent.
`PoolConfig` is the parsed `[[pools]]` entry. All of these types live in
`rules/pool.rs`.

## Parking

The hub parks only agents on pool hosts, because only those hosts must go
idle, and only agents that can wake (movable, with channels). The flow:

1. The host reports an idle signal. `SessionLeaseRef` in the heartbeat gets
   `idle_since: Option<Timestamp>` (`#[serde(default)]`): `None` while the
   session is in a turn, else the end of its last turn. A missing or stale
   report makes the agent ineligible.
2. The hub picks an agent when `idle_since` is older than the
   `idle_timeout` of its pool, it claims no task, and `bus.unacked` is empty.
3. The hub sends `HostCommand::Park { session }`. The host checks again that
   the session is idle, writes a final checkpoint, and stops the session. If
   the checkpoint fails, the host does not stop it and reports the error.
4. The host ends the session with `EndSession { reason: "park" }`. The hub
   sets the lease to `Released { since, parked: true }` (a new field,
   `#[serde(default)]`), so the release and the park mark change in one
   write.
5. The wake rule (`crates/borant-hub/src/leases.rs`, the gate that today
   needs an `Expired` lease) also wakes a `Released` lease with
   `parked: true` when its inbox has a message that wakes it, or when it
   claims a task. A plain `Released` lease (an ordinary session end) stays
   asleep, except that an end which leaves an `in_progress` claim on the
   agent places it again at once through the takeover path, with the prompt
   context of its session; a host that takes none sets the claim `stuck`
   with "the session of \<agent\> ended (\<reason\>); relaunch failed:
   \<why\>", and one `session.relaunch` event says which of the two
   happened. A message that arrived during the park stays in the inbox and
   wakes the agent.

## Tasks

| Task | Who | Blocked by | Files |
|---|---|---|---|
| Provisioner port and memory fake | worker | none | `crates/borant-core/src/ports/provision.rs`, `ids.rs`, `testing.rs`, `crates/borant-memory/src/provision.rs` |
| Fill-first placement and host tags | worker | none | `crates/borant-core/src/rules/schedule.rs`, `model.rs` (`HostInfo`), `crates/borant/src/config.rs` (host section), `crates/borant-host/src/lib.rs` |
| Pool rules | worker | Provisioner port | `crates/borant-core/src/rules/pool.rs` |
| Parking idle agents | worker | none | `crates/borant-core/src/{model.rs,proto.rs}` (`LeaseState::Released.parked`, `SessionLeaseRef.idle_since`, `HostCommand::Park`), `crates/borant-host/src/{sessions.rs,lib.rs,commands.rs}`, the wake gate in `crates/borant-hub/src/leases.rs`, a new `crates/borant-hub/src/park.rs` |
| Exec provisioner | worker | Provisioner port, phase 1 exec runner | `crates/borant-exec/src/provision.rs`, `tests/plugins/provision-file.sh` |
| Hub elastic executor | worker | Provisioner port, Pool rules | new `crates/borant-hub/src/elastic.rs`, `lib.rs` (tick, `HubConfig`), `keys.rs`, `crates/borant/src/config.rs` (`HubFile`), `roles.rs` (wiring) |
| Docker e2e scenario | worker | Exec provisioner, Hub executor, Parking | `docker/`, `tests/e2e/`, the fake harness |
| Hetzner plugin | orchestrator or helper | protocol only | `borant-infra` |
| Phase 4 release and deploy | helper, orchestrator | every code task | none |

### Provisioner port and memory fake

Acceptance: the port of "Interfaces" exists with the renames. The six
assertions of `provisioner_contract` hold for `MemoryProvisioner`: a create
is idempotent by pool and name, `list` shows only the hosts of that pool, a
delete of a missing host is Ok, a delete of a host of another pool is
`Denied`, a failed create returns an error and leaves no host, and `stop`
and `start` return `Invalid` by default. Proof:
`just test-crate borant-memory provision`.

### Fill-first placement and host tags

Acceptance: the sort key of "Placement". `HostInfo.pool` and `HostInfo.tags`
come from `host.toml` or `BORANT_POOL` and `BORANT_TAGS`. A host with neither
reports `None` and an empty list. Old `HostInfo` JSON parses. Proof:
`just test-crate borant-core schedule` (a static host beats a pool host; a
busier pool host beats an emptier one; static hosts keep today's order) and
`just test-crate borant config`.

### Pool rules

Acceptance: `choose` and `plan` hold every rule of "State of each pool host"
of the elastic doc without stop, start, and `min_warm`, with one test for
each: demand above supply creates; never above `max_hosts`, a pool budget,
or the monthly budget; no create during `create_backoff`; an idle host waits
for `idle_timeout` and the end of its billing step within 5 minutes; a drain
then a lease gives `Undrain`; a host with no key gives `Adopt`; a key with no
host gives `Forget`; no heartbeat for `lost_timeout` gives `MarkLost` then
`Delete`; a host that misses `boot_timeout` gives `MarkFailed` then `Delete`.
Proof: `just test-crate borant-core pool`.

### Parking idle agents

Acceptance: the five steps of "Parking". Proof: `just test-crate borant-hub
park` (an idle agent on a pool host is parked; a busy one, one with a claim,
one with an unacked message, one on a static host, and one with a stale
report are not; a parked agent wakes on a message; a plain released one does
not) and `just test-crate borant-host park` (the host refuses to park a
session in a turn, and does not stop it when the checkpoint fails).

### Exec provisioner

Acceptance: `ExecProvisioner` speaks the provisioner ops of the elastic doc
("Provisioner ops") through `borant-exec`, with `config` in each request, the
timeout of each op, and one op at a time. `tests/plugins/provision-file.sh`
keeps hosts in a JSON file and passes `provisioner_contract`. Proof:
`just test-crate borant-exec provision`.

### Hub elastic executor

Acceptance: the hub parses `[elastic]`, `[provisioners]`, and `[[pools]]`.
Every `elastic.tick`, it gathers the inputs, runs `plan` without the `mem`
mutex held across a provisioner call, executes the actions, and writes
`elastic.*` and `spend.*`. A create sets `boot` values in the `HostSpec`
(host name, pool, tags) for the new host to read. A hub with no `[[pools]]`
does nothing new. Proof: `just test-crate borant-hub elastic` on
`MemoryProvisioner`: an unplaced spawn creates a host, the host registers
and the agent is placed, an idle host is drained and deleted, `max_hosts`
holds, and a hub restart adopts a host with no key.

### Docker e2e scenario

Acceptance: the scenario of the elastic doc ("Tests"): a hub with no worker
hosts and one pool (`agents_per_host = 2`, `max_hosts = 2`), a
`provision-docker.sh` plugin that starts fake hosts with `docker run`, three
spawns give two containers, the containers go away when the agents park,
and a killed container gives a takeover and a delete. The cleanup removes
every container with the pool label. Proof: `just e2e elastic` in CI.

## Questions for Chris

1. Create the Hetzner project `borant-workers` and a read-write token for it,
   and save the token in the `Agents` vault. Real pool hosts wait for it (see
   the decision records).
2. The golden image and the snapshot timing test create servers. The brief
   forbids new cloud hosts until fast provisioning lands. Default: run them
   after the Docker scenario passes in CI.
3. The hub move to a small box and the delete of borant-2 change the fleet
   (roadmap question 2). Default: do not move or delete until Chris answers.
4. EU pools (roadmap question 3). Default: none until Chris answers.

## Decision records

Decision: no stop and start in phase 4. Evidence: Hetzner bills a stopped
server at the full price (roadmap, "Facts that changed"). Reversible: yes,
the port keeps the methods. Friend: none, scope cut. Blocker: none.

Decision: pools carry `tags`, and `HostInfo.tags` lands in phase 4, unused by
placement until phase 1b. Evidence: the roadmap says that elastic pools carry
tags, and phase 1b runs after phase 4. Reversible: yes. Friend: none.
Blocker: none.

Decision: build and test all of phase 4 tonight, and create real servers
only in a separate Hetzner project `borant-workers`, which Chris makes.
Evidence: the token of the existing project can also delete borant-1 (it
powers `just destroy`), so a label guard in the plugin cannot contain a bug
that bypasses it. Reversible: yes. Friend: Codex agreed, and asked that the
plugin still check an ownership label (`borant.managed=hub`), the pool, and
the name prefix, and fetch a server before it deletes it by ID. Blocker: the
project, from Chris.

Decision: parking uses `Released { parked: true }` and an idle signal in the
heartbeat. Evidence: an `Expired` lease lets the old session reclaim it
(`crates/borant-core/src/rules/lease.rs`, `reclaim`), which conflicts with an
intended stop. The hub has no idle signal today (busy state is host-local).
Reversible: yes. Friend: Codex recommended it, and added that a missing idle
report must make the agent ineligible, that the host must check again before
it stops the session, and that only wakeable agents can park. Blocker: none.
