# Phase 1 zero-touch hosts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new box launches Claude Code and omp workers with no login, no secret value leaks, and Claude Code accounts rotate at a usage limit.

**Architecture:** A `SecretStore` port in `borant-core` with `env`, `file`, and `exec` adapters in `borant-secrets`, an exec runner in `borant-exec`, resolver mode in the host launch with the `borant secret-exec` shim, and bundle members picked by a pure rule in the hub. Borant workers build the code tasks in parallel in channel `roadmap-p1`.

**Tech Stack:** Rust (tokio, serde, zeroize, clap), Claude Code hooks, NixOS and 1Password in `borant-infra`.

**Spec:** `docs/superpowers/specs/2026-10-25-phase-1-zero-touch-hosts-design.md`. Each task below names its spec section. The spec section holds the exact types, file formats, acceptance criteria, and proof. Workers read both.

## Global Constraints

- Strict ports and adapters (AGENTS.md). No core type, rule, or config key names a vendor.
- `Secret` has no `Serialize` and no `Clone`. Its Debug prints `Secret(..)`. `expose()` is its only reader.
- Only the host daemon calls `SecretStore`. The hub stores bundle and member names only.
- All new fields on `Agent`, `HostInfo`, `LaunchOrder`: `#[serde(default)]`.
- Hooks exit 0 with no output when the daemon is down.
- Outside CI, run focused tests only: `just test-crate <crate> <filter>`. Run `just fmt` and `just lint` before each push.

## Review Focus

1. A secret value in a Debug string, an error message, a log line, a session-log event, a tmux argument, or `LaunchSpec.env`. Resolver task test asserts all five.
2. A host with no `[secrets]` section must work as today, because the laptop host has none. Host config task test.
3. A member bundle with no entry in `LaunchOrder.members` fails the launch with `invalid`, not a launch with no token. Resolver task test.
4. A hook that fails when the daemon is down, or prints output. Limit restart task test.
5. An omp agent that waits because every Claude Code member is limited. Member rules task test: an omp launch gets no `claude-auth` member.

---

### Task 1: Core interfaces for secrets and members

Spec: "Interfaces" (Port, Records and protocol), "Member rules" (`MemberRules` only). No blocker.

- [ ] Write the tests: Debug of `Secret` prints no value; old JSON of `Agent`, `HostInfo`, `LaunchOrder` parses; `secret_store_contract` passes against an in-crate test store.
- [ ] Run `just test-crate borant-core secrets`. Expected: FAIL.
- [ ] Add `ports/secrets.rs`, the `name_type!` names, the records, `MemberBundle`, `MemberState`, `UsageWindow`, `MemberRules`, the `ToolCall` and `CtlRequest` variants, `MoveReason::Limit`, `SessionEventBody::SecretsIssued`, and the contract suite.
- [ ] Run the tests. Expected: PASS. Commit.

### Task 2: Exec runner crate

Spec: "Exec protocol", task "Exec runner crate". No blocker.

- [ ] Write shell fixture plugins and tests for: a good reply, an `error` reply for each code, bad JSON, a non-zero exit, a timeout, the empty environment plus `env`, and `BORANT_CREDENTIALS_DIRECTORY` with `BORANT_CREDENTIAL`.
- [ ] Add `PortErrorKind::Invalid` and `Denied` with their mapping in `crates/borant-core/src/proto.rs`.
- [ ] Add `borant-exec` with `pub async fn call(cfg: &ExecConfig, op: &str, args: serde_json::Value) -> PortResult<serde_json::Value>` and `pub async fn describe(cfg: &ExecConfig, kind: &str) -> PortResult<serde_json::Value>`, where `ExecConfig { command: Vec<String>, env: BTreeMap<String, String>, credential: Option<(PathBuf, String)>, timeout: Duration }`.
- [ ] Run `just test-crate borant-exec`. Expected: PASS. Commit.

### Task 3: The `borant secret-exec` shim

Spec: "The secret directory and the shim", task of the same name. No blocker.

- [ ] Write the test: a fixture dir with `env` and `pre.json`, and a program that prints its env and a marker file that `pre` made. Assert the env, the marker, and that `env` is deleted.
- [ ] Add the subcommand in a new module `crates/borant/src/secret_exec.rs`, wired in `cli.rs` and `main.rs`.
- [ ] Run `just test-crate borant secret_exec`. Expected: PASS. Commit.

### Task 4: Env and file secret stores

Spec: store kinds paragraph, task "Env and file secret stores". Blocked by Task 1.

- [ ] Write `tests/contract.rs` that runs `secret_store_contract` on `EnvStore` and `FileStore`, and a test that `FileStore` refuses `..`.
- [ ] Add `borant-secrets` with `EnvStore`, `FileStore`, `StoreConfig`, and `open_store` (exec returns `Invalid` for now).
- [ ] Run `just test-crate borant-secrets contract`. Expected: PASS. Commit.

### Task 5: Exec secret store

Spec: "Exec protocol" (secrets ops), task "Exec secret store". Blocked by Tasks 1, 2, and 4.

- [ ] Write the contract test against a shell fixture plugin, and a test that an error and the Debug output hold no value.
- [ ] Add `ExecStore` in `crates/borant-secrets/src/exec.rs` and the `exec` arm of `open_store`, which runs `describe`.
- [ ] Run `just test-crate borant-secrets exec`. Expected: PASS. Commit.

### Task 6: Secrets in host.toml

Spec: "`host.toml` and `hub.toml`", task "Secrets in host.toml". Blocked by Tasks 1 and 4.

- [ ] Write the tests: the example `host.toml` of the spec parses; `HostInfo.members` holds `claude-auth` with three members and `harnesses = [claude-code]`; a host with no `[secrets]` parses as today; a failing store drops its harnesses.
- [ ] Add the config types and the start-up `open_store` calls.
- [ ] Run `just test-crate borant config` and `just test-crate borant-host secrets_config`. Expected: PASS. Commit.

### Task 7: Resolver mode in the host launch

Spec: "The secret directory and the shim", task "Resolver mode in the host launch". Blocked by Tasks 1 and 6.

- [ ] Write the test with an `EnvStore`: no value in the tmux arguments, `LaunchSpec.env`, or the session log; `SecretsIssued` has names only; the directory is gone after the session ends; a member bundle without a member fails with `invalid`.
- [ ] Add the resolve, the directory, the shim wrap, the cleanup, the redactor, and the persistence of bundle and member.
- [ ] Run `just test-crate borant-host resolver`. Expected: PASS. Commit.

### Task 8: Member rules and hub assignment

Spec: "Member rules", task "Member rules and hub assignment". Blocked by Task 1.

- [ ] Write one `pick` test for each of rules 2 to 6, and a hub test that an omp launch gets no `claude-auth` member.
- [ ] Add `rules::members::pick`, the hub assignment in the launch path, `MemberState` in KV, reports in memory, `UsageReport` and `ListMembers`, and the CLI commands.
- [ ] Run `just test-crate borant-core members` and `just test-crate borant-hub members`. Expected: PASS. Commit.

### Task 9: Limit restart

Spec: "Limit restart", task of the same name. Blocked by Tasks 7 and 8, and by the token facts (the `StopFailure` input format).

- [ ] Write the hub test (restart on the next member on the same host, wait and wake with every member limited, the loop guard) and the hook tests.
- [ ] Add the `--settings` flag in the Claude Code adapter, the hook, the host relay, the hub entry point, and `[secrets.members]` in `hub.toml`.
- [ ] Run `just test-crate borant-hub limit` and `just test-crate borant hook`. Expected: PASS. Commit.

### Task 10: Orchestrator, helper, and Chris tasks

Spec: "Orchestrator and Chris tasks" and "Questions for Chris". The orchestrator runs the secrets facts on borant-2, writes the 1Password plugin and the `borant-infra` module with the helper, cuts the release with the helper, and runs the new-box test after Chris answers.
