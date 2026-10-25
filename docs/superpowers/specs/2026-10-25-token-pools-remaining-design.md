# Token pools: what remains

The [token pools design](2026-10-25-token-pools-design.md) has three build
phases after its facts phase. Roadmap phase 1 built the first two as "bundle
members" ([phase 1 spec](2026-10-25-phase-1-zero-touch-hosts-design.md),
sections "Member rules" and "Limit restart"):

| Token pools item | Where it is | PR |
|---|---|---|
| `Bundle.members`, `HostInfo.members`, `LaunchOrder.members`, `Agent.members` | `borant-core` model and ports | #22 |
| Fill-first by the earliest reset, one member needs no data, name order without fresh reports | `rules::members::pick` | #26 |
| The neutral `borant usage report` CLI and `ToolCall::UsageReport`; a report is fresh for 15 minutes (`REPORT_FRESH_FOR`) | `crates/borant/src/cli.rs`, `rules/members.rs` | #26 |
| `autoContinueAtUsageLimit: false` for every Claude Code launch with a member of a bundle with two or more members | `crates/borant-launch/src/harness.rs` | #74 |
| `StopFailure` hook, `MemberLimited`, the same-host restart with the next free member, the wait and wake at the reset, the loop guard | `crates/borant/src/hook.rs`, `crates/borant-hub/src/members.rs` | #74 |
| An alert when every member of a bundle is limited (`members_limited`) | `rules/alerts.rs` | #69 |

This spec builds the rest. It runs as roadmap phase 7, on fakes only.

## Goals

1. A Docker e2e scenario proves the whole flow with fakes: three agents on
   three members, a reported limit moves the agent to the next member on the
   same host, and with every member limited the agent waits, the
   `members_limited` alert fires, and the agent wakes at the reset.
2. A person switches the member of an agent by hand: `borant move <agent>
   --member <name>` (the token pools design, phase 1; the phase 1 spec cut
   it).
3. Measure (token pools phase 3): `borant members` shows the limit restarts
   of each member in the last 7 days, so Chris can decide on a balancer.

## Cuts

- Facts with real tokens (token pools phase 0). They need Chris's tokens, so
  they are a `needs_human` task.
- A balancer and a switch inside a running session. Phase 3 of the token
  pools design decides it from the numbers of goal 3.
- Pi and omp limit detection. LunaRoute needs no pool (decision 6).

## Interfaces

- `ToolCall::Move` (or the existing move call) gets `member: Option<MemberName>`.
  The hub accepts it only for an agent whose bundle has that member on the
  target host, sets `LaunchOrder.members`, and relaunches through the planned
  move path with `MoveReason::Planned`. CLI: `borant move <agent> --member <name>`.
- `MemberState` gets `restarts: Vec<Timestamp>` (serde default), kept for 7
  days. `ToolCall::ListMembers` and `borant members` show the count.
- The fake harness (`crates/borant-testkit`) gets a `LIMIT <resets_at>`
  command that sends the host the same `MemberLimited` that the Claude Code
  `StopFailure` hook sends.

## Tasks

| Task | Blocked by | Files | Proof |
|---|---|---|---|
| Docker e2e scenario `members` | none | `tests/e2e/run.sh`, `docker/`, fake harness `LIMIT` | `just e2e members` (the owner runs it on the laptop if the worker's box has no Docker) |
| Manual member switch | none | hub move path, `crates/borant/src/cli.rs` | `just test-crate borant-hub members` (a switch to a named member relaunches on the same host; an unknown member is Invalid) |
| Restart counts | none | `crates/borant-hub/src/members.rs`, `rules/members.rs`, CLI | `just test-crate borant-hub members` (a limit restart adds a time; times older than 7 days drop) |
