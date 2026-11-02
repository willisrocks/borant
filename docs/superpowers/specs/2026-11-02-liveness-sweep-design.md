# Liveness sweep and safe recovery of in-flight work

Status: approved by scope (Chris, 2026-11-02), no new config.

## The incident

After the hub outage of 2026-11-02 (about 16:50 UTC), on the macbook:

1. A helper's tmux session died. `ps` and `agents` showed it running for
   about 2 hours, and nobody was told.
2. `borant resume` placed it, but the launch failed: `git checkout -f -B main`
   hit a stale worktree that held `main`. The agent worked on another branch,
   and that path force-checks out over a dirty tree.
3. After a manual `git worktree prune`, resume returned `placed: false` and
   `ps` kept "failed recently" with the old error. Nothing retried.
4. A worker stayed queued and offline, and nobody was told.
5. A launch prompt sat in the input box, unsent.
6. An agent's borant MCP server disconnected and did not come back.
7. A session that ended the day before still showed running.

## Root causes

- **Lost session ends.** The host tells the hub that a session ended in a
  single call. When the hub is down, the end is lost
  (`borant-host/src/sessions.rs` `end_session`). The same happens for saved
  sessions found dead after a daemon restart (`persist.rs`) and for fenced
  sessions. Without the end, the hub only expires the lease. It never calls
  `relaunch_after_session_end`, never tells the owner, and leaves the spawn
  record `Running` for good.
- **PID-only liveness.** The host checks only the PID that the hook reported.
  A dead tmux session with a live or reused PID looks alive.
- **The branch at launch.** `agent.repo.branch` is read once, when the
  session starts. A checkpoint does not record the branch, so a restore
  checks out the launch branch (`main`) under the wip tree.
- **A forced restore.** `restore` runs `checkout -f` in an existing work dir
  without pruning worktrees or saving a dirty tree.
- **"Failed recently" never ends.** `failed_hosts` has no time limit, and
  only a session registration (or a hub restart) clears it. `resume` does
  not clear it. Failed resumes also count toward the spawn's 3 launch tries,
  which retires the agent.
- **No report for a queued spawn.** A spawn that stays queued is never
  reported.
- **No sign of a dead MCP server.** The recovery message says tools work
  again, but nothing tells the agent what to do when the borant tools are
  gone.

## Design

The fix reuses existing timers and settings, and adds no config.

### Host

- **End retry.** An end that the hub did not take goes to the outbox and is
  sent again after the hub is back. The same applies to ends of saved
  sessions found dead at restore, and of fenced sessions. The hub ignores an
  end for a session that it does not know, or that has already ended.
- **Launcher check.** `check_liveness` also asks the launcher
  (`Launcher::is_running`) for a launched session. The session ends when
  either the PID or the launch is gone.
- **Branch in the checkpoint.** The `Checkpoint` event carries the branch
  that was checked out when the snapshot was taken. The hub launches a
  restore with the branch of the newest checkpoint.
- **Safe restore.** In an existing work dir, `restore`:
  1. runs `git worktree prune`;
  2. if the tree is dirty, saves it with `save_stale` (a side ref) before any
     checkout, or fails without changing anything if the save fails;
  3. checks out the checkpoint's branch, and restores the wip tree on top of
     it.

### Hub: the sweep

A new step in the hub tick runs every 60 s.

1. **Dead agent.** Take an agent with a spawn record in `Running`, an
   expired lease, and no pause. It is dead when its host's heartbeat, newer
   than the expiry, leaves the session out (the `pinned_session_dead` test,
   used for every agent), or when the lease has been expired for longer than
   `grace`. The hub ends the session the same way an end from the host
   would.
2. **Ending a session.** Ending a session, from the host or from the sweep,
   does the following:
   - sets `Session.ended`;
   - moves the spawn record to a new `Ended` state, so `ps` and `agents` stop
     showing it as running;
   - runs `relaunch_after_session_end`.
3. **Auto-recovery.** A movable worker with an in-progress claim is
   relaunched from its checkpoint on its branch, by a pending move with
   reason `Recover`. Its tries are counted on the spawn record. After 3 tries
   (the same limit as `MAX_LAUNCH_TRIES`), the claimed task gets
   `needs_human`, and nothing relaunches the worker again.
4. **The owner's DM.** Each death and each relaunch sends one direct message
   to the agent's owner. The owner is the spawner, or else the parent, or
   else the channel owner; for a helper, that is its chief. The message names
   the agent, the bound task, the last event time, and the checkpoint ref
   with its sha and branch.
5. **Silence.** An agent with a held lease, a claimed in-progress task, and
   no session events for `[alerts] stall_minutes` (default 30) gets the same
   DM, once per silence. It is not killed: a live process may be thinking.
   The same check covers a launched session whose first turn never started
   (the unsent prompt): the DM tells the owner to look at the pane.
6. **A stuck queue.** A spawn that has been `Queued` for longer than
   `reserve` (default 300 s) sends one DM to the spawner, with the queue
   reason.

### Hub: "failed recently" and resume

- `failed_hosts` keeps the time of each failure. An entry older than `grace`
  no longer blocks the host.
- `resume` and an explicit move clear the agent's failed hosts.
- A launch failure counts toward the spawn's tries only while the spawn is
  `Queued` or `Launching`. A failed resume tells the person who resumed, and
  does not retire the agent.

### MCP server

The server already makes a fresh request per tool call, so it survives a hub
outage. The text that tells an agent its tools work again also tells it
this: "if the borant MCP tools are missing, use the `borant` CLI from Bash,
and ask the person to run /mcp to reconnect."

## Tests

- Host: an end that fails is sent again after the hub returns; a dead tmux
  session ends with a live PID; restore over a dirty tree saves it first and
  keeps the checkpoint's branch; restore runs after a stale worktree that
  holds the branch.
- Hub: the sweep marks the agent dead, ends the spawn, relaunches a bound
  worker, stops at 3 tries with `needs_human`, and sends the owner one DM per
  event; "failed recently" expires after `grace` and resume clears it; a
  queued spawn gets one DM.
- e2e: kill a worker's tmux session while the hub is down, bring the hub
  back, and see the relaunch on the worker's branch with its wip files.
