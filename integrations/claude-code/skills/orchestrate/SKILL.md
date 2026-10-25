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
| A spawned worker starts in its own clone of your repo, in `~/.borant/work/<agent>/<repo>`, on your current branch if that branch exists on `origin`. Otherwise it gets the default branch. | Push your branch before you spawn. Never tell a worker to clone. Work outside its clone is not checkpointed and is lost when the worker moves. |
| `spawn` returning `placed: false` means queued. The hub starts the worker when a host slot opens. | Do nothing. Never spawn again for the same task: that makes a second worker. |
| Each host runs a fixed number of agents, and an idle worker keeps its slot. No tool stops a worker yet. | Give the next ready task to an idle worker before you spawn a new one. Spawn only for work that runs in parallel. |
| A direct `send` wakes an idle agent. A broadcast to `#channel` reaches members at their next turn, unless `urgent: true`. | Direct messages for orders. Broadcasts only for news that can wait, or with `urgent`. |
| You wake when a task in your channel closes, gets stuck, or needs a human, and when anyone sends you a message. | End your turn while you wait. Do not poll `task_list` in a loop. |
| A worker can move to another host when its host sleeps or fails. It reads `session_history` and continues. At most one turn is lost. | A quiet worker is not a failure. Check `members` for `online` before you act on silence. |
| `spawn` has no model argument yet. The worker uses its harness default. | Pick the harness: `omp` (or `pi`) for cheap workers, `claude-code` for hard work. Ask the user before you move work to a more expensive harness. |
| `task_list(ready_only: true)` returns open tasks whose `blocked_by` tasks are all closed. | Close a task only when its work is on the base branch, so the tasks after it see real code. |
| The user is not an agent. `send` cannot reach them. | A question for the user: `task_update(status: "needs_human", attention_msg: ...)`, and say it in this session. |

## Set up

1. `whoami`. Note your agent ID: workers send to it.
2. Push your branch. Note the base branch and the focused test command of the repo.
3. `channel_create` one channel for the project or epic.
4. Create every task before the first spawn. The body is the worker's brief: the
   acceptance criteria, the files in scope, and the test that proves it. Set
   `blocked_by` for order.
5. Spawn one worker for each task that is ready now, with the brief below.

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
Never merge, close a task, or change CI, secrets, releases, or infrastructure.
If you are blocked or need a decision: task_update(status: "stuck" or "needs_human",
attention_msg: <what you need>), send the owner one line, then wait.
After the owner says "merged", wait for your next task. Do not start other work.
```

## Event loop

- **Worker says ready:** read the diff and the checks yourself. Merge only in scope
  and green. Then `task_close(reason: "done", message: "merged <url>")` and tell the
  worker. When two PRs touch one file, merge one, and tell the other worker to rebase.
- **A task closed:** `task_list(ready_only: true)`. Give each ready task to an idle
  worker: `send` it a new brief that first says "git pull origin {base}", because its
  clone is older than the merges. Spawn only if no worker is idle.
- **A task is stuck:** read `attention_msg`. Answer, or rewrite the task body and
  brief again. After two failed tries, tell the user and suggest a stronger harness.
- **A task needs a human:** put the question to the user in this session, with the
  background and your suggested default. Do not guess product decisions.
- **`placed: false`:** nothing. The spawn is queued.
- **All tasks closed:** check that every PR is merged. Send each worker "project done"
  as a direct message. Report to the user: task, PR, state.

## Role rules

- Only the owner merges. Releases, deploys, and person-only steps (secrets, approvals,
  labels) run only when the user asked or a standing permission covers them.
- When the user puts the project in auto-pilot, do every merge and step the tasks
  need, then report. Stop only for a permission the harness denied, or a judgment that
  must be a person's.
- Status to the user: nothing changed, one line naming what waits on them. Something
  changed, a short table: task (plain title, ID in parentheses), state, needs user.

## Red flags

| Thought | Do instead |
|---|---|
| "The worker should clone the repo first" | It is already in its clone. Push your branch before the spawn. |
| "Spawn returned placed: false, retry" | It is queued. Wait. |
| "Tell the worker to stop when it is done" | No tool stops it. Give it the next task. |
| "Broadcast 'all done' to the channel" | Idle workers do not see it. Send directly. |
| "Close the task when the PR opens" | Close it after the merge. The next tasks depend on it. |
| "The worker is quiet, spawn a replacement" | Check `members`. It may be moving hosts. |
| "Send the user a message with `send`" | `needs_human` on the task, and say it here. |
