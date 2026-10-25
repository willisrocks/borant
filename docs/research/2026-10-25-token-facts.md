# Token pool facts on borant-2

Task 01M5SD9DMXREX5F5Y49Z56HGDM. Input for the phase 0 facts of the token
pools design (`docs/superpowers/specs/2026-10-25-token-pools-design.md`,
section 5). Measured on 2026-10-25 on borant-2 as the `chris` user. No token
value is read, printed, or reproduced here; members are named only.

## Result

| Fact | Result |
|---|---|
| 1. Two Claude Code sessions on one member at the same time | Both work. Two launches both got member `a`, ran eight turns at the same time, and both finished with no error |
| 2. `borant move <agent> --member b` on a running agent | Works. The session stopped at the turn end, relaunched on the same host with member `b`, and made model turns in the same `~/.claude` |
| 3. First run on borant-2 | Three seeds are needed, not two (theme and trust, then the bypass-permissions dialog). With them a pool launch starts straight into the prompt. The watchdog stops a launch that never registers but reports an **empty** screen, not the screen |
| 4. `StopFailure` input at a real limit | Skipped as instructed. The limit cannot be forced without spending a real account's window |

## Seeds, and what was not enough

`~/.claude.json` (orchestrator, 16:05 and 16:12 UTC):

```json
{"hasCompletedOnboarding": true,
 "projects": {"/home/chris/.borant/work": {"hasTrustDialogAccepted": true}}}
```

`~/.claude/settings.json` (orchestrator, 16:15:18 UTC, the file borant-1 has):

```json
{"theme": "dark", "skipDangerousModePermissionPrompt": true}
```

Measured:

- A folder trust on `/home/chris/.borant/work` covers a child working
  directory. Verified with `HOME` on a temporary directory holding a copy of
  the seeded `~/.claude.json` and `claude` started in
  `/home/chris/.borant/work/w-tokfacts`: no trust dialog.
- `bypassPermissionsModeAccepted: true` in `~/.claude.json` does **not** skip
  the bypass-permissions dialog. That key was seeded at 16:12 and the next two
  launches (tk-c1, tk-c2) still parked on the dialog. The gate is
  `skipDangerousModePermissionPrompt` in `~/.claude/settings.json`, as on
  borant-1. The same temporary home with that file started straight into the
  prompt, no theme, trust, or bypass screen.
- `theme` also belongs in `~/.claude/settings.json`; the onboarded borant-1
  keeps it there.

## 1. Two sessions on the same member

Two launches at once, both without usage reports, so the pick rule took the
first free member in name order
(`crates/borant-core/src/rules/members.rs:98`):

```console
$ borant spawn --channel tflab --prompt-file p-tk-d1.md \
    --harness claude-code --host borant-2 --name tk-d1
$ borant spawn --channel tflab --prompt-file p-tk-d2.md \
    --harness claude-code --host borant-2 --name tk-d2
```

Each agent was told to write a timestamped line per turn, eight turns of
`sleep 8`, into its own file under `/tmp/tf`.

```console
$ jq -c '.[] | select(.agent_hint | IN("tk-d1","tk-d2")) | {agent_hint, members}' ~/.borant/sessions.json
{"agent":"tk-d2","members":{"claude-auth":"a"}}
{"agent":"tk-d1","members":{"claude-auth":"a"}}

$ borant status --json | jq -c '.agents[] | select(.agent|test("^tk-d")) | {agent, host, session, lease: .lease.state}'
{"agent":"tk-d1","host":"borant-2","session":"01M5SXH5P07V901EHDK9Y1AF9C","lease":"held"}
{"agent":"tk-d2","host":"borant-2","session":"01M5SXH5QHHXTFCBW88Z1RTGGP","lease":"held"}
```

Both sessions ran from 16:15:40 to 16:16:47 and their turns interleave, so
both were making model requests on member `a` at the same time:

```console
$ cat /tmp/tf/tk-d1.log     $ cat /tmp/tf/tk-d2.log
16:15:40 start              16:15:40 start
16:15:47                    16:15:43
16:15:55                    16:15:51
turn-2                      turn-2
16:15:59                    16:15:53
16:16:07                    16:16:01
turn-3                      turn-3
16:16:13                    16:16:04
16:16:21                    16:16:12
turn-4                      turn-4
…                           …
16:17:07 done               16:16:46 done
```

No session was lost, no turn failed, neither log shows an authentication or
limit error, and `~/.claude.json` stayed valid JSON through both
(`jq` parsed it afterwards, mtime 16:17:44). Two concurrent sessions on one
member work.

## 2. A member switch for a running agent

The switch was issued while tk-d1 was mid-run:

```console
$ borant move tk-d1 --member b          # 16:16:09, during turn 3
{ "moving": [ "tk-d1" ] }
```

The move did not interrupt the live turn: tk-d1 ran turns 4 to 7 and answered
`DONE` at 16:17:09. Then it landed at the turn end:

```console
$ borant logs tk-d1
16:17:09 assistant_message  DONE
16:17:09 session_end        planned move
16:17:09 move               borant-2 -> borant-2
16:17:11 session_start      /home/chris/.borant/work/tk-d1
16:17:11 secrets_issued
16:17:11 user_prompt        You continue Borant agent tk-d1 on host borant-2. Read your history with the session_history tool, …
```

```console
# journalctl -u borant-host
INFO borant_host::sessions: planned move: session stopped agent=tk-d1
INFO borant_host::commands: launched agent=tk-d1 harness=claude-code cwd=/home/chris/.borant/work/tk-d1 o.reason=Planned
INFO borant_host::sessions: session attached session=01M5SXM1X7RNHWF8D57WZJRWDE agent=tk-d1

$ jq -c '.[] | select(.agent_hint=="tk-d1") | {members}' ~/.borant/sessions.json
{"members":{"claude-auth":"b"}}
```

So the relaunch works with the new member's token in the same `~/.claude`:
new session id, member `b`, a fresh secret directory (`secrets_issued`), and
the new session made its own model turns (it read `session_history`, listed
channels and tasks, read `/tmp/tf/tk-d1.log`, and answered). The native
context is not resumed: a new Claude session starts with a hand-off prompt
and reads its history, as the design expects.

One caveat I cannot settle without reading credentials: borant-2 keeps a
stored login in `~/.claude/.credentials.json` (mtime today 05:06, unchanged
by the two runs), so "the session answered" does not prove that
`CLAUDE_CODE_OAUTH_TOKEN` and not the stored login served it. The unchanged
mtime of that file and of the `oauthAccount` object in `~/.claude.json`
(sha256 equal to the 16:05 backup) are consistent with the env token being
used, and nothing contradicts it, but only a credential read would prove it.

## 3. First run, and the watchdog

Three screens stood between a pool launch and a first turn on borant-2:

1. Theme picker (`Let's get started. Choose the text style that looks best
   with your terminal`), when `hasCompletedOnboarding` is absent. Seen on a
   copy of the box's `~/.claude.json`.
2. Folder trust. The trust on the parent `/home/chris/.borant/work` covers
   the agent directory, so this one no longer appeared once seeded.
3. Bypass permissions, because the harness runs with
   `--dangerously-skip-permissions`:

   ```console
   WARNING: Claude Code running in Bypass Permissions mode
   In Bypass Permissions mode, Claude Code will not ask for your approval
     before running potentially dangerous commands.
   ❯ No, exit
     Yes, I accept
   Enter to confirm · Esc to cancel
   ```

   Seen on the pool launches tk-b2, tk-c1, and tk-c2 at 16:07 to 16:14.

A launch that parks on such a screen stops there and never registers. The
watchdog does stop it:

```console
# journalctl -u borant-host (borant-2)
16:08:35 WARN borant_host: a launch did not register in time, stopping it agent=tk-a2 elapsed=90s
```

…but it does not report the screen. The hub logged an empty one, twice:

```console
# journalctl -u borant-hub (borant-1)
16:08:35 WARN borant_hub::moves: launch failed agent=tk-a2 host=borant-2 screen=
16:09:20 WARN borant_hub::moves: launch failed agent=tk-b2 host=borant-2 screen=
```

Cause: the tmux launcher builds its target as `-t =<session>`
(`crates/borant-launch/src/tmux.rs:35`), and tmux 3.7b rejects that form for
`capture-pane` (`tmux.rs:95`), while it accepts it for `has-session`:

```console
$ tmux -V
tmux 3.7b
$ tmux has-session -t =borant-tk-d1; echo $?
0
$ tmux capture-pane -p -S -40 -t =borant-tk-d1
can't find pane: =borant-tk-d1
$ tmux capture-pane -p -S -40 -t =borant-tk-d1:
❯
────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle)
```

So the 40 lines that `check_launch_timeouts` captures
(`crates/borant-host/src/lib.rs:837-876`) are always empty on this tmux, the
redacted screen that the hub logs and the `LaunchFailed` message carry
(`crates/borant-hub/src/moves.rs:786-790`) hold nothing, and a person cannot
see which first-run screen stopped the launch. `-t =<name>:` or the plain
name fixes it.

Two more things this showed:

- A stuck or failed launch on borant-2 is re-placed on another host: the hub
  clears the forced host after a failure
  (`crates/borant-hub/src/spawns.rs:505-513`). tk-a2, tk-b2, and the first
  tk-a1 all moved to borant-1, which has no `[secrets]` section and so ran
  Claude Code on the box login. `--host borant-2` alone does not keep a
  failure from hiding; the check is the host and `members` of the new
  session.
- A failed launch is reported at once as `launch failed` (the secret
  directory case), not by the watchdog; the watchdog only fires when the
  process lives but the session never registers.

## 4. `StopFailure` input at a real limit

Skipped, as the task says. Forcing a 5-hour or weekly limit would spend a
real account's window for a hook input that the design already lists as
documented but unproven (`2026-10-25-token-pools-design.md`, 4.3).

## Environment notes

- `borant 0.12.0`, `claude 2.1.310`, `tmux 3.7b`, all on borant-2's
  `chris` profile.
- The host daemon's environment holds no `ANTHROPIC_*` variable, so no stray
  API key hides the pool (design 2.1).
- `borant doctor` from a plain shell fails `plugin.secrets.op` ("the store
  names the credential \"store-token\" but there is no credentials
  directory"). That is the check: the unit passes the credential with
  `LoadCredential=store-token:/var/lib/borant-secrets/op-store-token`. The
  store answered for `borant members` and for every launch here.

## Scratch state left behind

- Channel `tflab` (the launches) and channel `tokfacts-lab`, which holds the
  probe task 01M5SWBT518S3ZEZZYXDT5HFHB.
- Agent records `tk-a1`, `tk-a2`, `tk-b2`, `tk-c1`, `tk-c2`, `tk-d1`,
  `tk-d2`: all cancelled, all `released`, no session and no tmux session on
  borant-2 or borant-1.
