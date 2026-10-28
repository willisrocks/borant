# Borant: agent instructions

Borant is a messaging, task, and mobility system for coding agents in
different harnesses (Claude Code, Pi, omp). Read
`docs/superpowers/specs/2026-10-24-borant-design.md` before you change
behavior. The plan is in `docs/superpowers/plans/`.

## Crates

| Crate | Role |
|---|---|
| `borant-core` | Domain types, rules, port traits, protocol. No I/O. |
| `borant-memory` | In-memory adapters for tests |
| `borant-nats` | NATS JetStream adapters |
| `borant-hub` | Hub daemon: the only writer of control-plane state |
| `borant-host` | Host daemon: local API, delivery, checkpoints, launches |
| `borant-launch` | Launcher adapters: process, tmux, zellij, orca |
| `borant` | The binary: CLI, daemon roles, MCP server, Claude Code hooks |
| `borant-testkit` | `borant-fake-harness` for Docker end-to-end tests |

Integrations live in `integrations/` (Claude Code plugin, Pi and omp
extension). Docker multi-host tests live in `docker/` and `tests/e2e/`.

## Rules

- Ports and adapters, strictly. Only Borant's domain logic (channels, tasks,
  leases, delivery, moves, placement rules) lives in the core. Every outside
  system is a port in `borant-core::ports` with neutral types and one or more
  adapters: the bus and state store (NATS), compute and hosts (Hetzner, Fly.io,
  Modal), secrets (1Password, Doppler, secretspec), launchers (tmux, herdr,
  Orca), notifiers, and account usage. No core type, rule, or config key may
  name a vendor or assume one, such as a VPS or a cloud.
- An adapter is either a crate that passes the contract suite of its port in
  `borant-core::testing`, or a program behind the built-in `exec` adapter (the
  exec plugin protocol in
  `docs/superpowers/specs/2026-10-24-elastic-compute-design.md`). Private
  backends, such as the Hetzner provisioner and the 1Password plugin, live as
  exec plugins in a private repo, not in this one.
- Only the hub writes control-plane keys. Host daemons send requests.
- Hooks and the Pi extension must never break the harness. If the daemon is
  down, they exit 0 with no output.
- Outside CI, run focused tests only: `just test-crate <crate> <filter>`.
  CI runs the full suite and the Docker scenarios.
- NATS adapter tests need `just nats-up` and
  `BORANT_NATS_URL=nats://127.0.0.1:4222`. Without the variable they skip.

## Commands

- `just fmt`, `just lint`
- `just test-crate borant-core rules`
- `just nats-up`, `just nats-down`
- `just e2e [scenario...]`

## Mod and TUI changes need a real render

Unit tests say what a tree contains; they do not paint it. A mod or TUI change
needs a real Claude Code session in tmux at fixed sizes, driven with keys, with
a color capture rendered to PNG and read back. Text captures without color hide
highlights and focus rings.

- `just cos-visual [plugin-dir] [out] [prefix]` runs the loop at 200x50 and
  120x40: it opens the band with `/cos`, focuses it with `ctrl+x tab`, `tab`,
  and `down`, opens the pane with `Enter`, moves with `j`, and writes
  `<prefix>-<width>-<state>.txt` and `.png` for each step.
- The board defaults to `BORANT_COS_BOARD=fixture`; for a live board use
  `BORANT_COS_BOARD=cli` with `borant` on PATH.
- Compare before and after: `git worktree add /tmp/before main`, run the same
  command with `--plugin-dir /tmp/before/integrations/claude-code`, and attach
  both sets to the PR.
- `freeze` comes from `nix shell nixpkgs#charm-freeze` (the nixpkgs package is
  `charm-freeze`, not `freeze`).
