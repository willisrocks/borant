# herdr facts on borant-1, 2026-10-25

Research for the phase 5 spec ("contain the agents", section 7 of
`2026-10-24-orchestration-api-and-sandboxes-design.md`). All runs were as user
`chris` on borant-1, with the pinned release binary at
`./.tools/dl/herdr` (removed after the tests) and a herdr session named
`p5test`. Nothing system-wide was changed; the omp integration was installed
and then uninstalled.

## 1. Version, protocol, nixpkgs, license

```
$ ./.tools/dl/herdr --version
herdr 0.9.5
$ ./.tools/dl/herdr api schema --json | jq .protocol
22
```

- Latest release: `v0.9.5`, published 2026-10-14 (GitHub
  `herdrdev/herdr`); license **Apache-2.0** (`spdx_id` from the repo API).
  Protocol **22**, which matches what the spec assumed.
- nixpkgs: `pkgs/by-name/he/herdr/package.nix` exists on `nixos-unstable`
  with `version = "0.9.5"` (built with `rustPlatform.buildRustPackage`, a
  vendored `libghostty-vt` from zig). On borant-1 the pinned nixpkgs does not
  have it yet:

  ```
  $ nix eval --raw nixpkgs#herdr.version
  error: flake 'flake:nixpkgs' does not provide attribute ... 'herdr.version'
         Did you mean one of feedr, herbe, herqq, hexd or serd?
  ```

  So a NixOS pin of herdr needs a newer nixpkgs (or an overlay from the repo).
- **Naming trap:** crates.io has an unrelated `herdr 0.1.0`
  (`ogulcancelik/herdr`, AGPL-3.0-or-later, "terminal workspace manager for AI
  coding agents"), so `cargo install herdr` installs the wrong project. The
  real one ships as GitHub release binaries (`herdr-linux-x86_64`, 30 MB).

## 2. Headless server

`herdr server` is the documented headless mode ("Run as headless server" in
`herdr --help`). The API subcommands do **not** start it: without a server,
`workspace create` fails with `server_not_running ... run \`herdr session
attach p5test\`` (that command is the interactive client).

```
$ setsid env -u BORANT_AGENT -u BORANT_CHANNEL -u BORANT_HOME -u BORANT_SOCKET \
    -u PI_SESSION_FILE ./.tools/dl/herdr --session p5test server &
$ ps -o pid,sess,tty,args -p <pid>
 707725 707725 ?  .../herdr --session p5test server
```

- No TTY is needed: the server runs with `tty=?` from a non-interactive shell
  and serves the socket API; panes still get their own PTYs.
- Per session it writes `~/.config/herdr/sessions/<name>/herdr.sock` (API),
  `herdr-client.sock` (TUI), and `herdr-server.log`, and prints
  `herdr server running; you can use any herdr CLI command in another terminal`.
- It survives the shell that started it **only when detached from that
  shell's process group** (`setsid`, or systemd). With plain `nohup`, a
  signal to the group (what a task runner sends when it kills the command)
  took the server with it. `nohup` alone is not enough.
- Session state persists on disk across a server restart: after a restart,
  `workspace list` still showed the earlier workspaces (panes and their
  processes were gone), so a restarted server needs a reconciliation pass
  (`pane list`, `agent list`).

## 3. Launcher mapping, as the spec writes it

| Port call | Spec command | Result today |
|---|---|---|
| `launch` | `herdr --session borant workspace create --cwd <cwd> --label borant-<agent> --env K=V ... --no-focus` | **works**; returns `root_pane.pane_id` (`w1:p1`), the workspace id (`w1`), and the tab id. `--env` only *adds* variables; it cannot remove one |
| | `herdr agent start <name> --kind claude --pane <root_pane> -- <args>` | **works** (`--kind` takes `omp`, `claude`, …24 kinds; `--timeout` ms, default 30000, max 300000; `[-- AGENT_ARG]...` passes args). The pane must be a pristine herdr shell: a pane where a nested `bash`/`omp` runs answers `agent_pane_busy: agent target pane w1:p1 is not an available shell` |
| | `herdr pane run <pane> exec <wrapped argv>` | **works** as `pane run <PANE_ID> <COMMAND>...`, e.g. `pane run w5:p1 sh -c 'echo P5-RUN-OK; env \| grep ^P5MARK'` printed `P5MARK=yes`. This is the path for a wrapped or env-scrubbed launch |
| `stop` | `herdr workspace close <workspace>` | **works**; `{"type":"ok"}`, and the pane processes die with it |
| `is_running` | `herdr agent get <name>` | **works** (`agent_list`/`agent_info` with `agent_status`, `state_change_seq`); `pane list` shows live panes |
| `attach` | `herdr --remote <ssh> --session borant` | documented, see 6 |
| `state` | `events.subscribe` for `pane.agent_status_changed` | the name is correct: `herdr api schema --json` lists `events.subscribe`, `events.wait`, and `pane.agent_status_changed` |

Additions found while testing: `herdr agent wait <TARGET> --until <state>` and
`herdr agent explain <TARGET>` (manifest, rule, and the screen evidence for
the current state), `herdr agent prompt <TARGET> <TEXT> [--wait]`.

## 4. Agent states

`agent start` returns when the agent is at its prompt: for omp it answered in
3.1 s with `{"agent":"omp","agent_status":"idle","interactive_ready":true,
"argv":["omp"]}`. A turn, sampled with `agent get` every 200 ms while
`agent prompt ... --wait` ran (integration installed, see below):

```
idle   seq=1
working seq=2   (+4.5 s)
idle   seq=3    (+8.6 s)
```

and the pane then showed the answer (`pane read --source recent` → `OK`). The
prompt reply carries `agent_session: {kind: "path", value:
/home/chris/.omp/agent/sessions/...}`.

Without the omp integration the state stayed `idle` and
`agent prompt` failed: `agent_prompt_stalled: agent prompt produced no observed
working or blocked state within 5000 ms; current status is idle`. So **omp
needs the integration for `working`/`done`**; the screen reader alone gives
only `idle` for its TUI.

```
$ ./.tools/dl/herdr integration install omp
installed omp integration to /home/chris/.omp/agent/extensions/herdr-omp-agent-state.ts
$ ./.tools/dl/herdr integration status | grep '^omp'
omp: current (v10) (/home/chris/.omp/agent/extensions/herdr-omp-agent-state.ts)
```

It writes **one file**, `<agent home>/…` per agent: omp
`~/.omp/agent/extensions/herdr-omp-agent-state.ts` (12.7 kB, v10, managed by
herdr); claude `~/.claude/hooks/herdr-agent-state.sh`; pi
`~/.pi/agent/extensions/herdr-agent-state.ts`; codex `~/.codex/…`; opencode
`~/.config/opencode/plugins/…`; `herdr integration status` lists all 18 paths.
The omp extension is a Node/TS module that connects to `$HERDR_SOCKET_PATH`
with `$HERDR_PANE_ID` and reports the session as `source: "herdr:omp"`; herdr
sets those variables in pane processes. `herdr integration uninstall omp`
removes the file.

With the integration, `agent explain` says `manifest: none unknown`, `rule:
none`, `screen_detection_skip_reason: full_lifecycle_hook_authority` (hooks
author the state); for claude, which was not integrated, it used the remote
manifest `claude.toml 2026.10.11.1` and the rule `live_blocked_form`. That is
how claude showed `blocked` on its first-run trust screen:

```
$ herdr --session p5test agent start p5claude --kind claude --pane <pane>
$ herdr --session p5test agent get p5claude
{"name":"p5claude","agent":"claude","agent_status":"blocked", ...}
```

`done` exists in `--until`/`--state` and in the schema, but omp went back to
`idle` after the turn, so `done` was not observed for it.

## 5. Agent names

```
$ herdr agent start BadName --kind omp --pane <pane>
{"error":{"code":"invalid_agent_name","message":"agent name must start with a
lowercase letter and contain only lowercase letters, digits, '-' or '_'
(1-32 characters)"}}
```

A 38-character name gives the same error. So the rule is
`[a-z][a-z0-9_-]{0,31}`, exactly as the spec says; the adapter must shorten
and hash long agent IDs.

## 6. Remote attach

From the docs (`v0.9.5/docs/next/website/src/content/docs/persistence-remote.mdx`),
not tested here because there is no laptop on this host:

```
herdr --remote workbox                 # an SSH config host alias
herdr --remote ssh://you@server:2222   # or a URL
herdr --remote workbox --session agents
herdr --remote workbox --handoff       # live handoff
HERDR_REMOTE_BINARY=target/release/herdr herdr --remote workbox
```

Keybindings come from the local client by default
(`--remote-keybindings server` uses the remote ones); the attach goes through
a temporary SSH config with keepalives unless `[remote].manage_ssh_config =
false`. The client must be able to use the key (ssh-agent when no interactive
prompt is possible).

## 7. Gotchas for the launcher adapter

- **Environment.** Panes inherit the server's environment. An omp or claude
  started in a pane that carries `BORANT_AGENT`/`BORANT_CHANNEL`/`BORANT_HOME`
  registers with Borant as that agent. Start the herdr server itself with a
  clean environment (`env -u BORANT_AGENT -u BORANT_CHANNEL -u BORANT_HOME -u
  BORANT_SOCKET ... herdr server`), or pass the harness a scrubbed environment
  through `pane run`. `workspace create --env` only sets variables.
  `agent start` needs a *herdr* shell, so scrubbing with a nested
  `pane run … bash` first makes the pane `agent_pane_busy`; scrub the server,
  not the pane.
- **Processes.** Close panes/workspaces by id and signal only the PID that the
  launch recorded. Never `pkill -f <name>` on a host where other agents run.
- **First-run screens** surface as `blocked`, and `agent start` can report
  success while the agent waits at one (claude's trust prompt).
- **Prompt latency.** `agent prompt` requires a `working` or `blocked` state
  within 5 s, so a lazy first token fails as `agent_prompt_stalled`.
- **Idle detection.** Use `agent wait --until idle,working,blocked` with a
  timeout, or subscribe to `pane.agent_status_changed`, instead of polling.
