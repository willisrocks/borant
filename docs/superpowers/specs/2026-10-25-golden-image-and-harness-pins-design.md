# Golden image and Nix-pinned harnesses

Date: 2026-10-25. Goal from Chris (through chief-personal), after `borant
host remove`. Builds on section 11 of
`2026-10-24-secrets-and-host-bootstrap-design.md`. All code is in
`borant-infra`. Borant itself does not change.

## Why

The borant-3 new-box test (2026-10-25) passed the "no login" check for Claude
Code, but two steps were still by hand: the Claude Code and omp installers
(`curl | sh` into `~/.local/bin`), and the omp setup and login. A new box also
takes about ten minutes (`just create`, `just install`, `just lock`).

Done when `just create <box>` from the image gives a working host with no
login and no hand steps, and the harness versions on every box come from one
pinned file that a bump PR updates.

## Facts that drive the design

- Both installers fetch a prebuilt binary and check it against a SHA-256 in a
  release manifest: Claude Code from
  `https://downloads.claude.ai/claude-code-releases/<version>/manifest.json`,
  omp from `https://build.stencil.so` (repo `can1357/oh-my-pi`). So Nix can
  pin both with `fetchurl` and the manifest SHA-256.
- Both are Bun single-file binaries. `patchelf` and `strip` break them, and
  `programs.nix-ld` already runs them on the boxes.
- The hub already sends boot values to a new elastic host as Hetzner user data
  (one JSON object): `BORANT_HOST_NAME`, `BORANT_POOL`, `BORANT_TAGS`. The
  host daemon already takes its name from `BORANT_HOST_NAME`.
- NATS users have no per-user permissions. A per-host user only gives a
  per-host password.
- omp needs four things besides its binary: the LunaRoute extension
  `@lunaroute/pi-extension@0.15.0` (0.16.0 and later do not load in omp
  18.12.6), `~/.omp/agent/config.yml` (`setupVersion: 2`, the default model),
  the `lunaroute` entry of `~/.pi/agent/models-store.json`, and the login in
  `~/.omp/agent/agent.db`.
- cpx31 in `hil` costs $0.118 per hour. Hetzner bills started hours.

## Part 1: harness pins

- `pkgs/harnesses.json` holds the version and SHA-256 of Claude Code, omp, and
  the LunaRoute extension, for `x86_64-linux` (and `aarch64-darwin`, so the
  laptop can use the same pins later).
- `pkgs/claude-code.nix` and `pkgs/omp.nix` fetch the binaries with
  `fetchurl`, install them without patchelf or strip, and wrap them:
  `DISABLE_AUTOUPDATER=1` for Claude Code, and the matching setting for omp,
  so that no box updates itself outside the pin.
- `modules/borant.nix` puts both in `environment.systemPackages` and in the
  `PATH` of `borant-host`. The deploy removes the hand-installed copies in
  `~/.local/bin`, because they come first in `PATH`.
- The omp setup becomes Nix: the extension tarball (pinned in
  `harnesses.json`) is linked into `~/.omp/agent`, and a seed unit merges
  `config.yml` and the `lunaroute` catalog entry, like the existing
  `borant-claude-seed` merges `~/.claude.json`. It writes only missing keys.

### Bumps

- `pkgs/bump-harnesses.sh` reads the newest version and SHA-256 of Claude
  Code and omp from their manifests and rewrites `harnesses.json`. It does not
  bump the LunaRoute extension, because that pin follows omp compatibility.
- A daily systemd timer on borant-1 (06:00 UTC) runs the script in a clone of
  `borant-infra`. If the file changed, it pushes a branch and opens a PR. The
  PR comes from `borant-swarm[bot]`, through the gh login that the App timer
  already keeps fresh.
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

## Part 2: golden image

- A generic NixOS config `host-image` in `flake.nix`: a host with no host
  name, no NATS credential, and no secrets in the store.
- `just image`: create a throwaway cpx31 from Ubuntu, run `nixos-anywhere`
  with `host-image`, delete the SSH host keys and `/etc/machine-id`, power
  off, snapshot with the labels `borant.image=host` and the harness versions,
  delete the box. Keep the two newest snapshots.
- `borant-firstboot` (root, once, before `tailscaled` and `borant-host`): read
  the user data from `http://169.254.169.254/hetzner/v1/userdata`, set the
  host name, and write `BORANT_HOST_NAME`, `BORANT_POOL`, and `BORANT_TAGS` to
  `/var/lib/borant/boot.env` for `borant-host`.
- An nftables rule drops traffic to `169.254.169.254` from every UID except
  root, so an agent cannot read the user data.
- User data carries no secret. `just create <box>` creates the server from
  the newest snapshot behind the install firewall, waits for SSH, then sends
  each secret over SSH stdin, as `push-store-token` does today: the Tailscale
  auth key, the store token, and the NATS credential. Then it locks the
  firewall and waits for the host heartbeat in `borant hosts`.
- NATS for `just create`: keep one user per host. `just create` runs `just
  nats-credential`, deploys borant-1, and pushes `nats-users.json`. This needs
  no new 1Password item.

## Part 3: the omp token

First a probe on borant-2: can omp take its LunaRoute login from a file or an
environment variable (an exported `agent.db` record, or `omp auth import`)?

- If it can: a `lunaroute` item in the Borant vault, and a `lunaroute-auth`
  bundle for omp, like `claude-auth` for Claude Code. Chris makes the item.
- If it cannot: hosts from the image offer Claude Code only, and the omp token
  stays on the open list.

## Tasks

| Task | Proof |
|---|---|
| Harness pins, the omp seed, and the deploy that removes `~/.local/bin` copies | borant-2: `claude --version` and `omp --version` from `/run/current-system`, one Borant session of each |
| The bump script and timer | A forced run opens a PR from `borant-swarm[bot]` |
| `host-image`, `borant-firstboot`, the metadata rule, `just image`, and `just create` from the image | The test below |
| omp token probe | A yes or no, with the command that proves it |

Test: `just image`, then `just create borant-3` from the image. Pass when the
host registers with no login and no hand step, a Claude Code agent runs a
command from the pool, an agent cannot reach `169.254.169.254`, and the leak
checks of the borant-3 test find no token. Then delete borant-3 and run
`borant host remove borant-3`.

## Spend

| Item | Estimate |
|---|---|
| Image build box, cpx31, about 1 h | $0.12 |
| Test box, cpx31, 1 to 2 h | $0.12 to $0.24 |
| One retry of each | up to $0.36 |
| Total for the build and the test | under $0.75 |
| Snapshots, 2 kept at about 1.3 GB each | about $0.04 a month, recurring |

## Open questions for Chris

1. Elastic hosts that the hub creates need the store token and the Tailscale
   key without the laptop. Default: out of this goal. Later choices: the user
   data carries them behind the root-only metadata rule, or the hub sends them
   over Tailscale SSH.
2. The laptop keeps its own path: Claude Code from the Homebrew cask
   `claude-code@latest`, and omp in `~/.local/bin` with its update step. Its
   sessions run on Chris's own Claude profile, and a version skew with the
   boxes does no harm. The `aarch64-darwin` pins make a later switch cheap.
3. The recurring snapshot cost (about $0.04 a month) needs Chris's approval.
4. The Tailscale key stays the reusable, tagged key that `just install` uses
   today. A single-use key per box needs a Tailscale OAuth client, which is a
   change to the Tailscale admin, so it is not in this goal.

## Cut

Auto-deploy of bumps, a single-use Tailscale key, a shared NATS user per pool,
elastic hosts from the image, and the laptop switch.
