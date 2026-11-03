# Board task lines

Status: proposed for the chief mod v3 (design gate, 2026-11-02).

## Problem

The chief mod v3 shows every unfinished task of every project: who works on
it, for how long, what it waits on, and its PR state. `borant board --json`
gives only counts per project and per epic. To list tasks, the mod must run
`borant tasks <channel>` once per channel, every 5 s: about 12 reads for 10
projects. The task records also lack two facts that the mod needs. They do
not say whether an open task can start, and they hold no PR state. The hub's
PR watch keeps the PR state under `pr.<channel>.<task>`, but no read returns
it.

## Design

`BoardView` gains one list per project, `tasks`, with one line for each task
that is not closed, and one count, `done_since`.

```rust
pub struct TaskLine {
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    pub kind: TaskKind,
    /// open, in_progress, needs_human, stuck (as the record has it)
    pub status: TaskStatus,
    /// Derived, for an open task only: ready, later, or blocked.
    pub start: Option<StartState>,
    /// What it waits on: the titles of the open blocked_by tasks, the
    /// blocked spawn's reason, or "orchestrator offline".
    pub waits_on: Option<String>,
    pub claimed_by: Option<AgentId>,
    /// When the task entered its current status.
    pub since: Timestamp,
    pub attention_msg: Option<String>,
    pub pr: Option<PrLine>,
}

pub struct PrLine {
    pub url: String,
    pub state: ChangeState,   // open, merged, closed
    pub checks: CheckState,   // as the PR watch last read it, with counts
    pub review: ReviewState,
}
```

The rules, in `borant-core::board`:

- `ready`: open, nothing in `blocked_by` is open, and no blocked spawn is
  bound to the task.
- `later`: open, and a task in `blocked_by` is open. `waits_on` names it.
- `blocked`: open, and a spawn bound to it is blocked, or the channel owner
  has no session. `waits_on` gives the reason.
- `done_since`: the tasks of the project closed as done after the time that
  the caller gives (`--done-since`, default 24 hours).

The CLI gains `borant board --json --tasks [--done-since <time>]`. Without
`--tasks`, the reply is the same as today, so old mods keep working. The
tasks of a closed saga or epic are left out.

### Additions after review

- `version`: the task's `updated` time. Every action from the mod sends the
  version that the person saw (`expect_updated`), so an action on a task that
  changed after the draw is a conflict, not a silent overwrite.
- `since` comes from the time of the task's last `task.status` event. A task
  record from before that event has none, and the line falls back to
  `updated`.
- `recent_done`: the last 10 tasks of the project closed as done, with title,
  closer, time, and PR. The project view lists them by name.
- The list of lines is `task_lines`, not the spec's `tasks`: `ProjectBoard`
  already has `tasks` as the counts, and an older reader reads that. The
  count and the list are left out of the reply, not empty, when the caller
  does not ask for `--tasks`.
- The rules reuse the existing claim and readiness rules (`rules/graph.rs`)
  and do not copy them. When both apply, `blocked` wins over `later`.
- An approval inside a saga, as drawn ("approve epics 3 to 5"), needs no new
  model: it is a task in `needs_human` that the later epics have in
  `blocked_by`. Those epics show as `later`, waiting on "your approval".
  Closing the approval task as done (`y`) frees them.
- `PrLine.checks` and `review` show only what the PR port knows. Today's
  `ChangeStatus` has no check counts and no requested-review state. The port
  and its contract suite gain `checks_passed`, `checks_total`, and
  `review_requested`; an adapter without them leaves them empty, and the mod
  then shows the state alone ("checks running").
- The read carries its `at` time. The mod reads the board again after each
  action, and shows the time of a read that is older than 90 s.

## Cost

A board read already loads every task of every channel. The lines add no
store read. The PR state comes from the PR watch's keys: one read per task
with a `pr_url`.

## Tests

- `borant-core` rules: ready, later, and blocked for each case above.
- The board fixture gains task lines, and the mod's fixture is regenerated.

## Cut

No per-task history in the board: the mod reads one task's history only when
the person selects it (see `2026-11-02-person-changes-feed-design.md`).
