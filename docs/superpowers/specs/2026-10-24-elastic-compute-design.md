# Elastic compute and economics for Borant

Date: 2026-10-24. Status: draft for review, revision 2. Track 2 of 5.

Related designs: `2026-10-24-secrets-and-host-bootstrap-design.md` (golden
image, logins, and the `SecretStore` port, which uses the "Exec plugin
protocol" section of this doc), `2026-10-24-orchestration-api-and-sandboxes-design.md`
(spawn, placement labels and taints), and `2026-10-24-observability-design.md`.

## Summary

Borant core stays agnostic about where compute runs. A new `Provisioner`
port in `borant-core` creates, lists, and deletes hosts with neutral types.
Demand, pools, timers, budgets, and the reconcile loop are pure rules in
`borant-core` plus a small executor in the hub, so every backend gets them
for free. A backend plugs in as an adapter crate behind a Cargo feature, for
public APIs such as Fly.io. Or it is an external program that the built-in
`exec` adapter runs with a small JSON protocol. The Hetzner backend becomes
an exec plugin in the private `borant-infra` repo. The open-source repo gets
no Hetzner code.

The economics do not change from revision 1. Hetzner has no "suspend" that
stops the bill, so scale to zero means "delete, then create again from a
snapshot". The two always-on CPX31 boxes cost about $148 per month and run
no agents. A small hub plus elastic workers costs about $24 to $81 per month
for 50 to 1,000 agent-hours. Fly.io costs about the same, but starts in
seconds.

## Goals and non-goals

Goals: stop paying for idle worker boxes. Start a host without a human when
the hub has work and no free slot. Remove a host only when no agent can lose
work. Keep `borant-core` and `borant-hub` free of cloud vendors, with several
backends at once. Test the elastic path in CI with no cloud account.

Non-goals: a highly available hub (design section 20), the golden image
(secrets doc), sandboxes (orchestration doc), and live price comparison.

## Facts on 2026-10-24

### Hetzner Cloud billing

| Fact | Value | Source |
|---|---|---|
| Powered-off server | Billed in full until deleted ("independent of their state") | [1] |
| Hourly billing | Lower of hourly rate x hours and the monthly cap. Each started hour is rounded up. | [1] |
| Snapshots and IPv4 | Snapshots $0.0199 per GB per month (API). IPv4 $0.60 per month in `hil`, billed also when unassigned. | [1], [8], [9] |
| Price increase | New prices from 2026-06-15 for new orders and rescales. Old orders keep old prices. | [5], [6] |
| Nested virtualization | "No, this is not possible on cloud server." | [7] |
| Names, labels, rate limit | Names are unique in a project. `GET /servers` takes a `label_selector`. 3,600 requests per hour. | [10] |
| Create from a snapshot | Not published. Estimate 1 to 2 minutes (unverified). Phase 0 measures it. | none |

### Hetzner Cloud prices

From the Hetzner API for Chris's `borant` project (2026-10-24). The account
bills in USD with no VAT, and the prices include the IPv4 address. `fsn1`
stands for the EU regions (`nbg1` and `hel1` have the same prices).

| Type | vCPU / RAM / disk | `hil` per month | `hil` per hour | `fsn1` per month | `fsn1` per hour |
|---|---|---|---|---|---|
| CPX11 | 2 / 2 GB / 40 GB | $20.49 | $0.0328 | $5.99 | $0.0096 |
| CPX21 | 3 / 4 GB / 80 GB | $37.49 | $0.0601 | $10.99 | $0.0176 |
| CPX31 (today) | 4 / 8 GB / 160 GB | $73.49 | $0.1178 | $20.49 | $0.0328 |
| CPX41 | 8 / 16 GB / 240 GB | $141.49 | $0.2267 | $37.99 | $0.0609 |
| CPX32 | 4 / 8 GB / 160 GB | not offered | | $41.99 | $0.0673 |
| CX43 (x86) | 8 / 16 GB / 160 GB | not offered | | $18.49 | $0.0296 |
| CAX21 (Arm) | 4 / 8 GB / 80 GB | not offered | | $12.49 | $0.0200 |
| CCX13 (dedicated vCPU) | 2 / 8 GB / 80 GB | $50.99 | $0.0817 | $50.49 | $0.0809 |

The cost model used $74.08 for the CPX31 in `hil`, so its US totals are about
1% high. EU workers cost 3.6 times less. The CX, CAX, and CCX lines showed
"currently unavailable" on the web pages [3], [4], [11].

Hetzner dedicated servers run only in Germany and Finland. An AX41 (6 cores,
64 GB, 2x 512 GB NVMe) costs €59.00 per month with no setup fee [12], [13].
The notice is "30 days to the end of the month" [14]. It is a static host.

### Fly.io Machines

| Fact | Value | Source |
|---|---|---|
| Billing | Per second while started. Shared vCPU $0.69, performance vCPU $21.00, RAM $6.00 per GB, each per month in `iad`. `sjc` costs 1.192 times more. | [15], [16], [17] |
| Stopped or suspended machine | $0.15 per GB per month of rootfs only | [15] |
| Shared CPU limit | 6.25% baseline per vCPU, 5 s starting burst. Builds need performance CPUs. | [19] |
| Start and create | Start of a stopped machine "usually well under a second". Create "low double digit seconds", no capacity guarantee. | [21] |
| Rootfs | "Blank slate on every startup". About 8 GB maximum (community posts). | [21], [22] |
| Suspend | Snapshot with memory, resume in "a few hundred ms", only up to 2 GB RAM. A lost snapshot cold-boots. | [23] |
| Machines API | `POST /v1/apps/{app}/machines` with `name`, `region`, `config.guest`, `config.env`, `config.metadata`. `GET .../machines?metadata.{key}=value`. `.../{id}/stop`, `/suspend`, `/start`, `DELETE`. | [24] |

A CPX31 slot (2 vCPU, 4 GB, one hour) in `hil` costs $0.030. Other options
cost more: Fly Sprites about $0.033 (plan fees not verified) [26],
Northflank $0.067 [27], and E2B, Daytona, Modal, Railway, Cloudflare
Containers, and Fargate $0.08 to $0.27, with session caps or much setup
[28] to [33].

Agent sizing: Claude Code uses 0.46 to 0.68 GB RSS (1.5 GB for one long
session), and `borant-1` uses 870 MB with NATS, the hub, and the host daemon.
One slot is 2 GB RAM and 1 vCPU, so a CPX31 holds 4 agents.

## Options compared

| Option | Idea | Cold start | Cost to build |
|---|---|---|---|
| A. Keep 2 always-on CPX31 | Today | None | None |
| B. Small hub plus a Hetzner pool | Delete idle workers, create from a snapshot | 2 to 4 min (unverified) | Port, pools, exec adapter, Hetzner plugin, image |
| C. Small hub plus a Fly pool | One machine per agent, stop when idle | Seconds | B, plus `borant-fly` and an OCI image |
| D. One dedicated AX41 | Many cheap slots in the EU | None | A static host, EU latency |
| E. Self-hosted Fly clone | Firecracker on dedicated metal | About 1 s | Weeks of work, ongoing upkeep |

Do not build option E. The savings come from the dedicated box, not from
microVMs: a plain `borant host` with `max_agents = 20` on an AX41 gets the
same slots [34], [35]. Options B and C can run side by side as two pools.

## Cost model

```
H      = live agent-hours per month (a session holds a lease)
slot_h = price per host-hour / agents_per_host
pack   = used slots / paid slots, including idle tails (0.5 assumed)

A (always on) = n_boxes * monthly_price
B (Hetzner)   = hub + H * slot_h / pack + snapshots
C (Fly)       = hub + H * fly_hour * 1.05 + stopped * rootfs_gb * 0.15
D (dedicated) = dedicated_monthly + hub (or 0 if the hub runs on it)
```

Inputs in USD: hub CPX11 in `hil` $21.16 per month. B: CPX31 $0.1187 per
hour, 4 slots, so $0.059 per agent-hour at `pack = 0.5`. C: `shared-cpu-2x`
4 GB in `sjc` $0.0420 per hour, `performance-1x` $0.0745, a 5-minute idle
tail (1.05), and $3.60 per month for eight stopped machines. D: AX41 $71.

| Agent-hours per month | A: 2x CPX31 | B: hub + Hetzner pool | C: hub + Fly shared | C: hub + Fly performance | D: AX41 + hub |
|---|---|---|---|---|---|
| 50 | $148 | $24 | $27 | $29 | $92 |
| 200 | $148 | $33 | $34 | $41 | $92 |
| 1,000 | $148 | $81 | $69 | $103 | $92 |

B beats A below about 2,150 agent-hours per month. B and C are within $12 at
1,000 agent-hours, so the choice is on start time and build speed. D beats B
above about 1,200 agent-hours. A box created before 2026-06-15 keeps its old
price, and a delete loses that price.

LLM spend for scale uses unverified token estimates, with half of each live
hour active. One live agent-hour costs about $1.67 on Claude Opus 5.5, $1.00
on Claude Sonnet 5.5, and $0.09 on DeepSeek V4.1 Flash at API prices [36],
[37]. Thus compute with B is about 9% to 14% of the bill for Claude agents,
and about 65% for omp agents on DeepSeek Flash.

## Recommended design

Choose option B for the first pool, and keep option C one crate away. Each
part lives here:

| Part | Repo and crate | Knows about clouds |
|---|---|---|
| `Provisioner` port, its types, `provisioner_contract` | `borant` / `borant-core` | No |
| Pool rules: demand, choice of pool, idle, budget, reconcile | `borant` / `borant-core::rules::pool` (pure) | No |
| Pool executor: runs the rules, writes `elastic.*` keys | `borant` / `borant-hub` | No |
| Fake provisioner for unit tests | `borant` / `borant-memory` | No |
| `exec` adapter and the plugin protocol codec | `borant` / new `borant-exec` | No |
| Fly adapter (later) | `borant` / new `borant-fly`, Cargo feature `fly` | Fly |
| Hetzner plugin, golden image, the hub `hub.toml` | `borant-infra` (private) | Hetzner |

The hub box is a CPX11 in `hil` with NATS, `borant hub`, and the plugins.
Laptops and always-on boxes are static hosts. Both ways to plug in a backend
(below) pass `provisioner_contract`.

### Provisioner port

The port is `crates/borant-core/src/ports/provision.rs`. Only the hub calls
it. Size, region, and image are opaque strings that only the backend reads.

```rust
pub struct PoolName(String);              // ids.rs, same rules as HostName
pub struct ProviderHostId(pub String);    // the backend's id, e.g. a server id

pub struct HostSpec {
    pub pool: PoolName,
    pub name: HostName,                   // chosen by the hub
    pub size: String,                     // "cpx31", "performance-1x"
    pub region: String,                   // "hil", "sjc"
    pub image: Option<String>,            // None: the backend default
    pub labels: BTreeMap<String, String>,
    /// Values the new host reads at boot: host name, pool, one-time join
    /// data (secrets doc). Adapters must not log them.
    pub boot: BTreeMap<String, String>,
}

pub struct HostRecord { pub id: ProviderHostId, pub pool: PoolName, pub name: HostName,
                        pub state: ProviderState, pub created: Timestamp,
                        pub labels: BTreeMap<String, String> }

pub enum ProviderState { Creating, Running, Stopping, Stopped, Deleting, Failed(String) }
pub enum StopMode { Stop, Suspend }

pub struct SizeInfo { pub id: String, pub vcpus: u32, pub mem_mb: u32,
                      pub price_hour_usd: Option<f64>, pub stopped_price_hour_usd: Option<f64> }

pub struct Capabilities {
    pub backend: String,                  // "hetzner", "fly", "fake": logs only
    pub sizes: Vec<SizeInfo>,
    pub regions: Vec<String>,
    pub typical_start: Duration,          // create until the first heartbeat
    pub typical_resume: Option<Duration>, // start after stop, if supported
    pub billing_step: Duration,           // 1 h on Hetzner, 1 s on Fly
    pub one_host_per_agent: bool,         // the natural shape of the backend
    pub stop_modes: Vec<StopMode>,        // empty: delete only
}

#[async_trait]
pub trait Provisioner: Send + Sync + std::fmt::Debug {
    async fn describe(&self) -> PortResult<Capabilities>;
    /// Returns when the backend accepted the order. Idempotent by pool and name.
    async fn create(&self, spec: &HostSpec) -> PortResult<HostRecord>;
    /// Deletes the host and its disk. Missing is Ok. Not ours is Denied.
    async fn delete(&self, id: &ProviderHostId) -> PortResult<()>;
    /// Only the hosts of this pool that this provisioner created.
    async fn list(&self, pool: &PoolName) -> PortResult<Vec<HostRecord>>;
    /// Optional. The hub calls them only for a mode in `stop_modes`.
    async fn stop(&self, _id: &ProviderHostId, _mode: StopMode) -> PortResult<()> {
        Err(PortError::new(PortErrorKind::Invalid, "stop is not supported"))
    }
    async fn start(&self, _id: &ProviderHostId) -> PortResult<HostRecord> {
        Err(PortError::new(PortErrorKind::Invalid, "start is not supported"))
    }
}
```

`PortErrorKind` gets two new kinds, `Invalid` and `Denied`, so that the
error codes of the exec protocol map one to one. The secrets doc needs the
same kinds.

### Pools in `hub.toml`

A pool names one provisioner instance and sets the shape of its hosts. One
instance can serve several pools, for example `hil` and EU Hetzner pools.

```toml
[elastic]
tick = "10s"
lost_timeout = "10m"
create_backoff = "5m"            # after a failed create in a pool
monthly_budget_usd = 80          # all pools together

[provisioners.hetzner]
adapter = "exec"
command = ["borant-provision-hetzner"]
config = { firewall = "borant-locked", ssh_key = "borant" }
credential = "hcloud-token"      # systemd credential of the hub unit

[[pools]]
name = "hil-workers"
provisioner = "hetzner"
size = "cpx31"
region = "hil"
image = "label:borant.image=worker"   # opaque: the plugin reads it
name_prefix = "borant-e"
labels = ["vps", "us"]                # host labels for placement
harnesses = ["claude-code", "omp"]
agents_per_host = 4                   # 1 for one microVM per agent
min_warm = 0
max_hosts = 2
keep_stopped = 0                      # stopped hosts to keep (Fly, not Hetzner)
idle_timeout = "20m"
boot_timeout = "10m"
monthly_budget_usd = 60
```

A pool can set `price_hour_usd` if its backend reports no price. Each pool
host gets the pool labels, `always_on = false`, and a new `HostInfo` field
`pool: Option<PoolName>`, which the host daemon reads from `BORANT_POOL`.

### Placement and choice of pool

The scheduler places an agent on an existing host first. The orchestration
doc adds labels and taints to `schedule::place`. This doc adds two terms to
its sort key: a static host wins over a pool host, and among pool hosts, more
active agents win. This packs the hosts, so that empty hosts can go away.

When no host qualifies, `rules::pool::choose` picks a pool. For each pool, it
builds the `HostView` of a new host (labels, harnesses, `agents_per_host`)
and runs the same filter as `place`. Thus a spawn that
requires `fast-start` goes only to a Fly pool, and one that requires `eu`
goes only to an EU pool. Among the pools that qualify and are below
`max_hosts`, below the budgets, and out of backoff, the rule picks a stopped
host that the pool can start first. Then it picks the lowest price per
slot-hour (`price_hour_usd / agents_per_host`, no price sorts last), then the
shortest `typical_start`, then the pool name.

### State of each pool host

The hub writes one key for each pool host, `elastic.{host}`: pool, provider
id, phase, creation time, last busy time, and end of the billing step.

```
          demand              create accepted       heartbeat
 (none) --------> Requested ----------------> Booting ---------> Ready
                                                 |                 |
                                    boot_timeout |                 | idle and past the billing step
                                                 v                 v
                                              Failed            Draining --(lease appears)--> Ready
                                                 |                 | 0 leases
                                                 v                 v
                                             Deleting <--- no --- room under keep_stopped and Stop?
                                                 |                 | yes
                                                 v                 v
                                              (none)            Stopped --(demand: start)--> Booting
 Ready --(no heartbeat for lost_timeout)--> Lost --> Deleting
```

`rules::pool::plan` is a pure function from the pools, hosts, `elastic.*`
keys, unplaced agents, and time to a list of actions. The hub runs it every
`elastic.tick` inside its 1-second tick and executes the actions:

1. Demand: the agents that `place` did not place. These are pending spawns,
   takeovers after a laptop sleeps, and parked agents that a message woke.
2. Supply: free slots on online hosts, plus `agents_per_host` for each pool
   host in `Requested` or `Booting`.
3. If demand is more than supply, start stopped hosts first, then create
   hosts in the pool that `choose` returns. Never go above `max_hosts` or a
   budget, and never create in a pool during `create_backoff`.
4. A `Ready` host with no lease and no reservation is idle. It stays until
   `idle_timeout` passes and its billing step ends within 5 minutes, because
   Hetzner bills the full hour anyway. On Fly the step is one second.
   `min_warm` hosts never go idle.
5. To remove an idle host: set `draining` on `host.{name}`, then read the
   leases again. If a lease appeared, clear `draining`. Otherwise, stop the
   host if the pool has room under `keep_stopped` and the backend has
   `StopMode::Stop`. Else delete it, and delete `host.{name}` with the
   `elastic.*` key and the view entry, as `host remove` does, so
   `borant hosts` names no box that is gone. A static host never takes this
   path.
6. Reconcile: compare `list(pool)` with the `elastic.*` keys. A host with no
   key (for example, after a hub crash during a create) gets a key in
   `Booting`. A key with no host goes away.
7. Budget: the hub adds `price_hour_usd x billed hours` to `spend.{yyyy-mm}`
   at each delete and stop, plus an estimate for live hosts. At the budget,
   the hub creates no host and notifies Chris. It never removes a busy host.
   A failed create starts the backoff, and `denied` or `invalid` notifies.

### Idle agents and safe deletes

An idle agent holds a lease, so its host never goes idle. The hub parks an
agent after `idle_timeout` with no turn, no claim, and an empty inbox. A park
is a planned stop with a final checkpoint. The lease then moves to the state
that the takeover-on-message path already handles.

A delete or a stop is safe, because a drain or a park writes a final
checkpoint to the WIP ref and releases the lease first (design sections 12
and 13). The checkpoint remote and the session log are never on a pool host.
Files that git ignores are lost (a non-goal, design section 2). A crashed
host loses at most the turn after the last checkpoint.

Laptop free slots count as supply only while the laptop is online. Add
`accept_spawns = false` as the laptop default in `host.toml`, so that a later
sleep does not move a new agent. A spawn with `--host` still goes there.

## Two ways to plug in a backend

| Way | Use it when | Examples |
|---|---|---|
| Adapter crate behind a Cargo feature [43] | The API is public and generic, other Borant users want it, and its contract tests can run in CI | `borant-fly` (feature `fly`) |
| `exec` adapter with a plugin program | The backend is private to one deployment, needs site rules (projects, firewalls, images), or is a prototype | `borant-provision-hetzner` in `borant-infra`, the CI test plugins |

## Exec plugin protocol

The `exec` adapter is in the new crate `borant-exec`. Every exec plugin in
Borant uses this protocol, for `Provisioner` and for the `SecretStore` port
of the secrets doc. Kubernetes and Docker credential helpers use the same
shape [39], [40].

1. The caller starts the program once for each operation. The program reads
   one JSON object from stdin: `{"v": 1, "op": "<name>", ...args}`.
2. The program writes one JSON object to stdout, either `{"ok": <value>}`
   or `{"error": {"code": "<code>", "message": "<text>"}}`.
3. The error codes are `unavailable` (retry later), `not_found`, `invalid`,
   `denied`, and `internal`. They map to `PortErrorKind` `Unavailable`,
   `NotFound`, `Invalid`, `Denied`, and `Other`.
4. A non-zero exit with no JSON counts as `internal`. Invalid JSON on stdout
   also counts as `internal`.
5. Stderr is for logs only. The caller (the hub or a host daemon) redacts it
   and copies it to its log at `debug`.
6. The caller sets a timeout for each op. At the timeout, it kills the
   program and reports `PortErrorKind::Timeout`.
7. The program never gets secrets in its arguments. It reads its own
   credentials from its environment or from a file. The caller starts it
   with an empty environment plus `PATH`, `HOME`, the `env` table of its
   configuration, and `BORANT_CREDENTIALS_DIRECTORY` if the configuration
   names a `credential` (the systemd credential folder of the caller unit).
8. The first request is `describe`. Its `ok` value always holds `"v"` (the
   protocol version) and `"kind"` (`"provisioner"` or `"secrets"`), plus the
   capabilities of the port. If the program returns `invalid`, a `v` that the
   caller does not support, or the wrong `kind`, the caller disables it.

For provisioner plugins, each request also carries `config`, the `config`
table of the provisioner in `hub.toml`. Any TOML value in that table, a
string, number, bool, array, or nested table, reaches the plugin as the same
JSON. The hub runs one op at a time for each provisioner instance, so a
plugin needs no locks. Default timeouts are
10 s for `describe`, 30 s for `list`, 120 s for `create`, and 60 s for the
others. `timeout_secs` sets one value for all ops. A create that times out
can still succeed. The next reconcile finds the host, and a create with the
same name returns it.

### Provisioner ops

Each pair shows a request on stdin, then a response on stdout.

```json
{"v": 1, "op": "describe", "config": {"firewall": "borant-locked"}}
{"ok": {"v": 1, "kind": "provisioner", "backend": "hetzner", "regions": ["hil", "ash", "fsn1"],
  "sizes": [{"id": "cpx31", "vcpus": 4, "mem_mb": 8192, "price_hour_usd": 0.1178, "stopped_price_hour_usd": null}],
  "typical_start_s": 120, "typical_resume_s": null, "billing_step_s": 3600, "one_host_per_agent": false, "stop_modes": []}}

{"v": 1, "op": "create", "config": {"firewall": "borant-locked"},
 "spec": {"pool": "hil-workers", "name": "borant-e1", "size": "cpx31", "region": "hil",
          "image": "label:borant.image=worker", "labels": {"borant.swarm": "main"},
          "boot": {"BORANT_HOST_NAME": "borant-e1", "BORANT_POOL": "hil-workers", "BORANT_JOIN": "<one-time>"}}}
{"ok": {"id": "61234567", "pool": "hil-workers", "name": "borant-e1", "state": "creating", "created": "2026-10-24T18:00:00Z",
        "labels": {"borant.swarm": "main", "borant.pool": "hil-workers", "borant.managed": "hub"}}}

{"v": 1, "op": "list", "config": {}, "pool": "hil-workers"}
{"ok": [{"id": "61234567", "pool": "hil-workers", "name": "borant-e1", "state": "running",
         "created": "2026-10-24T18:00:00Z", "labels": {"borant.pool": "hil-workers"}}]}

{"v": 1, "op": "delete", "config": {}, "id": "61234567"}
{"ok": null}
{"error": {"code": "denied", "message": "server 61234567 has no borant.managed=hub label"}}
```

`stop` (with `id` and `"mode": "stop"`) returns `{"ok": null}`, and `start`
(with `id`) returns the host record. A plugin that lists a mode in
`stop_modes` must answer them. An unknown op returns `invalid`. `state` is
`creating`, `running`, `stopping`, `stopped`, `deleting`, or `failed`.

## The Hetzner plugin in `borant-infra`

The plugin is `pkgs/borant-provision-hetzner.nix`, a `writeShellApplication`
script of about 150 lines with `hcloud` and `jq` [38]. The flake builds it
next to the golden image, and `modules/borant.nix` installs it on the hub.

| Op | `hcloud` call | Rule |
|---|---|---|
| `describe` | `server-type list -o json`, `location list -o json` | Prices for the types in `config` only |
| `create` | `server create --name --type --location --image <newest snapshot with the label> --label --firewall --ssh-key --user-data-from-file -` | Adds `borant.pool` and `borant.managed=hub`. Boot values go to user-data on stdin, never in argv. On `uniqueness_error`, it returns the existing server. |
| `list` | `server list -l borant.managed=hub,borant.pool=<pool> -o json` | Maps the Hetzner status to `state` |
| `delete` | `server describe`, then `server delete` | `denied` without `borant.managed=hub`. A missing server is `ok`. |

HTTP 429, 5xx, `resource_unavailable`, and `placement_error` (no capacity in
`hil`) become `unavailable`. `invalid_input` becomes `invalid`.
`unauthorized` and `forbidden` become `denied`.

### The Hetzner token

Decision: the plugin reads its own token file, not a value from the
`SecretStore` port. `just hub-credential` reads the token from the `Agents`
vault on the laptop and pushes it over Tailscale SSH to
`/var/lib/borant/credentials/hcloud-token` (mode 0400, root) on the hub box.
The `borant-hub` unit loads it with `LoadCredential`. The plugin reads
`$BORANT_CREDENTIALS_DIRECTORY/hcloud-token`, or `HCLOUD_TOKEN` when Chris
runs it by hand. Reasons:

1. It keeps the rule of the secrets doc that the hub never calls `SecretStore`.
2. It is the "secret zero" path of the secrets doc: one file, `LoadCredential`.
3. The token stays out of the `Borant` vault, which every host reads.

A rotation needs a new push and a hub restart. The token is for a separate
Hetzner project, `borant-workers`, so a plugin bug cannot delete the hub box.

### Manual use next to the hub

`just create`, `install`, `deploy`, and `destroy` stay for static boxes and
the hub box. They label servers `app=borant`, never `borant.managed=hub`, so
the plugin never lists or deletes them. A new recipe drives the plugin by
hand for tests and repairs. It reads `$HCLOUD_WORKERS_TOKEN_REF` with `op`
into `HCLOUD_TOKEN`, as the other recipes do, and pipes the request:
`just pool '{"v":1,"op":"list","config":{},"pool":"hil-workers"}'`.

## A Fly.io backend

A Fly backend needs `stop` and `start`, `billing_step`, `one_host_per_agent`,
and `agents_per_host = 1`. This doc adds all of them now, so Fly needs no
change to the port, the rules, or the hub. Write it as the crate
`borant-fly` behind the Cargo feature `fly`, because the Fly Machines API is
public and the same for every user. A prototype can be an exec plugin.

| Port op | Fly Machines API |
|---|---|
| `create` | `POST /v1/apps/{app}/machines` with `name`, `region`, `config.image`, `config.guest` (`cpu_kind`, `cpus`, `memory_mb`), `config.env` (the boot values), `config.metadata` (`borant_pool`, `borant_managed`), `config.restart.policy = "no"` |
| `list` | `GET /v1/apps/{app}/machines?metadata.borant_pool=<pool>` |
| `stop`, `start` | `POST .../{id}/stop` (or `/suspend`), `POST .../{id}/start` |
| `delete` | `DELETE .../{id}?force=true` |

The configuration is `[provisioners.fly]` with `adapter = "fly"`, `app`, and
a `fly-token` credential, plus a pool with `agents_per_host = 1`,
`keep_stopped = 4`, `idle_timeout = "5m"`, and the label `fast-start`.

What differs from Hetzner:

- One machine per agent, so packing is 1.0. A create takes seconds and a
  start under a second. Stopped machines bill only rootfs.
- The rootfs is blank at each start. Each start clones again and resumes
  from the checkpoint. Logins and the 1Password token come from Fly secrets.
- `tailscaled` joins with an ephemeral, tagged key from the boot values
  [25], [41]. NATS is on the tailnet, so Fly private networking is not used.
- The flake builds the OCI image (`borant`, `tailscaled`, git, tmux, the
  harnesses) with `dockerTools.buildLayeredImage` [42].
- Builds need `performance` CPUs, in a second pool with a `build` label.

Suspend keeps memory, so a tmux session can survive it. But it works only up
to 2 GB RAM, and a lost snapshot cold-boots the machine. The port carries
`StopMode::Suspend`, but the pool rules use only `Stop`, after a park.

## Tests

| Test | Where | Runs |
|---|---|---|
| `provisioner_contract(p, pool, spec)` | `borant-core::testing` | In each adapter crate |
| `rules::pool`: demand, choice of pool, idle timing, billing step, budget, reconcile | `borant-core` | `just test-crate borant-core pool` |
| `MemoryProvisioner` passes the contract. The hub executor runs on it with a fake clock. | `borant-memory`, `borant-hub` | Unit |
| `ExecProvisioner` with `tests/plugins/provision-file.sh` passes the contract | `borant-exec` | Unit, needs `jq` |
| Docker scenario `elastic` with `tests/e2e/plugins/provision-docker.sh` | `tests/e2e/scenarios` | CI |

The contract suite asserts six points. `describe` returns version 1, a size,
and a region. `create` returns the pool, the name, and the spec labels, and
a second `create` with the same name returns the same id. `list(pool)` shows
the host, and `list` of another pool does not. `delete` removes the host
within a timeout, and a repeat or an unknown id is `Ok`. An unlisted size
gives `Invalid`. If `stop_modes` has `Stop`, `stop` and `start` work.

`MemoryProvisioner` keeps hosts in a map, with a start delay, a stop flag,
and a create failure switch. `provision-file.sh` keeps a JSON file per
host. `provision-docker.sh` maps `create`, `list`, and `delete` to
`docker run -d --label borant.pool=...` of the e2e host image,
`docker ps --filter label=...`, and `docker rm -f`. The hub container mounts
the Docker socket.

The `elastic` scenario starts the hub with no workers and one pool with
`agents_per_host = 2` and `max_hosts = 2`. It spawns three agents and expects
two containers, then expects them to go away when the agents go idle. It
kills a container during a turn and expects a takeover and a delete. Thus CI
tests the exec path, the rules, and the hub with no cloud account.

## Phased plan

Phase 0, no code (this week): make sure that `borant-2` runs no agents
(`borant status`), then run `just destroy borant-2` in `borant-infra`. This
saves about $74 per month. Then measure one create from a snapshot, from
`hcloud server create` to a host heartbeat, for less than $0.20.

Phase 1, operations in `borant-infra`: build the golden image (secrets doc).
Create `borant-hub` (CPX11), move the hub with `borant hub move`, then delete
`borant-1`. This saves about $53 per month more.

Phase 2, the port and pools in `borant`: the `Provisioner` port, `PoolName`,
the two new `PortErrorKind` values, `provisioner_contract`, and
`MemoryProvisioner`. Then `rules::pool`, pools in `hub.toml`, the hub
executor, the `elastic.*` and `spend.*` keys, `pool` in `HostInfo`, the two
placement terms, parking, and `accept_spawns`.

Phase 3, the exec adapter in `borant`: `borant-exec` with `ExecProvisioner`,
the codec, `credential`, and timeouts, then the test plugins and the
`elastic` scenario.

Phase 4, the Hetzner plugin in `borant-infra`: the `borant-workers`
project and its token, `just hub-credential`, `borant-provision-hetzner`,
`just pool`, and the hub `hub.toml` with one pool at `max_hosts = 1`. Raise
`max_hosts` after a week with no orphaned box.

Phase 5, only if needed: set `min_warm = 1` or add `borant-fly` and a Fly
pool if cold starts hurt. Add one AX41 as a static EU host if use goes above
about 1,200 agent-hours per month.

Cut from this design: Hetzner code in the open-source repo, a Fly adapter
now, a microVM platform, suspend with live sessions, rescale, warm-host
schedules, live price comparison, a cost rule by expected job length,
long-running plugins, plugin discovery, and one Hetzner box per agent.

## Risks

| Risk | Effect | Mitigation |
|---|---|---|
| No capacity in a region | Create fails | `unavailable`, backoff, notify. A second pool in another region. |
| An orphaned host | Money each hour | Labels, reconcile each tick, `boot_timeout`, budgets |
| A rule bug deletes a host with work | Lost turn | Delete only after `draining` and a second lease read. Checkpoints. |
| A plugin hangs or prints junk | One pool stops | Op timeouts, `internal`, backoff. Other pools keep working. |
| A stolen hub token creates hosts | Money | Separate project, root-only credential file, budgets, `max_hosts` |
| Cold start longer than measured | Agents wait | `min_warm`, or a Fly pool |

## Open questions for Chris

1. Were `borant-1` and `borant-2` created before 2026-06-15? If so, they
   keep the old €20.99 price (see the invoice), and you do not delete them.
2. How many agent-hours per month, and how many agents at the peak? This
   decides B against D.
3. Can workers run in the EU (3.6 times cheaper, about 150 ms to
   `borant attach`)? With pools, this is one more `[[pools]]` entry.
4. Can unattended spawns land on an awake laptop, or do they go to a pool by
   default (`accept_spawns = false`)? What budget and `max_hosts` per pool?
5. Is a pushed token file on the hub box acceptable for the Hetzner token?
   The other choice is a hub-side `SecretStore` with its own vault.
6. Do you want `borant-fly` only in phase 5, or an earlier Fly prototype as
   an exec plugin, to measure the start time?

## Sources

1. Hetzner billing FAQ: https://docs.hetzner.com/cloud/billing/faq/
2. Hetzner Regular Performance (CPX): https://www.hetzner.com/cloud/regular-performance/
3. Hetzner Cost-Optimized (CX, CAX): https://www.hetzner.com/cloud/cost-optimized/
4. Hetzner General Purpose (CCX): https://www.hetzner.com/cloud/general-purpose/
5. Hetzner price adjustment: https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/
6. Northflank on the Hetzner increase: https://northflank.com/blog/hetzner-cloud-server-price-increases
7. Hetzner Cloud server FAQ: https://docs.hetzner.com/cloud/servers/faq/
8. Snapshot price (third party, 2026-10-20): https://costgoat.com/pricing/hetzner
9. Hetzner IPv4 prices: https://docs.hetzner.com/general/others/ipv4-pricing/
10. Hetzner Cloud API spec: https://docs.hetzner.cloud/cloud.spec.json
11. Cheap Hetzner lines unavailable: https://www.vincentschmalbach.com/hetzner-cheap-cloud-unavailable-price-increases/
12. Hetzner AX matrix: https://www.hetzner.com/dedicated-rootserver/matrix-ax/
13. Hetzner server auction: https://www.hetzner.com/sb/
14. Hetzner cancellation: https://docs.hetzner.com/general/billing-and-account-management/cancellation/cancellations-overview
15. Fly.io pricing: https://docs.fly.io/about/pricing
16. Fly.io pricing with region multipliers: https://fly.io/pricing.md
17. Fly.io regions: https://docs.fly.io/reference/regions/
18. Fly.io 2026 fees (competitor source): https://sota.io/blog/fly-io-hidden-costs-2026
19. Fly.io CPU performance: https://docs.fly.io/machines/cpu-performance/
20. Fly.io machine sizing: https://docs.fly.io/machines/guides-examples/machine-sizing/
21. Fly.io Machines overview: https://docs.fly.io/machines/overview/
22. Fly.io rootfs size (community): https://community.fly.io/t/is-it-possible-to-extend-the-root-volume-beyond-8gb/21055
23. Fly.io suspend and resume: https://docs.fly.io/reference/suspend-resume/
24. Fly.io Machines API (create fields, `metadata.{key}` list filter, stop, suspend, start): https://docs.fly.io/machines/api/machines-resource/
25. Tailscale on Fly.io: https://tailscale.com/kb/1132/flydotio
26. Fly.io Sprites: https://fly.io/sprites.md
27. Northflank pricing: https://northflank.com/pricing
28. Railway pricing: https://docs.railway.com/reference/pricing/plans
29. E2B pricing: https://e2b.dev/pricing
30. Daytona pricing: https://www.daytona.io/pricing
31. Modal pricing: https://modal.com/pricing
32. Cloudflare Containers pricing: https://developers.cloudflare.com/containers/pricing/
33. AWS Fargate pricing: https://aws.amazon.com/fargate/pricing/ (rates from https://cloudburn.io/blog/aws-fargate-pricing)
34. Firecracker and Cloud Hypervisor: https://github.com/firecracker-microvm/firecracker and https://github.com/cloud-hypervisor/cloud-hypervisor. Nested virtualization: https://github.com/firecracker-microvm/firecracker/issues/1721
35. microvm.nix: https://github.com/microvm-nix/microvm.nix
36. Claude prices (Claude API skill table, cached 2026-10-06): https://docs.claude.com/en/docs/about-claude/pricing
37. DeepSeek V4.1 Flash prices (secondary): https://www.requesty.ai/models/deepseek/deepseek-v4.1-flash and https://propakistani.pk/2026/09/11/deepseek-v4-1-flash-launches-with-lower-prices-and-native-vision/
38. hcloud CLI: https://github.com/hetznercloud/cli. nixpkgs `writeShellApplication`: https://nixos.org/manual/nixpkgs/stable/#trivial-builder-writeShellApplication
39. Kubernetes client-go credential plugins: https://kubernetes.io/docs/reference/access-authn-authz/authentication/#client-go-credential-plugins
40. Docker credential helpers: https://github.com/docker/docker-credential-helpers
41. Tailscale ephemeral nodes: https://tailscale.com/kb/1111/ephemeral-nodes
42. nixpkgs `dockerTools`: https://nixos.org/manual/nixpkgs/stable/#sec-pkgs-dockerTools
43. Cargo features: https://doc.rust-lang.org/cargo/reference/features.html
44. Local, read on 2026-10-24: `crates/borant-core/src/ports/mod.rs` and `launch.rs`, `crates/borant-core/src/rules/schedule.rs`, `HostInfo` in `model.rs`, the hub tick in `crates/borant-hub/src/lib.rs`, and the `borant-infra` justfile.

Not verified: the time to create a Hetzner server from a snapshot, the IPv4
price for dedicated servers, the Fly rootfs limit, whether Fly machine names
are unique per app, Sprites plan fees, omp RAM use, and all LLM token counts
per hour.
