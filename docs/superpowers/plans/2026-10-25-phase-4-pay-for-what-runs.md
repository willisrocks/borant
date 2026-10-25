# Phase 4 pay for what runs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The hub creates pool hosts when work waits, parks idle agents, and deletes idle pool hosts, proven by a Docker e2e scenario with a fake plugin.

**Architecture:** A `Provisioner` port with a memory fake and an exec adapter, pure pool rules, fill-first placement, parking through a parked release and a heartbeat idle signal, and a hub executor that runs the rules each `elastic.tick`. Borant workers build the tasks in parallel in channel `roadmap-p4`.

**Tech Stack:** Rust (tokio, serde, async-trait), the phase 1 `borant-exec` runner, Docker for the e2e scenario.

**Spec:** `docs/superpowers/specs/2026-10-25-phase-4-pay-for-what-runs-design.md`, built on `docs/superpowers/specs/2026-10-24-elastic-compute-design.md`. Each task names its spec section, which holds the exact types, acceptance criteria, and proof. Workers read both.

## Global Constraints

- Strict ports and adapters. No core type, rule, config key, test name, or example names Hetzner, a Hetzner size, or a Hetzner region.
- The hub never holds its `mem` mutex across a provisioner call.
- New fields `#[serde(default)]`. New `hub.toml` sections `deny_unknown_fields`.
- Outside CI, run focused tests only: `just test-crate <crate> <filter>`. Run `just fmt` and `just lint` before each push.

## Review Focus

1. A delete of a host that holds a lease. The executor test and the pool rule tests assert that a drain followed by a lease gives `Undrain`, never `Delete`.
2. A create loop: a failed create must start `create_backoff`, and the count must never pass `max_hosts`. Pool rule tests.
3. A hub restart during a create leaves a provider host with no `elastic.*` key. The executor test asserts `Adopt`.
4. Parking a busy agent, or an agent whose checkpoint failed. Parking tests on the hub and the host.
5. A hub with no `[[pools]]` must behave as today. Executor test.

---

### Task 1: Provisioner port and memory fake

Spec: "Interfaces" (Port), task of the same name. No blocker.

- [ ] Write `provisioner_contract` in `borant-core::testing` and run it from `crates/borant-memory/tests/contract.rs`. Expected: FAIL.
- [ ] Add `ports/provision.rs`, `PoolName` and `Tag`, and `MemoryProvisioner`.
- [ ] Run `just test-crate borant-memory provision`. Expected: PASS. Commit.

### Task 2: Fill-first placement and host tags

Spec: "Placement", "Records" (`HostInfo.pool`, `HostInfo.tags`), task of the same name. No blocker.

- [ ] Write the three schedule tests and the config tests. Expected: FAIL.
- [ ] Change the sort key, add the two fields and their sources.
- [ ] Run `just test-crate borant-core schedule` and `just test-crate borant config`. Expected: PASS. Commit.

### Task 3: Parking idle agents

Spec: "Parking", task "Parking idle agents". No blocker.

- [ ] Write the hub and host parking tests. Expected: FAIL.
- [ ] Add `idle_since`, `HostCommand::Park`, `Released.parked`, the host park path, the hub park pick in `park.rs`, and the wake gate change.
- [ ] Run `just test-crate borant-hub park` and `just test-crate borant-host park`. Expected: PASS. Commit.

### Task 4: Pool rules

Spec: "Pools" under "Rules", task "Pool rules". Blocked by Task 1.

- [ ] Write one test for each rule listed in the task. Expected: FAIL.
- [ ] Add `rules/pool.rs` with `PoolConfig`, `PlacementNeed`, `ElasticHost`, `Spend`, `Action`, `choose`, and `plan`.
- [ ] Run `just test-crate borant-core pool`. Expected: PASS. Commit.

### Task 5: Exec provisioner

Spec: "Constraints" (the exec protocol bullets), task "Exec provisioner". Blocked by Task 1 and the phase 1 exec runner.

- [ ] Write `tests/plugins/provision-file.sh` and a test that runs `provisioner_contract` against it. Expected: FAIL.
- [ ] Add `ExecProvisioner` in `crates/borant-exec/src/provision.rs`.
- [ ] Run `just test-crate borant-exec provision`. Expected: PASS. Commit.

### Task 6: Hub elastic executor

Spec: "`hub.toml`", task "Hub elastic executor". Blocked by Tasks 1 and 4.

- [ ] Write the executor tests on `MemoryProvisioner`. Expected: FAIL.
- [ ] Add the config, `elastic.rs`, the tick call, the keys, and the wiring in `roles.rs`.
- [ ] Run `just test-crate borant-hub elastic`. Expected: PASS. Commit.

### Task 7: Docker e2e scenario

Spec: task "Docker e2e scenario". Blocked by Tasks 3, 5, and 6.

- [ ] Add the Docker CLI to the e2e image, the socket mount, `provision-docker.sh`, the fake harness `SPAWN` command, and `scenario_elastic` with its cleanup.
- [ ] Run `just e2e elastic` locally once if Docker is available, else let CI run it. Commit.

### Task 8: Orchestrator, helper, and Chris tasks

The Hetzner plugin in `borant-infra` (with the ownership label checks of the decision record), the release, the deploy, and the "Questions for Chris".
