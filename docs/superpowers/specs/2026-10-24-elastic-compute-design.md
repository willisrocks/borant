# Elastic compute and economics for Borant

Date: 2026-10-24. Status: draft for review. Track 2 of 5.

Related designs from the same review:

- `2026-10-24-secrets-and-host-bootstrap-design.md`: the golden NixOS image,
  zero-touch harness logins, and the host readiness check. This doc uses the
  image and does not design it.
- `2026-10-24-orchestration-api-and-sandboxes-design.md`: the spawn API,
  placement in general, sandboxes, and launchers.
- `2026-10-24-observability-design.md`: dashboards and cost metrics.
- `2026-10-24-durable-orchestration-vision-design.md`: the long roadmap.

## Summary

Hetzner Cloud has no "suspend" that stops the bill. A powered-off server costs
the full price until you delete it. Thus scale to zero on Hetzner means
"delete the worker, then create it again from a snapshot". The recommendation
is a small always-on hub box, plus elastic worker boxes that a new
provisioner port in the hub creates on demand and deletes when idle. On the
June 2026 prices, the two always-on CPX31 boxes cost about $148 per month,
and today they run no agents. The elastic design costs about $24 to $81 per
month for 50 to 1,000 agent-hours. Fly.io Machines cost about the same per
agent-hour as Hetzner hourly boxes, so they give no economic gain. They give a
faster start (seconds, not minutes). A self-hosted "Fly clone" with
Firecracker is not worth the operations work for one person. A Hetzner
dedicated box (EU only) becomes the cheapest option above about 1,200
agent-hours per month. LLM API spend is 5 to 10 times the compute spend for
Claude models at API prices, but only about half of it for DeepSeek Flash.

## Goals and non-goals

Goals:

1. Stop paying for idle worker boxes.
2. Start a worker box without a human when the hub has work and no free slot.
3. Delete a worker box only when no agent can lose work.
4. Keep one adapter (Hetzner) now, and keep the port open for Fly.io later.
5. Show Chris the cost formula, so he can plug in his own numbers.

Non-goals:

- A highly available hub. The hub stays a single point of failure (design
  section 20). This doc only makes the hub box smaller.
- The golden image and the harness logins. The secrets-and-host-bootstrap doc
  owns them.
- Sandboxes and isolation between agents. The orchestration doc owns them.
- A multi-cloud scheduler that compares live prices.

## Facts on 2026-10-24

### Hetzner Cloud billing

| Fact | Value | Source |
|---|---|---|
| Powered-off server | Billed in full until deleted ("independent of their state") | [1] |
| Hourly billing | Lower of hourly rate x hours and the monthly cap. Hetzner rounds each started hour up. | [1] |
| Snapshots | About €0.0143 per GB per month, on the compressed size | [1], [8] |
| Backups | 20% of the server price | [1] |
| Primary IPv4 | €0.50 per month (€0.0008 per hour), billed also when unassigned | [9] |
| Primary IPv6 | Free | [1], [9] |
| Price increase | New prices from 2026-06-15 for new orders and rescales. Old orders keep old prices. | [5], [6] |
| Nested virtualization | "No, this is not possible on cloud server." | [7] |
| Rescale | The server must be off. You cannot shrink the disk. A rescale moves the server to the new prices. | [7], [10] |
| Snapshot to another location | Allowed: create a new server from the snapshot | [7] |
| API rate limit | 3,600 requests per hour per project | [10] |
| Time to create from a snapshot | Not published. Estimate 1 to 2 minutes to a running NixOS host (unverified). Phase 0 measures it. | none |

The API calls (all under `https://api.hetzner.cloud/v1`, from [10]):

| Action | Call |
|---|---|
| Create from snapshot | `POST /servers` with `image`, `server_type`, `location`, `labels`, `user_data` (32 KiB max), `firewalls`, `ssh_keys`, `public_net` |
| Delete | `DELETE /servers/{id}` |
| Snapshot | `POST /servers/{id}/actions/create_image` (shut down first for a clean disk) |
| Rescale | `POST /servers/{id}/actions/change_type` (server off) |
| Find the image | `GET /images?type=snapshot&label_selector=borant.image=worker` |
| Track progress | `GET /actions/{id}` |

### Hetzner Cloud prices

All prices exclude VAT and include the IPv4 address. Hillsboro (`hil`) and
Ashburn (`ash`) have their own price list, and it is much higher than the EU
list after the June increase [2], [5].

Correction (2026-10-24, from the Hetzner API for Chris's `borant` project):
the account bills in USD with no VAT, and the CPX line runs in the EU too.
The table below replaces the web-page figures. `fsn1` stands for the EU
regions (`nbg1` and `hel1` have the same prices). Snapshots cost $0.0199 per
GB per month, and a primary IPv4 in `hil` costs $0.60 per month.

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

The cost model in this doc used $74.08 per month for the CPX31 in `hil`, so
its US totals are about 1% high. The EU column makes EU workers 3.6 times
cheaper than `hil` workers of the same size, which strengthens the question
about EU workers at the end of this doc.

Notes:

- The CX and CAX "cost-optimized" lines showed "currently unavailable" on
  2026-10-24 [3], [11]. The CCX create buttons showed the same [4].
- CAX (Arm) runs only in the EU. Claude Code and omp ship Linux Arm builds,
  but the flake builds `x86_64-linux` today.
- The CPX31 in `hil` costs 3.0 times its pre-June price (€20.99 before) [5].
  It reaches its monthly cap after about 624 hours.
- This doc uses Hetzner's own USD to EUR ratio for US servers ($73.49 for
  €62.49 [5], so 1.176). CPX31 in `hil` is then $74.08 per month or $0.1187
  per hour.

### Hetzner dedicated

| Offer | CPU / RAM / disk | Price per month | Setup | Source |
|---|---|---|---|---|
| AX41 | Ryzen 5 3600, 6 cores / 64 GB / 2x 512 GB NVMe | €59.00 | €0 | [12] |
| AX42 | Ryzen 7 PRO 8700GE, 8 cores / 64 GB ECC | €79 to €99 | €49 | [12] |
| AX102 | Ryzen 9 7950X3D, 16 cores / 128 GB ECC | €159 to €259 | €129 | [12] |
| Server auction | Used hardware, no setup fee | From €67.70 | €0 | [13] |

Dedicated servers run only in Germany and Finland. None is in the US [12].
They are bare metal, so KVM, Firecracker, and Cloud Hypervisor work. Billing
is pro rata with a monthly cap, and the longest notice is "30 days to the end
of the month" [14]. The IPv4 price for dedicated servers is extra and was not
found (unverified, about €2).

### Fly.io Machines

| Fact | Value | Source |
|---|---|---|
| Billing | Per second while started | [15] |
| Shared vCPU | $0.69 per month in `iad` (derived from presets) | [15], [16] |
| Performance vCPU | $21.00 per month in `iad` (derived) | [15], [16] |
| RAM | $6.00 per GB per month | [15] |
| Region multiplier | `sjc` 1.192, `lax` 1.200, `dfw` 1.250. Fly has no `sea` region. | [16], [17] |
| Stopped or suspended machine | $0.15 per GB per month of rootfs only | [15] |
| Volumes | $0.15 per GB per month, also when detached | [15] |
| Egress | $0.02 per GB in North America. Inbound is free. | [15] |
| Private traffic between regions | $0.01 per GB in, $0.02 per GB out, since February 2026 (secondary source) | [18] |
| Shared CPU limit | 6.25% baseline per vCPU, 5 s starting burst, 500 s burst maximum | [19] |
| RAM per vCPU | Shared: 256 MB to 2 GB. Performance: 2 GB to 8 GB. | [20] |
| Start a stopped machine | "Usually well under a second" | [21] |
| Create a machine | "Low double digit seconds". No capacity guarantee. | [21] |
| Rootfs | "Blank slate on every startup". About 8 GB maximum (community posts). | [21], [22] |
| Suspend and resume | Firecracker snapshot, resume in "a few hundred ms", only up to 2 GB RAM. Snapshots can be lost, then the machine cold-boots. | [23] |
| Run time limit | None stated | [21] |
| Machines API | `POST /v1/apps/{app}/machines`, `.../{id}/start`, `.../stop`, `.../suspend`, `GET .../wait`, `DELETE .../{id}` | [24] |
| Tailscale | Run `tailscaled` in the image with an ephemeral, tagged auth key from a Fly secret | [25] |
| Image | Any OCI image. A Nix-built image (`dockerTools.buildLayeredImage` or nix2container) pushed to `registry.fly.io` fits. | [24] |

The shared CPU limit matters. A new `shared-cpu-2x` machine has 5 seconds of
burst, so the first `npm install` or `cargo build` runs at 12.5% of two cores.
Builds need performance CPUs on Fly. GPUs are not relevant for Borant,
because the models run behind remote APIs.

Fly also sells Sprites: persistent Firecracker VMs for agents. A Sprite bills
measured CPU ($0.0385 per CPU-hour) and measured memory ($0.021875 per
GB-hour), and nothing while it sleeps [26]. A Sprite is one machine for one
agent, and Borant then needs one host daemon in each Sprite.

### Other on-demand options

Each row prices one agent slot of 2 vCPU and 4 GB for one hour.

| Option | Price per slot-hour | Fit for Borant | Source |
|---|---|---|---|
| Hetzner CPX31 `hil`, hourly, 4 slots | $0.030 (full box) | Good. Today's stack. | [2] |
| Fly `shared-cpu-2x` 4 GB `sjc` | $0.042 | Good for idle agents, slow for builds | [15], [16] |
| Fly `performance-1x` 4 GB `sjc` | $0.075 | Good | [15], [16] |
| Fly Sprites (measured: 0.1 CPU, 1 GB) | about $0.033 | Possible. One host daemon per Sprite. Plan fees not verified. | [26] |
| Northflank | $0.067 | Possible. Long-running services, BYOC. | [27] |
| Railway (VM rates) | $0.27 | Poor. Container rates are lower, but billing is on usage. | [28] |
| E2B | $0.166, Pro plan $150 per month, 24 h session cap | Poor | [29] |
| Daytona | $0.166 | Poor | [30] |
| Modal sandboxes | $0.238 | Poor | [31] |
| Cloudflare Containers `standard-3` | about $0.08, made for request-driven work | Poor | [32] |
| AWS Fargate (`us-east-1` rates) | $0.099, `us-west-2` not verified | Poor. Much setup. | [33] |

## Agent sizing

Measured on 2026-10-24:

- Claude Code processes on the laptop use 0.46 to 0.68 GB RSS, and one long
  session uses 1.5 GB. Idle CPU is 0.1% to 0.6%.
- `borant-1` uses 870 MB with NATS, the hub, and the host daemon, and no
  agents. `borant-2` uses 503 MB. Both had a load average of 0.00 and no
  tmux sessions. The fleet was idle.

The sizing for this doc is one slot per agent: 2 GB RAM and 1 vCPU, plus a
shared build reserve on the box. A CPX31 (8 GB, 4 vCPU) holds 4 agents, which
matches `max_agents = 4`. omp is a bun binary and uses less RAM than Claude
Code (unverified). A repository build is the peak, not the harness.

## Options compared

| Option | Idea | Cold start | Gain | Cost to build |
|---|---|---|---|---|
| A. Keep 2 always-on CPX31 | Today | None | Nothing to build | None |
| B. Small hub plus Hetzner elastic workers | Delete idle workers, create from snapshot | 2 to 4 min (unverified) | Pays per hour of use | Provisioner port, adapter, image |
| C. Small hub plus Fly Machines workers | Start a stopped machine per agent | Seconds | Faster start, per-second billing | Provisioner adapter, OCI image, Tailscale in the image |
| D. One dedicated AX41 with microVMs or plain agents | Many cheap slots in the EU | None | Cheapest slots at high use | A new box type, EU latency |
| E. Self-hosted Fly clone | Firecracker pool on dedicated metal | About 1 s | Fast and cheap at scale | Weeks of work, ongoing upkeep |

### Self-hosted "Fly clone"

Recommendation: do not build it. The economics come from the price of the
dedicated box, not from the microVMs. A plain `borant host` on an AX41 with
`max_agents = 20` gets the same slots.

| Tool | State on 2026-10-24 | Note |
|---|---|---|
| Firecracker v1.17.0 | Active | Needs `/dev/kvm`, so dedicated metal only. Maintainers do not support nested virtualization. [34] |
| Cloud Hypervisor v54.0 | Active | Snapshot and restore. Can expose virtualization to guests. [34] |
| microvm.nix | Active, about 3,000 stars, no tagged releases | Declarative NixOS microVMs with eight hypervisors. Shares `/nix/store`. Snapshots not documented. [35] |
| flintlock | Active, small | gRPC service for microVMs (Liquid Metal) |
| firecracker-containerd | Active | containerd runtime for Firecracker |
| Kata Containers | Active | VM isolation for containers, best with Kubernetes |
| Weave Ignite | Archived since 2023-12 | Do not use |
| NixOS containers (systemd-nspawn) | Part of NixOS | No VM isolation, almost no cost |

Snapshot restore (milliseconds) does not help much here. The slow part is a
new Hetzner box (minutes), not the VM boot (about 1 s) or the harness start
(a few seconds). If the orchestration doc asks for isolation later, use
microvm.nix with Cloud Hypervisor on a dedicated box. Ops estimate for one
person (unverified): 2 to 4 weeks to build, then a few hours each month.

## Cost model

### Formulas

```
H      = live agent-hours per month (a session holds a lease)
slot_h = price per server-hour / max_agents
pack   = used slots / paid slots, including idle tails (0.5 assumed)

A (always on) = n_boxes * monthly_price
B (Hetzner)   = hub + H * slot_h / pack + snapshots
C (Fly)       = hub + H * fly_hour * 1.05 + pool * rootfs_gb * 0.15
D (dedicated) = dedicated_monthly + hub (or 0 if the hub runs on it)
LLM           = H * active_share * tokens_cost_per_active_hour
```

Inputs in USD:

- Hub: CPX11 in `hil`, $21.16 per month. It has enough RAM for NATS and the
  hub (870 MB measured on `borant-1`).
- B: CPX31 at $0.1187 per hour with 4 slots. With `pack = 0.5`, a slot costs
  $0.059 per agent-hour. Snapshots of about 8 GB cost about $0.13 per month.
- C: `shared-cpu-2x` 4 GB in `sjc` at $0.0420 per hour. The factor 1.05 is a
  5-minute idle tail per session. Eight stopped machines with a 3 GB rootfs
  cost $3.60 per month. The build profile is `performance-1x` 4 GB at
  $0.0745 per hour.
- D: AX41 at about $71 per month with IPv4, plus the hub.

### Scenarios

| Agent-hours per month | A: 2x CPX31 | B: hub + Hetzner elastic | C: hub + Fly shared | C: hub + Fly performance | D: AX41 + hub |
|---|---|---|---|---|---|
| 50 | $148 | $24 | $27 | $29 | $92 |
| 200 | $148 | $33 | $34 | $41 | $92 |
| 1,000 | $148 | $81 | $69 | $103 | $92 |
| Concurrent agents at most | 8 | 4 per box, `max_hosts` boxes | `pool` | `pool` | about 24 |

Break-even points:

- B beats A below about 2,150 agent-hours per month.
- B and C are within $12 of each other at 1,000 agent-hours. This is a tie.
  Choose on start time and build speed, not on price.
- D beats B above about 1,200 agent-hours, and beats C above about 1,500.
- Without the price increase, A cost $50 per month. A box created before
  2026-06-15 keeps its old price, and a delete loses that price.

### LLM spend for scale

Assumptions for one active agent-hour (unverified estimates): 40 requests,
3.2 M cache-read input tokens, 0.3 M cache-write input tokens at 1.25 times
the input price, and 60 k output tokens. Half of each live hour is active.

| Model | Price per M tokens (in / out / cache read) | Per live agent-hour | At 200 agent-hours | Compute share with B |
|---|---|---|---|---|
| Claude Opus 5.5 | $4 / $20 / $0.20 [36] | $1.67 | $334 | 9% |
| Claude Sonnet 5.5 | $2 / $10 / $0.20 [36] | $1.00 | $200 | 14% |
| DeepSeek V4.1 Flash, peak | $0.30 / $1.20 / $0.006 [37] | $0.09 | $18 | 65% |

The LunaRoute markup is not known. A Claude subscription makes the Claude
rows a flat fee. Thus compute is a small part of the bill for Claude agents,
and a large part for omp agents on DeepSeek Flash.

## Recommended design

Choose option B. Keep the port open for option C. Move to option D only if
use goes above about 1,200 agent-hours per month and EU latency is acceptable.

### Topology

- `borant-hub`: a CPX11 in `hil`, always on. It runs NATS, `borant hub`, and
  the provisioner. It runs no agents (`max_agents = 0`, or no host daemon).
- Elastic workers: CPX31 in `hil`, created from the newest snapshot with the
  label `borant.image=worker`. Names are `borant-e1`, `borant-e2`, and so on.
- Laptops: static hosts. The provisioner never creates or deletes them.

Put elastic workers in their own Hetzner project, for example
`borant-workers`. A Hetzner API token covers one project. Thus a bug in the
provisioner cannot delete the hub box.

### Provisioner port

The provisioner is a port in `borant-core`, and the hub daemon calls it from
its tick. The hub already knows the demand, because `place_agent` returns
false when no host qualifies, and the tick retries pending spawns, pending
moves, and takeovers. A stopped machine on Hetzner costs the full price, so
the port has no stop or start.

```rust
#[async_trait]
pub trait Provisioner: Send + Sync {
    /// Agent slots that one new machine gives (its max_agents).
    fn slots(&self) -> u32;
    /// Orders a machine. Returns when the provider accepted the order.
    async fn create(&self, name: &HostName) -> PortResult<Machine>;
    /// Deletes the machine and its disk. Deleting a missing machine is Ok.
    async fn delete(&self, name: &HostName) -> PortResult<()>;
    /// Lists the machines that this provisioner created, by label.
    async fn list(&self) -> PortResult<Vec<Machine>>;
}

pub struct Machine {
    pub name: HostName,
    pub state: MachineState,
    pub created: Timestamp,
}

pub enum MachineState { Creating, Running, Deleting, Failed(String) }
```

The Hetzner adapter maps `create` to `POST /servers` with these values:

- `image`: the newest snapshot with `borant.image=worker`.
- `labels`: `borant.elastic=true` and `borant.swarm=<instance>`.
- `firewalls`: `borant-locked` (no inbound rules).
- `public_net`: IPv4 on. A box without IPv4 cannot clone from GitHub
  (unverified for 2026).
- `user_data`: the host name and the one-time join data. The
  secrets-and-host-bootstrap doc defines its format.

`list` uses `label_selector=borant.elastic=true`. `delete` refuses any server
without that label.

Configuration in `hub.toml`:

```toml
[provisioner]
adapter = "hetzner"
max_hosts = 2            # elastic boxes at most
min_warm = 0             # boxes that stay up when idle
idle_timeout = "20m"
boot_timeout = "10m"
lost_timeout = "10m"
create_backoff = "5m"    # after a failed create
monthly_budget_usd = 60  # no new box above this estimate

[provisioner.hetzner]
token_file = "/var/lib/borant-secrets/hcloud-token"
location = "hil"
server_type = "cpx31"
max_agents = 4
image_label = "borant.image=worker"
firewall = "borant-locked"
name_prefix = "borant-e"
```

### State of each elastic host

The hub writes one key for each elastic host, `elastic.{host}`, with the
phase, the creation time, the last busy time, and the end of the paid hour.
The hub is still the only writer.

```
            demand                 API accepts          host heartbeat
 (none) ----------> Requested -----------------> Booting --------------> Ready
                                                   |                        |
                                     boot_timeout  |      idle and past     |
                                                   v      the paid hour     v
                                                 Failed               Draining
                                                   |                        |
                                                   +-----> Deleting <-------+
                                                              |   0 leases
                                                              v
                                                           (none)
 Ready --(no heartbeat for lost_timeout)--> Lost --> Deleting
```

Rules for each tick (every 10 seconds):

1. Demand: count the agents that `place_agent` did not place in this tick.
   These are pending spawns, takeovers after a laptop sleeps, and parked
   agents with a message that wakes them.
2. Supply: count the free slots on online hosts, plus `slots()` for each
   host in `Requested` or `Booting`.
3. If demand is more than supply, create `ceil((demand - supply) / slots)`
   hosts. Do not go above `max_hosts` or the budget. Do not create during
   `create_backoff` after a failure.
4. A `Ready` host is idle when it holds no lease and no reservation. An idle
   host stays until `idle_timeout` passes and its paid hour is within 5
   minutes of its end. Hetzner bills the full hour anyway, so the box gives
   free warm capacity until then. `min_warm` hosts never go idle.
5. To delete: set `draining` on `host.{name}`, then read the leases again. If
   a lease appeared in the meantime, clear `draining` and keep the host.
   Otherwise, call `delete` and remove `host.{name}`.
6. Reconcile: compare `list()` with the `elastic.*` keys. A labeled server
   with no key (for example, after a hub crash during a create) gets a key in
   `Booting`, and the normal timeouts then apply. A key with no server goes
   away.

A box that never joins (bad image, no capacity in `hil`) costs at most one
hour, because `boot_timeout` deletes it. If create fails with
`placement_error`, the hub logs the error and notifies Chris.

### Placement changes

The placement score today prefers the host with fewer active agents. That
spreads agents over all boxes, so no elastic box goes idle. Change the score
for elastic hosts:

1. The same harness wins (no change).
2. A static host (VPS that is always on, or an awake laptop) wins over an
   elastic host.
3. Among static hosts, fewer active agents win (no change).
4. Among elastic hosts, more active agents win. This packs the boxes.

Add `elastic: bool` to `HostInfo`. The host daemon reads it from its
configuration, and the golden image sets it.

### Idle agents on an elastic box

An idle agent with no claims holds a lease, so its box never goes idle. The
hub parks such an agent. A park is a planned stop: the session ends its turn,
the daemon writes a final checkpoint, and the lease moves to a state that the
takeover-on-message path already handles. When a message wakes the agent,
the hub places it again, and the provisioner creates a box if necessary.
Park after `idle_timeout` with no turn, no claim, and an empty inbox.

### Why a delete is safe

- A planned drain or park writes a final checkpoint to the WIP ref and
  releases the lease before the delete (design sections 12 and 13).
- The checkpoint remote must never be on an elastic box. Use GitHub or a bare
  repository on the hub.
- The session log lives on the hub. The next session reads it through
  `session_history`.
- Files that git ignores (`.env`, `node_modules`, build output) are lost. The
  next host builds again. This is a known non-goal (design section 2).
- A lost box (crash) loses at most the turn after the last checkpoint. The
  hub takes over after the grace period and deletes the box after
  `lost_timeout`.

### How a new host registers

The image comes from the secrets-and-host-bootstrap doc. This doc needs only
these properties from it:

1. The image holds no Tailscale node state. Each box joins with a new,
   ephemeral, tagged key from `user_data`. A box from a snapshot of a joined
   box has the same node key, and Tailscale then sees two nodes as one.
2. The host name comes from `user_data`, not from the flake. The host daemon
   reads `BORANT_HOST_NAME`.
3. Harness logins work with no browser.
4. The image is built on a CPX11 (40 GB disk). A snapshot keeps its
   `disk_size`, and Hetzner cannot create a server with a smaller disk than
   the image (unverified). A 40 GB image fits every type.

The host daemon starts at boot and sends a heartbeat. The hub sees the new
`host.{name}` key and moves the host to `Ready`.

### Laptops

Laptops are free compute while they are awake. The provisioner treats them as
static hosts. It never creates or deletes them, and it counts their free
slots as supply only while they are online. When a laptop sleeps, its agents
become takeovers. If no static host has a slot, the provisioner creates an
elastic box. This is the main wake path for "Chris closes the laptop and
walks to work".

An empty laptop wins placement for a spawn today, and a later sleep then
moves the agent again. Add `accept_spawns = false` to the laptop
`host.toml` as the default. A spawn with `--host` still goes there.

### Fly.io adapter (later)

The same port fits Fly. `create` starts a stopped machine from a pool, or
creates one. `delete` stops it, and the pool keeps it for the next start.
Each machine runs one host daemon with `max_agents = 1`, so packing is 1.0.
The image is a Nix-built OCI image with `borant`, `tailscaled`, git, tmux,
and the harnesses. The rootfs is blank at each start, so logins come from Fly
secrets in the environment. Do not use suspend: it allows only 2 GB, and a
lost snapshot cold-boots the machine anyway.

## Phased plan

Phase 0, no code (this week):

1. Make sure that `borant-2` runs no agents (`borant status`), then run
   `just destroy borant-2`. This saves about $74 per month. The flake can
   create it again in about 10 minutes.
2. Measure one create from a snapshot: time from `hcloud server create` to a
   host heartbeat. The cost is less than $0.20.

Phase 1, operations only:

1. Build the golden image with a new `just image` recipe (from the
   secrets-and-host-bootstrap doc).
2. Create `borant-hub` (CPX11) and move the hub with `borant hub move`. Then
   delete `borant-1`. This saves about $53 per month more.
3. Add `just up <name>` and `just down <name>` recipes that create and delete
   elastic boxes by hand.

Phase 2, the provisioner:

1. Add the `Provisioner` port, the Hetzner adapter, the `elastic.*` key, and
   the tick rules, with `max_hosts = 1`.
2. Change the placement score and add `elastic` and `accept_spawns`.
3. Add parking for idle agents.
4. Test with a fake provisioner in the Docker multi-host tests. Focused tests
   only outside CI.

Phase 3, only if needed:

- If cold starts hurt, set `min_warm = 1` or add the Fly adapter.
- If use goes above about 1,200 agent-hours per month, add one AX41 as a
  static host in the EU.

Cut from this design: the Fly adapter now, a Firecracker or microVM platform,
Fly suspend, rescale for more CPU (it needs a power-off), schedules for warm
boxes, price-aware placement across providers, auction bidding, IPv6-only
boxes, and one Hetzner box per agent.

## Risks

| Risk | Effect | Mitigation |
|---|---|---|
| No CPX31 capacity in `hil` | Create fails | Log, notify, back off. Allow a second `server_type` or `ash` later. |
| An orphaned box | Money each hour | Labels, reconcile each tick, `boot_timeout`, budget cap |
| A provisioner bug deletes a box with work | Lost turn | Delete only after `draining` and a second lease read. Checkpoints. |
| Cold start longer than measured | Agents wait | `min_warm`, or Fly for fast starts |
| Hetzner prices change again | The model is wrong | The formula takes new prices |
| The image ages | Old harness versions | Rebuild the image on each flake change. Claude Code updates itself at start. |
| A hub token that can create servers | A stolen hub creates boxes | Separate project, budget cap, `max_hosts` |

## Open questions for Chris

1. Were `borant-1` and `borant-2` created before 2026-06-15? If so, they keep
   the old €20.99 price, and you do not delete them. The invoice shows the
   price.
2. How many agent-hours per month do you expect, and how many agents at the
   peak? This decides B against D.
3. Can workers run in the EU? EU cloud boxes cost about half, and an AX41
   gives about 24 slots for $71. `borant attach` then has about 150 ms of
   latency.
4. Can unattended spawns land on an awake laptop, or do they go to the cloud
   by default (`accept_spawns = false`)?
5. What monthly compute budget and `max_hosts` do you want? Is a separate
   Hetzner project for workers acceptable?
6. Do you use Claude Code on a subscription or on the API? This changes the
   LLM share of the bill.
7. Is a cold start of 2 to 4 minutes acceptable after a laptop sleeps? The
   2-minute grace period comes first.

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
24. Fly.io Machines API: https://docs.fly.io/machines/api/machines-resource/
25. Tailscale on Fly.io: https://tailscale.com/kb/1132/flydotio
26. Fly.io Sprites: https://fly.io/sprites.md
27. Northflank pricing: https://northflank.com/pricing
28. Railway pricing: https://docs.railway.com/reference/pricing/plans
29. E2B pricing: https://e2b.dev/pricing
30. Daytona pricing: https://www.daytona.io/pricing
31. Modal pricing: https://modal.com/pricing
32. Cloudflare Containers pricing: https://developers.cloudflare.com/containers/pricing/
33. AWS Fargate pricing: https://aws.amazon.com/fargate/pricing/ (rates from https://cloudburn.io/blog/aws-fargate-pricing)
34. Firecracker and Cloud Hypervisor releases: https://github.com/firecracker-microvm/firecracker and https://github.com/cloud-hypervisor/cloud-hypervisor (GitHub API, 2026-10-24). Nested virtualization: https://github.com/firecracker-microvm/firecracker/issues/1721
35. microvm.nix: https://github.com/microvm-nix/microvm.nix
36. Claude prices (Claude API skill table, cached 2026-10-06): https://docs.claude.com/en/docs/about-claude/pricing
37. DeepSeek V4.1 Flash prices (secondary): https://www.requesty.ai/models/deepseek/deepseek-v4.1-flash and https://propakistani.pk/2026/09/11/deepseek-v4-1-flash-launches-with-lower-prices-and-native-vision/

Not verified: the time to create a Hetzner server from a snapshot, the rule
that a snapshot cannot go to a smaller disk, the IPv4 price for dedicated
servers, GitHub over IPv6, the Fly rootfs limit, Sprites plan fees, Fargate
in `us-west-2`, omp RAM use, and all LLM token counts per hour.
