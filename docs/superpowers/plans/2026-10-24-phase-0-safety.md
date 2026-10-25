# Phase 0 safety fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restrict NATS, back up the hub every 5 minutes, log silent failures at `warn`, and list stale agents, without a long stop of the swarm.

**Architecture:** Three code tasks in this repo, built by Borant workers in channel `roadmap-p0`. The orchestrator does the `borant-infra` tasks (NATS users, passwords, the backup timer) and the deploys. Chris applies the Tailscale policy. The urgent hub restart stall fix (task 01M5RG3T9B649D17ASFP40M3ME) is already in progress and is not repeated here.

**Tech Stack:** Rust (tokio, async-nats, clap, tracing), NixOS (`services.nats`, systemd timers), 1Password `op`.

**Spec:** `docs/superpowers/specs/2026-10-24-phase-0-safety-design.md`

## Global Constraints

- Ports in `borant-core::ports` expose no NATS types. Only the hub writes control-plane keys.
- Hooks and the Pi extension exit 0 with no output when the daemon is down. No task changes them.
- Outside CI, run focused tests only: `just test-crate <crate> <filter>`. Run `just fmt` and `just lint` before each push.
- NATS adapter tests need `just nats-up` and `BORANT_NATS_URL=nats://127.0.0.1:4222`.
- No secret value in git, a log, a task, a PR, or a message.
- No phase 0 release before v0.3.0 ships.

## Review Focus

1. A snapshot or a prune on a store of 1758 keys must not hold the hub `mem` mutex for longer than 1 s. Heartbeats wait for it, and hosts wait only 5 s (`crates/borant-host/src/lib.rs:74`). Snapshot task, step 1, measures it.
2. A failed snapshot must leave no partial file at the target path. A half file on borant-2 looks like a good backup. Snapshot task, step 1, test `snapshot_fails_without_a_file`.
3. A loop that fails each second (heartbeat, inbox pull) must not log a `warn` each second. Warn task, step 3.
4. `borant agents` with no subcommand must keep its current output, because people and scripts use it. Prune task, step 4.
5. A pending message for an agent that left its channel after the snapshot must not reach other members on import. Snapshot task, test `import_sends_pending_only_to_its_agent`.

---

### Task 1: Warn on silent failures

**Files:**
- Modify: `crates/borant-host/src/delivery.rs` (lines 51, 82, 94, 97-98, 408, 444-451)
- Modify: `crates/borant-host/src/commands.rs:56`, `crates/borant-host/src/lib.rs:270`, `:304-314`, `crates/borant-host/src/sessions.rs:146`
- Modify: `crates/borant-nats/src/bus.rs:98`
- Modify: `crates/borant-hub/src/lib.rs:112`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing that other tasks use.

- [ ] **Step 1: Change each site in the spec table "Silent failures at warn" to `tracing::warn!`**, with an `agent` or `subject` field. Replace `let _ =` and `.is_ok()` drops with a `match` or `if let Err(e)` that logs.
- [ ] **Step 2: In `crates/borant-hub/src/lib.rs:112`, log at `warn` only when `e.code` is `ApiErrorCode::Internal` or `ApiErrorCode::Unavailable`.** Other codes stay at `debug`.
- [ ] **Step 3: For the heartbeat loop and the inbox pull loop, keep a `failing: bool`.** Log the first failure at `warn` and set it. Log later failures at `debug`. On the next success, log "recovered" at `info` and clear it.
- [ ] **Step 4: Run** `just test-crate borant-host delivery` and `just lint`. Expected: PASS, no new warnings.
- [ ] **Step 5: Commit** `fix: log silent ack, append, and hub request failures at warn`.

### Task 2: Hub snapshot export

Blocked by the hub restart stall fix, because both change `NatsState::list`. Start from `main` after that fix merges.

**Files:**
- Modify: `crates/borant-core/src/proto.rs` (`CtlRequest`)
- Modify: `crates/borant-hub/src/export.rs`, `crates/borant-hub/src/lib.rs` (`handle`)
- Modify: `crates/borant-nats/src/state.rs:110-130` (key-iterator errors)
- Modify: `crates/borant/src/cli.rs` (`HubCmd::Export`), `crates/borant/src/roles.rs` (`export`)
- Test: `crates/borant-hub/tests/export.rs`

**Interfaces:**
- Consumes: `Hub`, `Store::list`, `MessageBus::unacked`, `SessionStore::read`.
- Produces: `CtlRequest::Snapshot { out: PathBuf }`, reply `Reply::Ok` with `{"path": ..., "state": n, "sessions": n, "pending": n, "lock_ms": n}`. `pub async fn snapshot(hub: &Hub, out: &Path) -> anyhow::Result<SnapshotReport>` in `export.rs`, where `SnapshotReport { state, sessions, pending, lock_ms }`. CLI: `borant hub export --snapshot --out <file>`.

- [ ] **Step 1: Write the failing tests in `crates/borant-hub/tests/export.rs`:**
  - `snapshot_keeps_serving`: start `hub.run` on the memory adapters, send `Snapshot`, and assert that a `ListAgents` call during the snapshot gets a reply, and that no `Maintenance` state is set.
  - `snapshot_fails_without_a_file`: a state store wrapper whose `list` fails. Assert that `snapshot` returns `Err` and that neither `out` nor `out.tmp` exists.
  - `snapshot_keeps_a_message_acked_during_it`: a bus wrapper that, after `unacked` returns for agent `a`, appends the receipt event to the session log of `a` and acks the message. Assert that the import of the file has the message as a pending record or as a session event.
  - `import_sends_pending_only_to_its_agent`: an export with a `Pending { agent: a, envelope }` whose `envelope.to` is channel `c`, where `b` is also a member of `c`. After import, assert that `unacked(a)` has the envelope and `unacked(b)` does not.
  - `snapshot_lock_is_short`: seed 1758 keys (1307 `chan.*`, 158 `session.*`, 147 `lease.agent.*`, 146 `agent.*`), run `snapshot`, assert `lock_ms < 1000`.
- [ ] **Step 2: Run** `just test-crate borant-hub export`. Expected: FAIL, `Snapshot` and `snapshot` are not defined.
- [ ] **Step 3: Implement `snapshot`** with the read order of the spec section "Hub snapshot": `agent.*` without the mutex, inboxes, KV under `hub.mem.lock()`, session logs. Write to `<out>.tmp`, `fsync`, rename. In `Hub::handle`, route `CtlRequest::Snapshot` to `snapshot` before it takes the mutex.
- [ ] **Step 4: Fix `import`** so that a `Pending { agent, envelope }` record is published on `subjects::agent(&agent)`.
- [ ] **Step 5: Make `NatsState::list` return `Err` on a key-iterator error** instead of `.ok()` filtering.
- [ ] **Step 6: Add `--snapshot` to `HubCmd::Export`.** With it, `roles::export` sends `Snapshot { out }` to `subjects::CTL` with a 60 s timeout, skips the maintenance request, and prints the report. Without it, behavior is unchanged.
- [ ] **Step 7: Run** `just test-crate borant-hub export`, then `just nats-up` and `BORANT_NATS_URL=nats://127.0.0.1:4222 just test-crate borant-nats state`. Expected: PASS.
- [ ] **Step 8: Run the restore drill** of the spec ("Hub snapshot export", proof) and put the commands and the output in the PR.
- [ ] **Step 9: Commit** `feat(hub): snapshot export without maintenance mode`.

### Task 3: Agents prune dry run

**Files:**
- Create: `crates/borant-hub/src/prune.rs`
- Modify: `crates/borant-core/src/proto.rs` (`ToolCall`), `crates/borant-hub/src/calls.rs` (one dispatch arm), `crates/borant-hub/src/lib.rs` (`handle` routing)
- Modify: `crates/borant/src/cli.rs` (`Command::Agents`), the CLI print code for agents
- Test: `crates/borant-hub/tests/prune.rs`

**Interfaces:**
- Consumes: `Store::list`, `MessageBus::unacked`, `SessionStore::read`, `session_log::undelivered`, `Memory::pending_moves`.
- Produces: `ToolCall::PruneAgents { older_than_secs: u64 }` (allowed for humans). Reply JSON `{"candidates": [{"agent", "since", "keys": [..], "bytes"}], "kept": [{"agent", "reason"}], "totals": {"candidates", "keys", "bytes"}}`. `reason` is one of `session`, `lease`, `pending_move`, `claims`, `owner`, `unacked`, `undelivered`.

- [ ] **Step 1: Write the failing tests in `crates/borant-hub/tests/prune.rs`**, one for each kept reason, and `prune_candidate_lists_its_keys` (an expired agent of 2 days with one membership and two session records lists 5 keys), and `prune_writes_nothing` (the key set and revisions are equal before and after the call), with `older_than_secs = 86400`.
- [ ] **Step 2: Run** `just test-crate borant-hub prune`. Expected: FAIL.
- [ ] **Step 3: Implement `pub(crate) async fn prune_agents(hub: &Hub, older_than: Duration) -> Result<Value, ApiError>` in `prune.rs`.** Read the KV state and copy `pending_moves` under the `mem` mutex, release it, then read inboxes and session logs. In `Hub::handle`, route `Call { call: ToolCall::PruneAgents { .. }, .. }` there before it takes the mutex.
- [ ] **Step 4: Change `Command::Agents` to `Agents { #[command(subcommand)] cmd: Option<AgentsCmd> }`** with `AgentsCmd::Prune { #[arg(long)] dry_run: bool, #[arg(long, default_value_t = 24)] older_than_hours: u64 }`. Without a subcommand, print as today. Without `--dry-run`, exit 2 with "only --dry-run is available". The CLI has no duration parser, and this task adds no new crate for one.
- [ ] **Step 5: Run** `just test-crate borant-hub prune` and `just test-crate borant cli`. Expected: PASS.
- [ ] **Step 6: Commit** `feat: borant agents prune --dry-run`.

### Task 4: Orchestrator and Chris tasks

These are in the spec sections "Rollout of NATS authentication", "Tailscale policy", and "Backup timer", with their done conditions in "Orchestrator and Chris tasks". The orchestrator does them in `~/Code/borant-infra` and logs each deploy in `.orchestrator/notes.md`.

- [ ] Release with the stall fix (v0.3.0, else v0.3.1), and deploy it to borant-1.
- [ ] NATS users with `no_auth_user`, the monitor on `127.0.0.1:8222`, and `just nats-credential <box> <role>`.
- [ ] Passwords on borant-2, then borant-1 (host, then hub), then the laptop.
- [ ] Remove `no_auth_user` and `legacy`. Test anonymous and wrong-password clients.
- [ ] Tailscale policy: save the old policy, then a `needs_human` task for Chris with the diff.
- [ ] Phase 0 release (Tasks 1 to 3) with the release helper, and deploy one box at a time.
- [ ] Backup timer on borant-1 to borant-2, the restore runbook in `borant-infra/README.md`, and the restore drill.
