# Project mirrors on each host

Each host keeps one bare mirror for each project, and each agent of that
project borrows its objects. A spawn then fetches only new commits, and N
agents on a host share one object store instead of N full clones. This is the
"Projects" rule of the
[orchestration document](2026-10-24-orchestration-api-and-sandboxes-design.md),
with one change, below.

## Decision: `git clone --reference`, not `git worktree`

The orchestration document says "a `git worktree` per agent". This spec uses
`git clone --reference <mirror>` for each agent instead.

Git refuses to check out one branch in two worktrees of one repository.
`checkpoint::restore` (`crates/borant-host/src/checkpoint.rs`) runs
`checkout -f -B <branch>` for each agent, and all agents of a channel start on
the channel branch. With worktrees, the second agent on a host fails. A
workaround (`--ignore-other-worktrees`, detached heads) lets a commit of one
agent move the branch under another. Worktrees also share one git
configuration, and `set_checkpoint_remote` writes a value for each agent.

A clone with `--reference` keeps its own refs, `HEAD`, and configuration, and
borrows the objects of the mirror through `objects/info/alternates`. So
`restore`, the checkpoints, and `set_checkpoint_remote` do not change.

Decision: clone with `--reference`. Evidence: the git rule above, and the
current `restore`. Reversible: yes, the layout is local to each host.
Friend: Codex agreed, and added the lock, the refspec, the retention rules,
and the sandbox note below. Blocker: none.

## Design

1. The mirror of project `<name>` is `~/.borant/projects/<name>.git` (under
   `BORANT_HOME`). The host creates it with `git clone --bare <remote_url>`,
   then sets `remote.origin.fetch` to `+refs/heads/*:refs/remotes/origin/*`,
   `gc.auto` to `0`, `gc.pruneExpire` to `never`, and `maintenance.auto` to
   `false`. It creates the mirror in a temporary directory and renames it into
   place only after these steps succeed.
2. Before each restore of an agent of a project, the host runs `git fetch
   --quiet origin` in the mirror, under a lock for that project. The lock is
   held only for the creation and the fetch.
3. A new agent directory is `git clone --quiet --reference <mirror>
   <remote_url> <dir>`. The `origin` of the agent stays the real remote. An
   existing agent directory keeps `git fetch origin`, as today.
4. A launch without a project keeps today's full clone.
5. The host never prunes, repacks, or deletes a mirror. A mirror stays while
   any agent directory borrows from it.

Moves need no change: the destination host makes a fresh clone from its own
mirror, then fetches the WIP ref from the checkpoint remote.

## Notes for later phases

- The bubblewrap sandbox (roadmap phase 5) must expose the mirror read-only at
  the same path, because the alternates file holds that path.
- Agent commits and WIP objects stay in the agent clone. Only upstream objects
  are shared.

## Cut

- `git worktree`, for the reasons above.
- Mirrors for launches without a project.
- Cleanup of mirrors and agent directories. `borant agents prune` is a dry
  run in phase 0.
