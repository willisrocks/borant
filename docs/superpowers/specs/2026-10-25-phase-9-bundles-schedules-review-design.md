# Phase 9: spawn bundles, schedules, and cross-harness review

Three items from the roadmap's "Later" list, kept small:

1. Bundles on spawn and on a channel: the levels 3 and 4 of section 3.2 of
   the [secrets design](2026-10-24-secrets-and-host-bootstrap-design.md).
   Levels 1 (harness default) and the collection and role bundles exist.
2. Schedules from the
   [durable orchestration vision](2026-10-24-durable-orchestration-vision-design.md)
   (its table of later items, "Schedules"). Chris's open question 2 there
   (Borant or the steve bot as the scheduler) is closed: on 2026-10-25 he
   decided that Borant schedules recurring work. This is the small first
   version, configured only in `hub.toml`.
3. Cross-harness review as a recipe in the orchestrate skill, with no hub
   code.

Cut for later: run until done, usage and cost, the goal API.

## Goals

1. A spawn can name bundles, and a channel can add bundles to every agent in
   it. A child agent never gets a bundle that its parent does not hold.
2. The hub fires recurring work from `hub.toml`: a task from a template in a
   channel, or a message to an agent, on a cron rule in a time zone, at most
   once per fire time, also across a hub restart.
3. An owner can ask an agent of another harness to review a PR, from a
   written recipe.

## Interfaces

### Spawn and channel bundles

- `ToolCall::Spawn` gets `bundles: Vec<BundleName>` (serde default).
  `Agent.bundles` keeps the spawn level only. The MCP `spawn` tool and
  `borant spawn --bundle B` (repeatable) pass it.
- `[channels.<name>] bundles = [...]` in `hub.toml` (the `ChannelConfig` of
  phase 6).
- The hub merges, in this order without duplicates: collection, role,
  channel, spawn, into `LaunchOrder.bundles`. The host adds the harness
  default level, as today. A move or a takeover recomputes it, so a change in
  `hub.toml` applies at the next launch.
- Rule (secrets design 3.2 rule 4): an agent that spawns can name only
  bundles that it holds now (its merged list); a person can name any.
  Otherwise the spawn fails with Denied "bundle <b> is not yours to give".
- The scheduler already skips a host without every bundle of the launch.

### Schedules

- `hub.toml`:

  ```toml
  [schedules.nightly-report]
  cron = "0 6 * * *"            # five fields: minute hour day month weekday
  time_zone = "America/Los_Angeles"
  overlap = "skip"              # or "buffer_one"
  [schedules.nightly-report.task]
  channel = "reports"
  title = "Morning report for {date}"
  body = "Summarize the events of the last 24 hours."
  # or: [schedules.<name>.message] to = "<agent>", text = "..."
  ```

- The hub tick checks each schedule once a minute. A fire at time T makes the
  task with `create_key = "sched.<name>.<T as RFC 3339 UTC>"` (task create is
  idempotent by key since phase 6), or sends the message once (the hub
  records `sched.<name>` with the last fire time before it sends).
- Overlap, for a task target: `skip` makes no new task while the task of the
  previous fire is still open; `buffer_one` makes at most one waiting task
  and makes it when the previous one closes. A message target has no overlap
  rule.
- A missed fire (the hub was down) fires once at the next tick if it is less
  than one period old; older fires are skipped with one info line.
- `{date}` in the title and body is the fire date in the schedule's time zone.
- `borant schedules` (a new `ToolCall::Schedules`, allowed for a person) lists
  each schedule with its next fire time and its last fire.

### Cross-harness review recipe

- A short section in `integrations/claude-code/skills/orchestrate/SKILL.md`:
  when a PR is ready, spawn a reviewer of another harness (`--harness
  claude-code` for an omp author, or `omp` for a Claude Code author), bound to
  a review task with `stop_on_close`, with a brief that names the PR, the
  focused test command, and the answer format ("approve" or a numbered list
  of findings with file:line). The owner reads the findings before the merge.

## Tasks

| Task | Blocked by | Files | Proof |
|---|---|---|---|
| Spawn and channel bundles | none | `borant-core` proto/model, `crates/borant-hub` spawn and launch path, MCP, CLI | `just test-crate borant-hub bundles` (the merge order; a child cannot give a bundle it lacks; a person can; a move recomputes; old Agent JSON parses) |
| Schedules: config and the pure rules | none | `borant-core::rules::schedules` (next fire time from cron and time zone, the overlap decision, the missed-fire rule), config types | `just test-crate borant-core schedules` (cron fields, a DST change in the time zone, skip, buffer_one, a missed fire inside and outside one period) |
| Schedules: the hub tick and `borant schedules` | the rules task | `crates/borant-hub/src/schedules.rs`, CLI | `just test-crate borant-hub schedules` (one task per fire across a hub restart; skip and buffer_one; a message target sends once) |
| Cross-harness review recipe | none | the orchestrate skill | The owner reads it; one real review on a PR of this phase |

Each code task also runs `cargo check --workspace --all-targets`, `just fmt`,
and `just lint`. No real secret: the bundles task uses the `env` and `file`
stores and the fake exec plugin of the tests.

## Decision records

### Schedules only in hub.toml

Decision: schedules come from `hub.toml`; there are no calls to create or
delete one.
Evidence: Chris decided on 2026-10-25 that Borant, not the steve bot,
schedules recurring work (vision question 2). A config file keeps the first
version small and reversible, and borant-infra already owns `hub.toml`.
Reversible: yes. Calls can come later.

### A cron crate, not our own parser

Decision: use a maintained cron and time zone crate (for example `croner`
and `chrono-tz`) in `borant-core::rules::schedules`.
Evidence: time zone and DST rules are easy to get wrong; the rule stays pure
because the crate computes from a given time.
Reversible: yes.
