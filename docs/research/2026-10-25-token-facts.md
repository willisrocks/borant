# Token pool facts on borant-2

Task 01M5SD9DMXREX5F5Y49Z56HGDM. Input for the phase 0 facts of the token
pools design (`docs/superpowers/specs/2026-10-25-token-pools-design.md`,
section 5). Measured on borant-2 as the `chris` user. No token value is read,
printed, or reproduced here; members are named only.

## Result

| Fact | Result |
|---|---|
| 1. Two Claude Code sessions on one member at the same time | No result. No pool session can start on borant-2 (blocker below) |
| 2. `borant move <agent> --member b` relaunches with the new token | No result. Same blocker |
| 3. First run on borant-2: theme and trust screens | The box is not onboarded, and the theme screen comes first. A pool launch on borant-2 stops earlier, at the secret directory, so no launch reached the screen. The watchdog is built to report the screen; not observed |
| 4. `StopFailure` input at a real limit | Skipped as instructed. The limit cannot be forced without burning a real account's window |

## Setup, observed on 2026-10-25

- `borant 0.12.0`, `claude 2.1.310`, both from the `chris` profile.
- `~/.borant/host.toml` of borant-2: `[secrets.bundles.claude-auth]` with
  members `a`, `b`, `c`; `[secrets.harness] claude-code = ["claude-auth"]`;
  store `op`, an exec plugin (`borant-secrets-1password`), credential
  `store-token`.
- The harness command in `[commands]`:
  `claude --dangerously-skip-permissions --plugin-dir
  /nix/store/…-borant-0.12.0/share/borant/claude-code`.
- `borant members`:

  ```console
  $ borant members
  claude-auth
    a            free                                           0 waiting   restarts (7 d): 0
    b            free                                           0 waiting   restarts (7 d): 0
    c            free                                           0 waiting   restarts (7 d): 0
  ```

- `host.toml` has no `[timing]` section, so the launch watchdog uses its
  default of 90 s (`crates/borant-host/src/lib.rs:116`).
- `borant doctor` run from a plain shell fails one check:

  ```console
  fail plugin.secrets.op   Invalid: the store names the credential "store-token"
                           but there is no credentials directory
  ```

  That is the check, not the store: the unit passes the credential with
  `LoadCredential=store-token:/var/lib/borant-secrets/op-store-token`
  (`systemctl cat borant-host`), and a shell has no credentials directory.
  The store itself answered for `borant members`.

## Blocker: the host cannot create `/run/borant/agents`

A pool launch fails before the harness starts. The only launch I made:

```console
$ borant spawn --channel tflab --prompt-file p-tk-a1.md \
    --harness claude-code --host borant-2 --name tk-a1 --json
{ "agent": { "agent": "tk-a1", "placed": true } }

# journalctl -u borant-host, two seconds later
15:56:20 WARN borant_host::commands: launch failed: Other:
         cannot create "/run/borant/agents": Permission denied (os error 13) agent=tk-a1
```

- `/run/borant` does not exist, and `/run` is `root:root 0755`, so `chris`
  cannot create it:

  ```console
  $ ls -ld /run/borant
  ls: cannot access '/run/borant': No such file or directory
  $ systemctl show borant-host -p RuntimeDirectory
  RuntimeDirectory=
  ```

- The code creates the directory on the `/run` tmpfs
  (`crates/borant-host/src/secrets.rs:341`: `/run/borant/agents`) and fails
  the whole launch when it cannot (`secrets.rs:114`).
- The secrets doc prescribes the unit setting
  (`docs/superpowers/specs/2026-10-24-secrets-and-host-bootstrap-design.md:450`):

  ```nix
  RuntimeDirectory = "borant/agents";
  RuntimeDirectoryMode = "0700";
  ```

- The module that builds this box does not set it. In
  `~/borant-infra-next/modules/borant.nix` (same in `~/borant-infra`), the
  directories come from `systemd.tmpfiles.rules` (line 189) and the service
  config carries `LoadCredential` (line 213), but there is no
  `RuntimeDirectory` and no tmpfiles rule for `/run/borant/agents`.
- borant-1 is in the same state: `/run/borant` is absent and
  `RuntimeDirectory` is empty. It launches Claude Code only because its
  `host.toml` has no `[secrets]` section at all, so no bundle is resolved.

Consequence: on borant-2, any harness whose launch resolves a bundle cannot
start — which is every `claude-code` launch there, because
`[secrets.harness]` gives `claude-auth` to that harness.

## The fallback hides the failure

`--host borant-2` is not sticky after a launch failure. The hub clears the
forced host and places the agent on any host
(`crates/borant-hub/src/spawns.rs:505-513`), so my failed pool launch became a
session on borant-1:

```console
$ borant logs tk-a1
15:56:18 move               - -> borant-2
15:56:21 move               - -> borant-1

# journalctl -u borant-host on borant-1
15:56:21 INFO borant_host::commands: launched agent=tk-a1 harness=claude-code
         cwd=/home/chris/.borant/work/tk-a1 o.reason=Spawn
```

That session is not a pool session: borant-1 has no `[secrets]` section, so
its Claude Code used the box's stored login instead of a member's token. A
broken pool member or a broken secret directory thus turns into a silent
non-pool session elsewhere, as long as another host takes the agent. The agent
was cancelled; no `tk-a1` tmux session or process is left on borant-1.

## 1. Two Claude Code sessions on one member at the same time

Not measured. No pool session starts on borant-2 (blocker above), and the
task allows test agents only there.

What the code says, for the record: with no fresh usage reports, `pick` takes
the first free member in name order
(`crates/borant-core/src/rules/members.rs:98`), so two launches at once do get
the same member `a`. The case is the normal one, not a corner.

## 2. `borant move <agent> --member b`

Not measured, for the same reason. Two code facts set the path it will take
when the blocker is gone:

- A member switch validates the member against a bundle that the agent
  already uses (`crates/borant-hub/src/moves.rs:546-557`); an agent with no
  member bundle is refused with `agent {agent} has no member bundle to
  switch` (`moves.rs:482`).
- The relaunch resolves the bundles again through the same `issue_secrets`
  (`crates/borant-host/src/secrets.rs:114`), so on borant-2 as it is today a
  switch fails with the same `cannot create "/run/borant/agents"` error.
  Unobserved, because no agent on borant-2 can hold a member.

## 3. First run on borant-2

The box is not onboarded. `~/.claude.json` has no `hasCompletedOnboarding`,
no `theme`, and no `projects` entry (the file's mtime stayed
`2026-10-25 10:08:37`; I changed nothing in it). It does hold
`oauthAccount` and `~/.claude/.credentials.json`, so a login exists without
onboarding.

The theme screen is real. I ran the same binary, with the host's own argv and
`HOME` pointed at a temporary directory that held a byte copy of
`~/.claude.json` (`/tmp/tfhome/.claude.json`, deleted afterwards):

```console
$ env HOME=/tmp/tfhome claude --dangerously-skip-permissions \
    --plugin-dir /nix/store/…-borant-0.12.0/share/borant/claude-code
 Let's get started.
 Choose the text style that looks best with your terminal
 To change this later, run /theme
     Auto (match terminal)
 ❯ ✔ Dark mode
     Light mode
     …
```

After Enter it asked for a login method, because that temporary home had no
credentials. On the real box the stored credentials skip that screen and the
folder-trust prompt follows
(`crates/borant-host/src/lib.rs:833-837` names "a folder-trust prompt" as a
screen that never registers).

- Does a pool launch stop there? Today it does not get there: it dies at
  `/run/borant/agents` first. Once that is fixed, it will stop at the theme
  screen on borant-2, which never registers the session.
- Does the launch watchdog report the screen? By construction, yes: after
  `launch_timeout` (90 s here) it stops the launch and sends the hub the last
  40 lines of the screen, redacted (`lib.rs:837-876`, `WATCHDOG_LINES` at
  `lib.rs:883`). Not observed, because no launch on borant-2 reaches a
  screen — the secret failure is reported at once as `launch failed`, not by
  the watchdog.

What the orchestrator should seed in `~/.claude.json`: the shape that works
on borant-1, which has completed onboarding:

```console
$ jq -c '{hasCompletedOnboarding, theme, trust: ((.projects // {})
  | to_entries | map({dir: .key, trust: .value.hasTrustDialogAccepted}))}' ~/.claude.json   # borant-1
{"hasCompletedOnboarding":true,"theme":null,
 "trust":[{"dir":"/home/chris/borant-test","trust":true},
          {"dir":"/home/chris/work/owner1","trust":true}]}
```

So: `hasCompletedOnboarding: true` (it alone skips the theme screen;
`theme` is null on the onboarded box), and a trust entry with
`hasTrustDialogAccepted: true` for the folder that agent clones live under,
`/home/chris/.borant/work`. I did not verify that Claude Code accepts a
parent folder instead of the exact project directory; borant-1's entries name
the exact clone directories.

## 4. `StopFailure` input at a real limit

Skipped, as the task says. Forcing a 5-hour or weekly limit would spend a
real account's window for a hook input that the design already lists as
documented but unproven (`2026-10-25-token-pools-design.md:4.3`).

## What a phase 0 rerun needs

1. `RuntimeDirectory = "borant/agents"` with mode `0700` on
   `systemd.services.borant-host` in `borant-infra` (or an equivalent
   tmpfiles rule for `/run/borant/agents`), on every box that has `[secrets]`,
   then a switch of that unit.
2. `hasCompletedOnboarding` and the folder trust seeded in `~/.claude.json`
   before the first pool launch, or the first launch parks on the theme
   screen for 90 s and then fails.
3. Then facts 1 and 2: two `claude-code` launches on borant-2 (both get
   member `a`), then `borant move <agent> --member b`.

## Scratch state left behind

- Channel `tflab` (created for the launches) and channel `tokfacts-lab`, which
  holds the probe task 01M5SWBT518S3ZEZZYXDT5HFHB "probe: do the CLI and the
  MCP see one store?" (a typo in a channel name, kept as the record of the
  run).
- Agent record `tk-a1`, released, no session and no process.
