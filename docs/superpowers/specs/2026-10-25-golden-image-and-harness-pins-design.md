# Golden image and Nix-pinned harnesses

Date: 2026-10-25. Goal from Chris (through chief-personal), after `borant
host remove`. Builds on section 11 of
`2026-10-24-secrets-and-host-bootstrap-design.md`. Chris answered the first
draft the same day. His answers are in this version.

Two phases:

- Phase A (`borant-infra` only): Nix-pinned harnesses, the omp key from the
  store, the image, and `just create` from the image. Done when `just create
  <box>` gives a working host with no login and no hand step.
- Phase B (`borant` and `borant-infra`): the hub creates a box from the image
  and the box gets its first secrets from the secrets layer, with no laptop
  step. Done when an elastic pool host boots, registers, and runs an agent.

## Why

The borant-3 new-box test (2026-10-25) passed the "no login" check for Claude
Code, but two steps were still by hand: the Claude Code and omp installers
(`curl | sh` into `~/.local/bin`), and the omp setup and login. A new box also
takes about ten minutes (`just create`, `just install`, `just lock`).

## Facts that drive the design

- Both installers fetch a prebuilt binary and check it against a SHA-256 in a
  release manifest: Claude Code from
  `https://downloads.claude.ai/claude-code-releases/<version>/manifest.json`,
  omp from `https://build.stencil.so` (repo `can1357/oh-my-pi`). So Nix can
  pin both with `fetchurl` and the manifest SHA-256.
- Both are Bun single-file binaries. `patchelf` and `strip` break them, and
  `programs.nix-ld` already runs them on the boxes.
- omp takes its LunaRoute key from `LUNAROUTE_API_KEY` (Chris). The key is
  already an item in the Borant vault.
- omp also needs the LunaRoute extension `@lunaroute/pi-extension@0.15.0`
  (0.16.0 and later do not load in omp 18.12.6), `~/.omp/agent/config.yml`
  (`setupVersion: 2`, the default model), and the `lunaroute` entry of
  `~/.pi/agent/models-store.json`.
- The hub already sends boot values to a new elastic host as Hetzner user data
  (one JSON object): `BORANT_HOST_NAME`, `BORANT_POOL`, `BORANT_TAGS`. The
  host daemon already takes its name from `BORANT_HOST_NAME`.
- Agents run as `chris`, and `chris` has `sudo` with no password. A rule that
  lets only root read the metadata service does not stop an agent. So user
  data must carry no secret that is still useful after the first boot.
- The hub has no secret store today. Only `borant-host` opens stores.
- NATS users have no per-user permissions. The NATS user list is Nix config
  on borant-1, so a new NATS user needs a deploy of borant-1.
- `borant doctor` already reports a harness program that is not on `PATH`.
- cpx31 in `hil` costs $0.118 per hour. Hetzner bills started hours.

## Phase A

### Harness pins

- `pkgs/harnesses.json` holds the version and SHA-256 of Claude Code, omp, and
  the LunaRoute extension, for `x86_64-linux` only.
- `pkgs/claude-code.nix` and `pkgs/omp.nix` fetch the binaries with
  `fetchurl`, install them without patchelf or strip, and wrap them with
  self-update off (`DISABLE_AUTOUPDATER=1` for Claude Code, the matching
  setting for omp).
- `modules/borant.nix` puts both in `environment.systemPackages` and in the
  `PATH` of `borant-host`. The deploy removes the hand-installed copies in
  `~/.local/bin` on the boxes, because they come first in `PATH`.
- The omp setup becomes Nix: the extension tarball is linked into
  `~/.omp/agent`, and a seed unit merges `config.yml` and the `lunaroute`
  catalog entry, like `borant-claude-seed` merges `~/.claude.json`. It writes
  only missing keys.
- A `lunaroute-auth` bundle in the `[secrets]` of `host.toml` reads the
  existing Borant vault item and sets `LUNAROUTE_API_KEY`. It is the default
  bundle of the omp harness, as `claude-auth` is for Claude Code. A probe on
  borant-2 first makes sure that omp runs one turn with only that variable.
- The laptop gets nothing. Its `host.toml` has no `[secrets]`, and this goal
  installs and manages no harness there. If a harness is missing on the
  laptop, `borant doctor` reports it.

### Bumps

- `pkgs/bump-harnesses.sh` reads the newest version and SHA-256 of Claude
  Code and omp from their manifests and rewrites `harnesses.json`. It does not
  bump the LunaRoute extension, because that pin follows omp compatibility.
- A daily systemd timer on borant-1 (06:00 UTC) runs the script in a clone of
  `borant-infra`. If the file changed, it pushes a branch and opens a PR as
  `borant-swarm[bot]`, through the gh login that the App timer keeps fresh.
- I review and merge the PR, deploy borant-2 first, run one short session of
  each harness there, then deploy borant-1, then rebuild the image.

Rejected: nixpkgs `claude-code` (built from npm, it lags the native releases,
and nixpkgs has no omp). A GitHub Actions schedule (borant-infra has no CI,
and the box already has a token). A Borant schedule that spawns an agent (a
script needs no model). An auto-deploy after the merge (the rule is one host
at a time, with a lease check).

### Rollback

- One box: `sudo nixos-rebuild switch --rollback` returns to the last
  generation in seconds.
- All boxes: revert the bump commit and deploy.
- The image: delete the bad snapshot. Creates take the newest snapshot with
  the label `borant.image=host`, so they fall back to the one before.

### Image and `just create`

- A generic NixOS config `host-image` in `flake.nix`: a host with no host
  name, no NATS credential, and no store token.
- `just image`: create a throwaway cpx31 from Ubuntu, run `nixos-anywhere`
  with `host-image`, delete the SSH host keys and `/etc/machine-id`, power
  off, snapshot with the labels `borant.image=host` and the harness versions,
  delete the box. Keep the two newest snapshots (Chris approved the storage,
  about $0.04 a month).
- `borant-firstboot` (root, once, before `tailscaled` and `borant-host`): read
  the user data from `http://169.254.169.254/hetzner/v1/userdata`, set the
  host name, and write `BORANT_HOST_NAME`, `BORANT_POOL`, and `BORANT_TAGS` to
  `/var/lib/borant/boot.env` for `borant-host`.
- In phase A, user data carries no secret. `just create <box>` creates the
  server from the newest snapshot behind the install firewall, waits for SSH,
  and sends each secret over SSH stdin, as `push-store-token` does today: the
  reusable tagged Tailscale key, the store token, and a NATS credential from
  `just nats-credential` (with a deploy of borant-1). Then it locks the
  firewall and waits for the host heartbeat in `borant hosts`.

## Phase B: first secrets for a box that the hub creates

The hub creates the box, so no laptop step can send secrets. The design: the
user data carries only the Tailscale key and a single-use join token. The box
joins the tailnet, then trades the join token at the hub for the rest of its
secrets, once. The hub reads those secrets through its own `SecretStore`.

### Steps

1. The hub decides to create a pool host. It makes a random join token,
   stores only its SHA-256 with the host name and an expiry of
   `boot_timeout`, and adds `BORANT_JOIN` to the boot values. It reads the
   Tailscale key from its store (bundle `host-boot`) and adds it as
   `TAILSCALE_AUTHKEY`. The provisioner sends the boot values as user data on
   stdin, as today.
2. `borant-firstboot` reads the user data, writes the Tailscale key to the
   file that `tailscaled` reads, and waits for the tailnet.
3. `borant-firstboot` sends the host name and the join token to a new hub
   endpoint, `POST /v1/join`, on the hub's tailnet address
   (`100.75.2.7:7420`). The hub accepts a token once, before its expiry, for
   its own host name only. Then it deletes the token and returns the values of
   bundle `host-join` once: the store token and the NATS credential.
4. `borant-firstboot` writes the store token to
   `/var/lib/borant-secrets/op-store-token` and the NATS credential to
   `nats-host.env` (root, 0600), then starts `borant-host`. The host opens its
   store and reads `claude-auth` and `lunaroute-auth` as every host does.
5. After the join, the user data holds a used join token and the reusable
   Tailscale key. The Tailscale key is the one that `just install` already
   puts on every box.

### The hub's secret store

- `hub.toml` gets a `[secrets]` table with the same stores and bundles as
  `host.toml`. The hub opens its stores with the `borant-secrets` crate that
  `borant-host` uses, and resolves only `host-boot` and `host-join`.
- Store `op` (the 1Password exec plugin, read-only on the Borant vault):
  `TAILSCALE_AUTHKEY` and the NATS pool password.
- Store `file` (the existing `file` adapter on
  `/var/lib/borant/credentials`): the store token that the hub hands out.
- NATS: one user per pool, `host-pool-<pool>`, added once to
  `nats-users.json`. A per-host user needs a deploy of borant-1 for each box,
  and the hub cannot deploy. NATS users have no per-user permissions, so a
  shared user loses nothing that per-host users give today.

### Bootstrap secrets that stay on the hub

| Secret | Where | Why it cannot come from the store |
|---|---|---|
| Hetzner token | `/var/lib/borant/credentials/hcloud-token` (exists) | The provisioner needs it, and no store holds it now |
| Store token (`borant-hosts`) | `/var/lib/borant-secrets/op-store-token` (exists), also read by the hub through the `file` store | It opens the store, so it cannot come from the store |
| The hub's own NATS credential | `nats-hub.env` (exists) | The hub needs NATS to start |

### Chris steps in phase B (each with a rollback)

- Add the reusable tagged Tailscale key to the Borant vault, because the hub
  reads only that vault. Rollback: delete the item.
- Add a `nats-host-pool` item in the Borant vault with a new password.
  Rollback: delete the item and the user `host-pool-hil` from
  `nats-users.json`, then deploy borant-1.
- If the Tailscale policy blocks the tagged hosts from port 7420 on the hub,
  allow it. Rollback: remove that rule. I check the policy first and change
  nothing in it myself.

### Code in `borant`

- The hub opens `[secrets]` stores and resolves a bundle by name.
- The join token: made at create, hashed in the hub KV, deleted at use or at
  the `boot_timeout` reap.
- `POST /v1/join` on the hub HTTP listener. No other HTTP route returns a
  secret, and the reply goes only to the caller.
- Contract tests: a token works once, a token for another host name is
  refused, an expired token is refused, and the hub log never holds a value.

## Tasks

| Phase | Task | Proof |
|---|---|---|
| A | Harness pins, the omp seed, `lunaroute-auth`, and the deploy that removes the `~/.local/bin` copies | borant-2: both from `/run/current-system`, one Borant session of each with no login |
| A | Bump script and timer | A forced run opens a PR from `borant-swarm[bot]` |
| A | `host-image`, `borant-firstboot`, `just image`, `just create` from the image | Test A below |
| B | Hub `[secrets]`, join tokens, `POST /v1/join` | Hub tests in CI |
| B | Firstboot join, the NATS pool user, the hub `[secrets]` in `modules/borant.nix`, an elastic pool | Test B below |

Test A: `just image`, then `just create borant-3`. Pass when the host
registers with no login and no hand step, a Claude Code agent and an omp agent
each run a command, and the leak checks of the borant-3 test find no token.
Then delete borant-3 and run `borant host remove borant-3`.

Test B: an elastic pool with `max = 1` and the image. A spawn for that pool
makes the hub create a box. Pass when the box joins, registers, and runs the
agent, a second use of its join token fails, and the leak checks find no
token. Then the pool scales to zero and the hub deletes the box.

## Spend

| Item | Estimate |
|---|---|
| Image build box, cpx31, about 1 h | $0.12 |
| Test A box, 1 to 2 h | $0.12 to $0.24 |
| Test B box, 1 to 2 h | $0.12 to $0.24 |
| One retry of each | up to $0.60 |
| Total for the build and both tests | under $1.20 |
| Snapshots, 2 kept at about 1.3 GB each (approved) | about $0.04 a month |

The total can pass $1 only with retries. I ask before a retry would pass $1.

## Cut

Auto-deploy of bumps, a single-use Tailscale key per box (it needs a
Tailscale OAuth client, later), the laptop install, and NATS operator mode
with per-host user tokens.
