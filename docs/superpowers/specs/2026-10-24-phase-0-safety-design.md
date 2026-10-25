# Phase 0: safety fixes

This spec builds phase 0 of the [roadmap](2026-10-24-borant-roadmap.md). It
has three parts: restrict NATS, back up the hub, and make silent failures
visible. It also holds one urgent bug that blocks the NATS rollout: a hub
restart stops the hub from answering requests for 60 to 90 s.

The orchestrator wrote it in auto-pilot. Codex (`gpt-6.1-sol`, effort high)
gave a second opinion on the three material choices. The decision records
at the end name each choice and the evidence.

## Why

- NATS on borant-1 has no authentication. The firewall trusts all of
  `tailscale0`, and the tailnet policy allows all traffic. Any device on the
  tailnet, and any process on a box, can act as a hub or a host.
- The hub state lives only in the JetStream directory of borant-1. If the
  disk fails, the swarm loses every agent, channel, and task.
- Several failures (a failed ack, a failed session-log append, a failed hub
  request) are logged at `debug` or dropped. Operators see a stuck agent and
  no cause.
- Borant builds itself. These fixes must land without a long stop of the
  workers that build them.

## Goals

1. Only processes with a NATS password can connect to NATS. Only `tag:borant`
   boxes and Chris's devices can reach port 4222 on the tailnet.
2. A copy of the hub state, at most 5 minutes old, exists off borant-1. A
   restore drill proves that it restores.
3. The three silent failure classes log at `warn`.
4. `borant agents prune --dry-run` lists the stale agents and the keys that a
   prune removes.
5. A hub restart does not pause the swarm.

## Non-goals and cuts

- Per-role NATS subject permissions. Authentication keeps outsiders out. It
  does not stop an authenticated host from acting as the hub. The JetStream
  API subjects that each role needs are inferred, not verified, and a wrong
  list stops the swarm. Agents run as the same user as the daemons and can
  read the passwords until sandboxes land (roadmap phase 5). Permissions
  come with sandboxes.
- JWT operator mode (`nsc`, a resolver, `.creds` files). It adds moving parts
  for four users.
- TLS for NATS. Tailscale already encrypts the traffic.
- Real deletion in `agents prune`. The roadmap asks for `--dry-run`. Safe
  deletion needs a protocol against hosts that publish outside the hub lock
  (see the prune decision record).
- Channel pruning (1307 `chan.*` keys). This needs a channel close rule first.
- Host outbox retention and replay after a restore (vision document, lines
  110 to 130). A restore in phase 0 accepts the loss of messages since the
  last good backup.
- New warn logs outside the three named classes. The research list of other
  dropped errors (KV key-iterator errors aside, which the backup task fixes)
  stays for a later phase.
- Hetzner Object Storage or the laptop as backup targets (see the backup
  decision record).

## Constraints

- Workers run on the Borant that this phase changes. Deploy hub and host
  changes only between tasks. Restart one host at a time. Test on borant-2
  first.
- `borant-hub.service` has `Requires=nats.service`. A NATS restart restarts the
  hub. Until the stall fix is deployed, a hub restart pauses every session.
- The boxes run a pinned GitHub release (`borant-infra/pkgs/borant.nix`,
  v0.2.1). Code changes reach the boxes only through a release. Another
  session owns v0.3.0. Its content (PR #5, projects and `model` on spawn) is
  on `main`, but the release is not cut yet. No phase 0 release happens
  before v0.3.0 ships.
- v0.2.1 already supports `nats.user` and `nats.password` in `host.toml` and
  `hub.toml`, and the `BORANT_NATS_USER` and `BORANT_NATS_PASSWORD` variables
  (`crates/borant/src/config.rs:60-75`). The NATS rollout needs no release.
- Files that Nix generates land in the world-readable Nix store. No password
  goes into a Nix expression. A bcrypt hash of a 32-byte random password can.
- The CLI, the MCP server, the hooks, and the Pi and omp extension use only the
  Unix socket of the host daemon. They need no NATS password. `borant hub
  export`, `import`, and `move` connect to NATS directly with `hub.toml`
  (`crates/borant/src/roles.rs:35`). They need the hub password.
- Never change the Tailscale policy without the rollback in this spec.
  Never write a secret value in a file in git, a log, a task, or a message.

## Approaches

| Part | Chosen | Other approaches | Why |
|---|---|---|---|
| NATS auth | Static `authorization.users` with bcrypt hashes, one user for each process | JWT operator mode with `nsc` | Four users. No resolver. The client already supports passwords. |
| Rollout | `no_auth_user` keeps anonymous clients working, then one client at a time moves to its password | Switch all clients at once | Keeps every client working at every step. |
| Hub backup | The running hub serves a new `Snapshot` request. It writes the existing JSONL export format. A timer copies the file to borant-2. | `nats stream backup` of the three streams, with no Borant code | Reuses `hub import` and `hub move`. Stays backend-neutral. The JSONL file holds only unacked messages, not 30 days of `BORANT_MSG`. |
| Prune | Dry run only, served by the hub | Real deletion now | The roadmap asks for the dry run. Deletion races with host publishes. |

## Design

### Hub restart stall (urgent, before the rest)

After a restart, the hub uses about 30 s of CPU and does not answer
`borant.ctl` for 60 to 90 s. The store holds 1758 keys from stress tests (1307
`chan.*`, 158 `session`, 147 `lease`, 146 `agent`). Hosts pause their sessions
when the hub is silent for longer than the 30 s lease.

The suspects come from a read of the code. The worker measures before the fix:

1. `Hub::tick` holds the `mem` mutex for all of `tick_leases`. `Hub::handle`
   takes the same mutex.
2. After a restart, the memo sets in `Memory` (`claims_released`,
   `idle_checked`, `expired_at`, `wake_checked`) are empty. On the first ticks
   every expired movable agent gets `has_claims`, `unclaim_everywhere`, inbox
   reads, and a full session-log read.
3. `NatsState::list` reads every key of the bucket, filters the prefix in the
   client, then does one `get` for each key.

This is Borant task "Hub restart: answer borant.ctl within seconds with a
large store" (01M5RG3T9B649D17ASFP40M3ME) in channel `roadmap-p0`. Worker
`w-hubstall` builds it now. Its acceptance criteria:

1. The PR states the measured cause, with numbers.
2. With a store of the shape above, the hub answers a `borant.ctl` request
   within 5 s of start. The catch-up work of the tick must not block requests
   for longer than one short step.
3. The tick gives the same results for lease expiry, claim timeouts,
   takeovers, and wake checks.
4. If `NatsState::list` is part of the cause, it lists only the keys under
   the prefix, and the contract suite in `borant-core::testing` passes.

Proof: a new focused test in borant-hub that fails before the fix
(`just test-crate borant-hub <filter>`), and the NATS state tests if
borant-nats changes.

The fix reaches borant-1 only in a release. Merge it into `main` before the
other session cuts v0.3.0, so that v0.3.0 carries it. If v0.3.0 ships without
it, the release helper cuts v0.3.1 with the fix alone, before the rest of
phase 0.

### NATS users

The NATS server on borant-1 gets static users in the existing global account.
Do not put them in a named account. A named account moves their JetStream
storage to a new namespace, and the hub then sees an empty store.

| User | Process | Where the password lives on the client |
|---|---|---|
| `hub` | `borant hub` on borant-1, and the backup timer | `/var/lib/borant-secrets/nats-hub.env`, root-only, loaded with `EnvironmentFile=` |
| `host-borant-1` | `borant host` on borant-1 | `/var/lib/borant-secrets/nats-host.env`, root-only, `EnvironmentFile=` |
| `host-borant-2` | `borant host` on borant-2 | `/var/lib/borant-secrets/nats-host.env`, root-only, `EnvironmentFile=` |
| `host-macbook` | `borant host` on the laptop | `nats.user` and `nats.password` in `~/.borant/host.toml`, mode 0600 |
| `legacy` | Clients with no credentials, during the rollout only | None |

Each env file holds `BORANT_NATS_USER` and `BORANT_NATS_PASSWORD`. The
passwords are 32 random bytes (`openssl rand -base64 32`). Each password lives
in one 1Password item in the vault that `OP_AGENTS` in `borant-infra/.env`
names, titled `borant nats <user>`. This adds four new items and changes no
existing item. Rollback: delete the four items.

The NATS settings in `modules/borant.nix` hold only the bcrypt hashes
(`nats server passwd` or `htpasswd -bnBC 11`). `validateConfig` stays on. A
monitor port on `127.0.0.1:8222` shows the user of each connection at
`/connz?auth=true`.

A new `just nats-credential <box> <role>` recipe in `borant-infra` reads the
password with `op read` on the laptop and writes the env file on the box over
SSH with `sudo install -m 0600 -o root`. The value never goes to the
terminal or into a file in the repo.

### Tailscale policy

Tailscale policies only allow traffic. Restrict port 4222 by listing the
other ports:

```json
{"action": "accept", "src": ["*"], "dst": ["autogroup:member:*"]},
{"action": "accept", "src": ["*"], "dst": ["tag:borant:1-4221,4223-65535"]},
{"action": "accept", "src": ["tag:borant", "<chris's login>"], "dst": ["tag:borant:4222"]}
```

Before the change, make sure that:

1. `tailscale status --json` shows `tag:borant` on borant-1 and borant-2. If a
   box is user-owned, the `autogroup:member:*` rule opens its port 4222 again.
2. The current policy grants nothing else that these rules drop: other tags,
   SSH rules, groups. Keep those entries as they are.

The orchestrator has no Tailscale admin access. Chris applies the change in
the admin console, from a `needs_human` task that holds the exact diff.

Rollback: before the change, save the full current policy JSON to
`.orchestrator/tailscale-policy-<date>.json`. To roll back, paste it back in
the admin console. The admin console does not depend on the boxes, so a bad
policy cannot lock out the rollback.

### Rollout of NATS authentication

Each step keeps the laptop, both boxes, and the hub working. Deploy between
worker turns. After each step, make sure that heartbeats, inbox delivery, and
session-log appends work: `borant agents` shows each host online, and a
`send` to an idle worker on that box wakes it.

1. Deploy the stall fix to borant-1, so that the NATS restart in the next step
   does not pause the swarm. This needs v0.3.0 or a later release.
2. Add the five users and `no_auth_user = "legacy"` to the NATS settings.
   Deploy borant-1. NATS reloads `no_auth_user` only on a restart, so this
   step restarts NATS and the hub. Expect a short pause.
3. Push the `host-borant-2` env file. Deploy borant-2. Make sure that
   `/connz?auth=true` on borant-1 shows `host-borant-2` for its connection.
4. Do step 3 for `host-borant-1`, then for `hub` (the hub restarts, which is
   safe after step 1).
5. Write `nats.user` and `nats.password` into `~/.borant/host.toml` on the
   laptop. Restart the `dev.borant.host` LaunchAgent. Make sure that
   `/connz` shows `host-macbook`.
6. Make sure that `/connz` shows no `legacy` connection. Remove `no_auth_user`
   and the `legacy` user. Deploy borant-1.
7. From the laptop, make sure that a NATS client with no password and one with a
   wrong password both fail to connect, for example
   `nats --server nats://borant-1:4222 server ping`.

Rollback for each step: `git revert` the commit in `borant-infra` and run
`just deploy <box>`. Before step 6, an anonymous client still connects, so a
missing credential does not stop a host.

The Tailscale policy change is independent of these steps. It can land
before or after them.

### Hub snapshot

`borant hub export --snapshot --out <file>` sends a new `CtlRequest::Snapshot
{ out }` to the running hub. The hub does the export and writes the file on
its own box. The command needs no maintenance mode, and the swarm keeps
working.

`Hub::handle` holds the `mem` mutex for the whole dispatch
(`crates/borant-hub/src/lib.rs:105-115`). The snapshot must not go through
that path. `handle` sends `Snapshot` to its own function, which takes the mutex
only for the KV read. The snapshot also skips the maintenance step that
`roles::export` sends today (`crates/borant/src/roles.rs:137`).

The snapshot point is the start of the inbox read. It reads in this order:

1. The `agent.*` keys, without the mutex, for the list of agents.
2. The unacked envelopes of the inbox of each of those agents. Read the
   inboxes before the session logs. A host appends the receipt of a message to
   the session log, then acks the message. With this order, a message that was
   in an inbox at the snapshot point is in the inbox copy, the session-log
   copy, or both. Import drops duplicates by ID.
3. All KV keys, while the hub holds its request mutex. Release the mutex after
   this step. Measure how long the hub holds it.
4. The session logs of the agents in the KV state of step 3.

The snapshot accepts these losses, because they happen after the snapshot
point: a message published after its inbox was read and received after its
session log was read, and the messages of an agent created after step 1.
Import creates the inbox of that agent from the KV state, empty.

Then the hub writes the JSONL to `<out>.tmp` and renames it to `<out>`. Any
read error fails the snapshot. This includes key-iterator errors that
`NatsState::list` drops today (`crates/borant-nats/src/state.rs:114`).

`hub import` gets one fix. A pending record keeps the agent that it was
pending for. Import publishes it so that only that agent receives it again.
Today import publishes it to the channel subject, and every restored member
receives it (`crates/borant-hub/src/export.rs:112-127`).

### Backup timer

A systemd timer on borant-1 runs every 5 minutes as `chris`, with
`EnvironmentFile=/var/lib/borant-secrets/nats-hub.env`. The service:

1. Runs `borant hub export --snapshot --out ~/.borant/backup/hub-<UTC time>.jsonl`.
2. Copies the file to `borant-2:~/.borant/hub-backup/` as `<name>.tmp`, then
   renames it on borant-2.
3. Deletes local files older than 1 hour, and files on borant-2 older than
   24 hours.

If a step fails, the service exits non-zero. `systemctl --failed` on borant-1
shows it.

borant-2 is a temporary target. It departs from the roadmap, which names
object storage or the laptop, because borant-2 can be deleted in phase 4.
Before phase 4 deletes borant-2, a new target must hold a verified copy.

Restore runbook (written into `borant-infra/README.md`):

1. Stop `borant-hub` and `nats` on the new hub box. Move the JetStream
   directory (`/var/lib/nats`) aside.
2. Start `nats`.
3. Load the hub env file: `set -a; . <(sudo cat /var/lib/borant-secrets/nats-hub.env); set +a`.
4. Run `borant hub import <newest file>`.
5. Start `borant-hub`.

### Silent failures at warn

Change these sites to `tracing::warn!` with the agent or subject in the event:

| Class | Sites |
|---|---|
| Failed ack | `crates/borant-host/src/delivery.rs:82`, `:97-98`, `crates/borant-nats/src/bus.rs:98` |
| Failed session-log append | `crates/borant-host/src/delivery.rs:94`, `:408`, `:444-451` (`flush_outbox`, including undecodable items that it deletes) |
| Failed hub request | `crates/borant-host/src/commands.rs:56` (`LaunchFailed`), `crates/borant-host/src/lib.rs:270` (`RegisterHost`), `crates/borant-host/src/sessions.rs:146` (`RegisterSession`), `crates/borant-host/src/lib.rs:304-314` (log the first failed heartbeat, not only after the lease time), `crates/borant-hub/src/lib.rs:112` (warn only for the internal and unavailable error codes, so that ordinary invalid and not-found replies stay at `debug`) |

A loop that can fail each second (the inbox pull, the heartbeat) logs the
first failure at `warn` and the later ones at `debug` until it succeeds again.

### `borant agents prune --dry-run`

A new read-only `ToolCall::PruneAgents { older_than }`. The CLI sends it to
the host daemon on `/v1/call` (`crates/borant-host/src/api.rs:77,154`), like
`borant agents`, and the host relays it to the hub. It works from any host. An
agent session can call it too, which is harmless because it writes nothing.
Old hosts reject the new variant, so it works on a host only after the phase 0
release reaches that host.

The hub must not hold the `mem` mutex while it reads about 146 inboxes and
session logs, because every heartbeat waits for that mutex. Like the
snapshot, the hub reads the KV state under the mutex, releases it, then reads
the inboxes and the session logs. The CLI prints the result. Without `--dry-run`, the command exits with
"only --dry-run is available". `--older-than-hours` sets the age, default 24.

An agent is a candidate only if all of these are true:

1. `agent.session` is `None`.
2. Its lease is `Expired` or `Released`, with `since` older than `older_than`.
3. It has no pending move in hub memory.
4. It claims no task.
5. It owns no channel.
6. `bus.unacked` returns no message. `inbox_pending` is not enough, because it
   leaves out delivered messages that wait for an ack.
7. `session_log::undelivered` of its session log is empty.

The output has one line for each candidate, the reason each other agent is
kept, and totals: candidates, keys that a prune removes (`agent.*`,
`lease.agent.*`, `chan.*.member.*`, the `session.*` records of the agent), and
their bytes.

## Tasks

The orchestrator does the `borant-infra` tasks and the deploys, because
workers must not change secrets or infrastructure. Workers do the code tasks.

| Task | Who | Blocked by |
|---|---|---|
| Hub restart stall | worker | none |
| Warn on silent failures | worker | none |
| Hub snapshot export | worker | Hub restart stall |
| Agents prune dry run | worker | none |
| Release with the stall fix (v0.3.0, or v0.3.1 if v0.3.0 ships without it) | other session, else release helper | Hub restart stall |
| Deploy the stall fix to borant-1 | orchestrator | Release with the stall fix |
| NATS users with anonymous fallback | orchestrator | Deploy the stall fix |
| NATS passwords on each client | orchestrator | NATS users with anonymous fallback |
| Remove anonymous NATS access | orchestrator | NATS passwords on each client |
| Tailscale policy for port 4222 | Chris | none |
| Phase 0 release and deploy | release helper | Warn on silent failures, Hub snapshot export, Agents prune dry run |
| Backup timer and restore drill | orchestrator | Phase 0 release and deploy, NATS passwords on each client |

### Warn on silent failures

Acceptance:

1. Each site in the table in "Silent failures at warn" logs at `warn`, with the
   agent ID or subject.
2. A loop that fails each second logs one `warn` per run of failures.
3. No behavior changes apart from the log level and the first-heartbeat log.

Proof: `just test-crate borant-host delivery` and `just lint` pass. No new
test: the change is the log level only.

### Hub snapshot export

Acceptance:

1. `borant hub export --snapshot --out F` writes F on the hub box while the
   hub keeps serving requests. No maintenance mode.
2. The hub holds its request mutex only for the KV read. The PR states the
   measured hold time for a store of 1758 keys. It is under 1 s.
3. A read error, including a key-iterator error, fails the snapshot, and no
   file appears at F.
4. A message that a host receives during the snapshot is in the snapshot.
5. After import, a pending message reaches only the agent that it was
   pending for.
6. `borant hub export` without `--snapshot` works as before.

Proof:

- `just test-crate borant-hub export`, with new tests for acceptance items 3,
  4, and 5 on the memory adapters.
- A restore drill on the laptop against the NATS of `just nats-up`, with
  `BORANT_NATS_URL=nats://127.0.0.1:4222` and a separate `BORANT_HOME`, so that
  the real `dev.borant.host` daemon is not used. Seed state, take a snapshot
  while messages flow, then `just nats-down`, `just nats-up` for an empty
  store, and `borant hub import` the file. Compare the import report with the
  snapshot (KV keys, session events, pending messages), and count the keys
  with `nats kv ls borant`. Do not start a hub on the restored store, because
  it can try takeovers and launches. Put the commands and the result in the
  PR.

### Agents prune dry run

Acceptance:

1. `borant agents prune --dry-run` lists candidates by the seven rules in
   "`borant agents prune --dry-run`", the kept agents with their reason, and
   the totals.
2. The request writes no key and deletes nothing.
3. `borant agents prune` without `--dry-run` exits non-zero with "only
   --dry-run is available".

Proof: `just test-crate borant-hub prune`, with one test for each of the seven
rules and one that asserts the store is unchanged after the request.

### Orchestrator and Chris tasks

| Task | Done when |
|---|---|
| Deploy the stall fix to borant-1 | `systemctl restart borant-hub` on borant-1, and `borant agents` on the laptop answers within 5 s. No session pauses. |
| NATS users with anonymous fallback | Step 2 of the rollout. `nix build` of the borant-1 system passes `validateConfig`. Every host is online after the restart. |
| NATS passwords on each client | Steps 3 to 5. `/connz?auth=true` shows `hub`, `host-borant-1`, `host-borant-2`, and `host-macbook`, and no `legacy`. |
| Remove anonymous NATS access | Steps 6 and 7. An anonymous client and a wrong-password client both fail. |
| Tailscale policy for port 4222 | The policy is saved, the change is applied, and from a device outside `tag:borant` and Chris's login, port 4222 on borant-1 does not answer. |
| Backup timer and restore drill | Two timer runs in a row succeed. A file on borant-2 is less than 5 minutes old. The newest file imports into an empty NATS on the laptop (`just nats-up`, separate `BORANT_HOME`, no hub started), and the import report matches the key count of `nats kv ls borant`. |

## Decision records

Decision: Static NATS users with bcrypt hashes in the Nix config, one user for
each process, in the global account. Full permissions for each user in phase 0.
Evidence: the client supports user and password
(`crates/borant-nats/src/lib.rs:68-76`). Nix files are world-readable. Agents
run as the daemon user. A named account changes the JetStream namespace.
Reversible: yes, revert the `borant-infra` commit. Friend: Codex agreed, and
asked for separate env files for the hub and the host on borant-1, and for a
plain statement that authentication does not stop a host from acting as the
hub. Blocker: none.

Decision: Roll out with `no_auth_user`, borant-2 first, and remove the legacy
user last. Evidence: each step keeps every client connected. Reversible: yes.
Friend: Codex said that `no_auth_user` needs a NATS restart, which restarts the
hub, so the stall fix must be deployed first. Codex also suggested the
Tailscale change first. It stays independent, because it waits on Chris and
the NATS steps do not. Blocker: v0.3.0 for the stall fix.

Decision: The running hub writes a JSONL snapshot, read in the order inbox,
then KV under the mutex, then session logs. Import sends a pending message
only to its agent. Evidence: `crates/borant-hub/src/export.rs:48` reads logs
before inboxes, and `crates/borant-host/src/delivery.rs:85` appends before it
acks, so the old order can lose a message. Import publishes pending messages
to the channel subject (`export.rs:112-127`). Reversible: yes. Friend: Codex
found both gaps and the dropped key-iterator errors. It suggested
`nats stream backup` as an interim step while releases are blocked. Rejected:
the release block ends with v0.3.0, the JSONL path serves `hub move` too, and
the stream backup copies 30 days of `BORANT_MSG`. Blocker: none.

Decision: borant-2 is the temporary backup target. Evidence: box-to-box SSH
works today. Object storage needs a new credential and a bill. Reversible: yes.
Friend: Codex agreed if the spec records the exception and requires a new
verified target before phase 4 deletes borant-2. Blocker: none.

Decision: `agents prune` is a dry run only in phase 0. Evidence: the roadmap
asks for `--dry-run`. A host acks a message before it delivers it, so an empty
inbox does not prove that no work waits (`session_log::undelivered`). Hosts
publish outside the hub mutex, so a delete can race with a publish. Reversible:
yes. Friend: Codex recommended the dry run only, and added rules 6 and 7.
Blocker: none.
