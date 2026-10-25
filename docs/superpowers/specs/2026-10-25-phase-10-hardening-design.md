# Phase 10: hardening

Tonight's worst bugs showed up only in real use: alert keys with a colon that
NATS refuses, idle sessions that did not wake after a NATS outage, retired
workers that did not stop, and a host restore that took 85 to 103 s. Unit
tests on the memory adapters passed for all of them. This phase adds focused
Docker e2e regressions through real NATS and real host processes, one
contract test that keeps the memory store honest, and a small read-only
`borant doctor`. The supervisor's brief is `.orchestrator/phase-10-hardening.md`;
Codex (gpt-6.1-sol, high) agreed with its order.

"Run until done" stays later: it needs these proofs first.

## Goals

1. Each of these has its own small e2e scenario, runnable alone with `just e2e
   <name>`:
   - `pause`: a role cap stops an agent, the stopped agent is not placed again,
     and `borant resume` brings it back.
   - `nats_restart`: after a restart of the NATS container, an idle session
     wakes on a direct message within 30 s, and every message sent during the
     outage arrives once.
   - `cancel`: `borant cancel` of an idle worker stops its session within one
     heartbeat, and the host deletes its clone.
   - `host_restart`: a restart of a host daemon with three live sessions
     restores all three within 10 s, and their leases stay held.
2. A contract test in `borant-core::testing` checks that a state store refuses
   every key that NATS refuses and accepts the rest. The memory store and the
   NATS store both pass it.
3. `borant doctor` prints the readiness of the local host and exits non-zero
   when a check fails.

## Cuts

- Real auth checks in `doctor` (a real Claude or LunaRoute login). The checks
  stay local and read-only.
- A fix inside a scenario task. A scenario that finds a bug reports it; the
  owner files the fix as its own task.
- One large end-to-end flow. Each scenario is small and stands alone.

## Interfaces

- `tests/e2e/run.sh`: four new `scenario_*` functions and their entries in
  the list and in the affected-paths map. The NATS restart uses
  `docker compose restart nats`; nothing else in the swarm restarts.
- `borant-core::testing::state_store_key_rules(store)`: a list of keys that
  NATS refuses (`a:b`, `.a`, `a.`, `a..b`, `a b`, an empty key, `a*`, `a>`)
  and keys it accepts (`agent.a1`, `alert.members_limited.fake-auth`,
  `x/y=z_-`); `put` of a refused key fails, `get` of it is absent. The
  `borant-memory` contract test and the NATS adapter test (with
  `BORANT_NATS_URL`) both call it.
- `borant doctor [--json]` on a host, read-only. Checks, each with ok, warn,
  or fail and one line of detail:
  - the local daemon answers on its socket;
  - NATS is reachable with the host's credentials (a `ListHosts` call);
  - each harness command in `host.toml` resolves on PATH;
  - the launcher program exists (`tmux`, `herdr`, or the process launcher);
  - disk use of `BORANT_HOME` (warn at 85%, fail at 95%, the disk guard's
    lines);
  - the sandbox program, when `[sandbox] enabled`;
  - the `describe` handshake of each exec plugin in `host.toml` (secret
    stores, notifiers), with the expected kind.
  The exit code is 1 when any check fails, else 0.

## Tasks

| Task | Files | Proof |
|---|---|---|
| e2e `pause` | `tests/e2e/run.sh`, docker config | `just e2e pause` (the owner runs it on the laptop if the worker's box has no Docker) |
| e2e `nats_restart` | the same | `just e2e nats_restart` |
| e2e `cancel` | the same | `just e2e cancel` |
| e2e `host_restart` | the same | `just e2e host_restart` |
| Key rules contract | `borant-core::testing`, `borant-memory` tests, `borant-nats` tests | `just test-crate borant-memory contract`; with a NATS server, `just test-crate borant-nats key_rules` |
| `borant doctor` | new `crates/borant/src/doctor.rs`, CLI | `just test-crate borant doctor` (each check on a temp `BORANT_HOME` with a fake plugin and a missing command; the exit code) |

The four scenario tasks touch the same `run.sh`, so each adds its function in
its own block and rebases before its PR. Each code task also runs
`cargo check --workspace --all-targets`, `just fmt`, and `just lint`.
