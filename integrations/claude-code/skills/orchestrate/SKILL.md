---
name: orchestrate
description: Use when this session must run a project as the owner of a Borant channel - splitting work into a task graph, spawning worker agents on other hosts, reviewing and merging their PRs - or when a Borant task event, a worker's "ready" message, or a needs_human question arrives in such a session.
---

# Orchestrate a project with Borant

The owner owns the board, not the code. Workers build; the owner writes the tasks,
briefs the workers, merges, and tells the user only what needs them. The Borant MCP
tools do the work; this skill is how to run the role, and the Borant facts that a
general plan gets wrong.

## Borant facts that change the plan

| Fact | So |
|---|---|
| A spawn in a channel with a project starts in its own clone of the project, on the channel branch. A missing channel branch starts from the project's default branch. A channel without a project copies your repo, on your branch if it exists on `origin`, else the default branch. | Use a project channel. Without one, push your branch before you spawn. Never tell a worker to clone. Work outside its clone is not checkpointed and is lost when the worker moves. |
| `spawn` returning `placed: false` means queued. The record survives a hub restart, and the hub places it on the next free slot. | Do nothing. Never spawn again for the same task: that makes a second worker. |
| `spawn` takes `key`, an idempotency key. A retry with the same key and arguments returns the same agent. | A lost reply is safe to retry with its key. Never spawn again for the same task without the key. |
| Each host runs a fixed number of agents, and an idle worker keeps its slot. A slot comes back two ways: a task-bound worker with `stop_on_close` (the default) stops when its task closes, and `borant cancel <agent>` stops an unbound worker or cancels a queued spawn. | Give the next ready task to an idle worker before you spawn a new one, and bind every worker to its task so its slot comes back. Spawn only for work that runs in parallel; use cancel only for a worker that has no task. |
| A worker spawned with `--continue 5` and bound to its task gets up to 5 "Continue with task ..." messages, each when it goes idle while its task is `in_progress` and claimed by it. After the fifth, the hub sets the task `needs_human` with "continue limit reached (5)" and sends nothing more. `--continue` without a bound task is refused, and `--continue-hours H` caps the continues by session hours. | Spawn every worker with `--task` and `--continue 5`. A worker that is done calls the `wait` tool on its task, which stops the continues; a continue that ends in `needs_human` is a real question for the user. |
| A launch that does not register fails after `launch_timeout_secs` (default 90), and the hub tries at most 3 launches. Then the spawn is Failed. | Pick a different host or harness, or `borant cancel <agent>`. A Failed spawn does not retry: it waits for a person. |
| A direct `send` wakes an idle agent. A broadcast to `#channel` reaches members at their next turn, unless `urgent: true`. | Direct messages for orders. Broadcasts only for news that can wait, or with `urgent`. |
| You wake when a task in your channel closes, gets stuck, or needs a human, and when anyone sends you a message. | End your turn while you wait. Do not poll `task_list` in a loop. |
| A worker can move to another host when its host sleeps or fails. It reads `session_history` and continues. At most one turn is lost. | A quiet worker is not a failure. Check `members` for `online` before you act on silence. |
| `spawn` takes `harness` and `model`. The model reaches the harness as `--model`. If placement must use another harness, the hub drops the model. | Always set both, as a pair the user approved: for example cheap workers on `omp` with a cheap model, hard work on `claude-code`. Ask the user before you move work to a more expensive pair, unless they gave standing permission. |
| `spawn` takes `host`. | A helper for your own steps (rebases, release notes, releases) runs as `claude-code` on your own host. Brief it like a worker, plus the permissions the user granted for those steps. |
| `task_list(ready_only: true)` returns open tasks whose `blocked_by` tasks are all closed. | Close a task only when its work is on the base branch, so the tasks after it see real code. |
| A saga's gate holds every claim and spawn in the channel until it closes as `done`; before that the hub refuses both with `saga_not_approved`, and `task_list(ready_only)` returns nothing. | Create the epics, set the gate to `needs_human`, and end the turn. Only a person or a role with `approves = true` approves it. |
| An epic is a task of kind `epic` that holds tasks; it is never claimed, no spawn binds to it, and the board derives its status from its children. | Create every epic's tasks with `parent` set to the epic, and spawn only for plain tasks. |
| A task is ready only when its own `blocked_by` and the `blocked_by` of each parent are closed, so an epic blocked by another epic holds back every child of it. | Set `blocked_by` between epics for order, and create every task before the first spawn. |
| A spawn bound to a task starts on the `branch` property of the task's epic, if the epic has one; otherwise on the channel branch. The branch must be on `origin`. | Keep the epic's `branch` at the top branch of its stack, and push that branch before you spawn. |
| `gh stack` keeps its metadata in the local clone, so only you run it, in your own clone; the host of an orchestrator needs the `gh-stack` extension of the GitHub CLI. | Run `gh stack` yourself; workers push a branch and open a PR, and never run it. |
| `channel_close` closes a channel, only for its owner or a person, and only when every task of it is closed; a closed channel refuses `task_create`, `task_claim`, and `spawn`. | Close a saga when its last epic closes; close a wontfix saga's epics as `wontfix` first. |
| In a project with `mirror_issues` on, the hub copies each saga, epic, and task with a PR to the issue tracker (Linear) and writes the key into `issue`. A create needs a team: the item's `issue_target`, an ancestor's, or the parent issue's. With none, the item goes `needs_human` with 'Issue team needed'. | Set `issue_target` (the Linear team key, for example `ENG`) at `channel_create` for a saga or an epic. A plain project channel has no anchor, so its tasks inherit nothing from the channel: set `issue_target` on each task that will get a PR, or on a parent task they hang under. When you do not know the team, ask the chief before you create. Answer a team question by `task_update(issue_target: ...)`; the hub then restores the task's status. |
| The user is not an agent. `send` cannot reach them. | A question for the user: `task_update(status: "needs_human", attention_msg: ...)`, and say it in this session. |

## Set up

1. `whoami`. Note your agent ID: workers send to it.
2. Note the base branch and the focused test command of the repo. If the repo is not
   a Borant project yet, run `borant project add <name> --from-cwd` (`project_list`
   shows the projects).
3. Size the goal: a plain task runs in the project channel; a standalone epic
   needs `channel_create` with `kind: "epic"` and a `title`; a saga needs
   `channel_create` with `kind: "saga"`, a `title`, the `issue` link, and
   `issue_target`. An epic channel's hub-created root task is the epic; a saga
   holds a gate and the epics.
4. Create every task before the first spawn. The body is the worker's brief: the
   acceptance criteria, the files in scope, and the test that proves it. Set
   `blocked_by` for order.
5. Spawn one worker for each task that is ready now, with the brief below.

## Sagas and epics

A saga is one approval, then a set of epics that run without more gates.

1. Create the saga: `channel_create` with `kind: "saga"`, `title`, `issue`,
   `issue_target`, `project`, and base `branch`. The hub also creates the gate
   task "Approve the scope and the epics".
2. Create the epics: `task_create` with `kind: "epic"` and a body that states the
   scope of each; set `blocked_by` between epics for order.
3. Set the gate to `needs_human` with `task_update`, and an `attention_msg` that
   states the scope and lists the epics. End the turn.
4. After the approval, create every epic's tasks with `parent` set to their epic,
   then spawn for the ready tasks.
5. When the last epic closes, `channel_close` the saga. If the gate closes as
   `wontfix`, close the epics as `wontfix`, then `channel_close`.

## Epic stacks

The PRs of one epic's tasks form one linear stack on the base branch. The tasks
are a DAG, so acceptance is the order.

1. One owner of the stack: `gh stack` keeps its metadata in the local clone, so
   only you run it, in your own clone, and your host needs the `gh-stack`
   extension of the GitHub CLI. Workers push a branch and open a PR.
2. Start: when an epic becomes ready, set the epic task's `branch` property to
   the base branch. Run no `gh stack` yet: `gh stack init --base <base>` with no
   branch names fails without a terminal ("interactive input required; provide
   branch names as arguments"), so the stack starts on the first accepted PR.
3. Accept the first PR: on a worker's "ready", review it, `git fetch origin
   <branch>:<branch>`, then `gh stack init --base <base> <branch>`, which adopts
   the pushed branch as the stack. Then `gh stack rebase` and `gh stack submit
   --auto`. `submit` retargets the PR to the branch below it and links the stack;
   new PRs it creates are drafts unless `--open`, and the worker's PR already
   exists so it is retargeted. Set the epic's `branch` and `pr_url` to the new
   top, and close the task.
4. Accept each later PR: `git fetch origin <branch>:<branch>`, `gh stack top`
   (the current branch must be the top of the stack, else `gh stack add` answers
   "current branch is not part of a stack"), then `gh stack add <branch>`, which
   prints "Adopted existing branch". Then `gh stack rebase` and `gh stack submit
   --auto` as above; set `branch` and `pr_url` to the new top, and close the task.
5. Order: a task starts only after its `blocked_by` closed and closes when its PR
   is in the stack, so the acceptance order is a topological order of the DAG.
6. Parallel tasks both start on the same top; the second accepted is rebased onto
   the first by `gh stack rebase`. On a conflict, run `gh stack rebase --abort`
   before you send the task back to its worker.
7. A change lower in the stack, a review fix for example, gets `gh stack rebase`
   and `gh stack submit --auto`, so every PR above it is rebased up.
8. Merge only when every child of the epic is closed and the CI of every PR is
   green: `gh stack merge --yes` up to the top PR lands the stack at once. Branch
   protection still applies.

## Parallelize

Be mildly aggressive: run work in parallel by default, and fix the merge conflicts
yourself. A conflict costs you one rebase; an idle slot costs the whole wait.

- Start every task that has no real code dependency on an open task. Use `blocked_by`
  only when a task needs another task's code or decision, never for deploy order,
  release order, or a conflict that might happen.
- Split work along files and modules, so tasks rarely touch the same file. When two
  tasks must touch one file, run them anyway and decide the merge order up front.
- When tasks share a new interface, write the interface (names, types, signatures) in
  both task bodies, so they build against it at the same time.
- Keep the slots full: when a task closes, hand out the next ready task at once.
- Serialize only deploys, releases, and changes to shared live state (NATS
  configuration, data migrations). Make each one its own task after the code tasks.
- Do not split below one reviewable PR with one focused test. Never put two workers on
  the same files, and never spawn more workers than free slots.
- Merge in the order the PRs get ready. Rebase each later PR yourself or with your
  helper, rerun its focused test, and let CI run again. Tell the workers whose files
  the merge touched to pull before their next commit.

## Worker brief

Fill every slot. A worker reads only this prompt and its task.

```
You are worker {name} in Borant channel {channel}. The owner is agent {owner_id}.
Your task is {task_id}: "{title}". Read it with task_get, then task_claim it.
You are already in your own clone of the repo, on branch {base}. Do not clone again.
Create the branch {branch} here, and commit to it after each step.
Scope: only this task. Other workers build {siblings}; do not edit {their_files}.
Prove it with: {focused_test}. CI runs the full suite.
When done: push, open a PR against {base}, then send the owner one message:
"ready {task_id}: <PR URL> <head SHA> <test result>".
When your work is in the owner's hands, call the wait tool on your task for "closed"
(`wait(channel: "{channel}", task: "{task_id}", until: ["closed"])`), and never close
it yourself: the owner closes after the merge.
Never merge, close a task, or change CI, secrets, releases, or infrastructure.
Other agents share this host and its user. Stop only a process whose PID you
started, never by name (no pkill, no killall). Never run `tmux kill-server` or kill
a tmux session that you did not create; a tmux test uses its own socket
(`tmux -L test-{name} ...`).
If you are blocked or need a decision: task_update(status: "stuck" or "needs_human",
attention_msg: <what you need>), send the owner one line, then wait.
After the owner says "merged", wait for your next task. Do not start other work.
```

## Cross-harness review

A PR gets a reviewer from the other harness: an `omp` author gets
`--harness claude-code`, a `claude-code` author gets `--harness omp`. Create the
review task first, then spawn the reviewer bound to it with `--task`,
`--stop-on-close`, `--continue 5`, and a `--key` naming the PR, so a lost reply
retries safely:

```
borant spawn --channel {channel} --prompt-file review.md --harness {other} \
  --task {channel}/{review_task} --stop-on-close --continue 5 --key review-{pr}
```

Read the findings yourself before the merge. A finding goes back to the author,
never to the reviewer; approve only when the focused test passes on the head SHA.

```
You are reviewer {name} in Borant channel {channel}. The owner is agent {owner_id}.
Your task is {task_id}: "{title}". Read it with task_get, then task_claim it.
You are in your own clone of the repo, on {base}; do not clone again.
Review PR {pr_url} at head {head_sha}. Do not change the code, commit, or push.
Run only the focused test: {focused_test}.
Answer the owner in one message: "approve", or a numbered list of findings, each
with file:line and why it matters. A finding goes back to the author.
If you are blocked: task_update(status: "stuck", attention_msg: <what you need>),
send the owner one line, then wait.
```

## Roles and tags

When the hub runs roles (phase 1b), name the role on every spawn and let the role decide
the rest:

- A worker spawn: `role: "worker"`, bound to its task and passing `--continue 5`.
  Leave `harness` and `model` off; the role's pair is the approved one. Pass them
  only when the user asked for that child. From a shell:

  ```
  borant spawn --channel {channel} --prompt-file brief.md --role worker \
    --task {channel}/{task} --stop-on-close --continue 5
  ```

  Every worker spawn binds the worker to its task and passes `--continue 5`, so a
  worker that goes idle before it finishes gets a continue instead of dying quiet.
- A helper for your own steps: `role: "helper"`. It runs on your host.
- A spawn from a roled agent must name a role that its own role allows; the hub refuses
  the rest with `denied`.
- The role also sets the placement tags and the skill line in the brief. In a
  collection, a channel name starts with `<collection>-`.

`chief`, `orchestrator`, `worker`, `helper`, `laptop`, and `vps` are examples from
`hub.toml` and `host.toml`, not code.

## Event loop

- **Worker says ready:** read the diff and the checks yourself. Merge only in scope
  and green. Then `task_close(reason: "done", message: "merged <url>")` and tell the
  worker. When two PRs touch one file, merge one, and tell the other worker to rebase.
- **Keep the merge queue short:** never wait on CI in the foreground. Start each CI
  watch in the background and review the next PR while it runs. A failed check goes
  back to its worker at once with the failing job. With more than three PRs waiting,
  hand first-pass reviews to your helper or to review subagents, and keep the merge
  decision yourself.
- **A task closed:** `task_list(ready_only: true)`. Give each ready task to an idle
  worker: `send` it a new brief that first says "git pull origin {base}", because its
  clone is older than the merges. Spawn only if no worker is idle.
- **A task is stuck:** read `attention_msg`. Answer, or rewrite the task body and
  brief again. After two failed tries, tell the user and suggest a stronger harness.
- **A task needs a human:** put the question to the user in this session, with the
  background and your suggested default. Do not guess product decisions.
- **Long wait instead of polling:** `wait(channel, task, until, timeout_secs)` (MCP) or
  `borant wait --task CHANNEL/ID` returns when the task reaches a named state, woken by
  a direct message, also across a hub restart. For a task that needs a person:
  `borant inbox`, then `borant answer CHANNEL/ID <text>` — CLI calls, not MCP tools.
- **`placed: false`:** nothing. The spawn is queued, and the record survives a hub restart.
- **All tasks closed:** check that every PR is merged. Send each worker "project done"
  as a direct message. Report to the user: task, PR, state.

## Role rules

- Only the owner merges. Releases, deploys, and person-only steps (secrets, approvals,
  labels) run only when the user asked or a standing permission covers them.
- Only the channel owner (or a person) closes a task as `done`; a worker's done-close is
  refused, so the owner closes after the merge.
- When the user puts the project in auto-pilot, do every merge and step the tasks
  need, then report. Stop only for a permission the harness denied, or a judgment that
  must be a person's.
- Status to the user: nothing changed, one line naming what waits on them. Something
  changed, a short table: task (plain title, ID in parentheses), state, needs user.

## Red flags

| Thought | Do instead |
|---|---|
| "Spawn before the approval" | The hub refuses it. Set the gate to needs_human and end the turn. |
| "Close the gate myself" | Only a person or the chief approves. |
| "The worker should clone the repo first" | It is already in its clone. Use a project channel, or push your branch before the spawn. |
| "Spawn returned placed: false, retry" | It is queued. Wait. |
| "Tell the worker to stop when it is done" | Bind it to its task with `stop_on_close`, so the slot frees on close; otherwise `borant cancel <agent>`. |
| "Broadcast 'all done' to the channel" | Idle workers do not see it. Send directly. |
| "Close the task when the PR opens" | Close it after the merge. The next tasks depend on it. |
| "Wait for that PR to merge, to avoid a conflict" | Run both. Rebase the later one yourself. |
| "Block this task until the deploy" | Code it now. Make the deploy its own task. |
| "The worker is quiet, spawn a replacement" | Check `members`. It may be moving hosts. |
| "Send the user a message with `send`" | `needs_human` on the task, and say it here. |
| "Guess the Linear team" | Ask the chief. A wrong team puts the issue in the wrong place; the mirror never moves it. |
