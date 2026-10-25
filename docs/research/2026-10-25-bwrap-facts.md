# bubblewrap sandbox facts on borant-2

Task 01M5S5XBM34S8J4KDCHK4Q4DK2. Input for the phase 5 spec, section 6
(Sandboxes). Measured on borant-2 as the `chris` user, in the w-warn clone.
No secret value is reproduced here; credential files are named by path only.

- borant-2: NixOS 26.05.20261021.da6dc0e (Yarara), kernel 6.18.57.
- `bubblewrap 0.12.0` from `nixpkgs#bubblewrap`
  (`/nix/store/nnnh2ks97pxsv3cswhmvir8vzi0gc63m-bubblewrap-0.12.0/bin/bwrap`).
- Harnesses: `omp v18.12.6`, `claude` 2.1.310. Both are Bun-built generic
  Linux ELF binaries (`interpreter /lib64/ld-linux-x86-64.so.2`).

The tests used this argv, with `$HOME=/home/chris`:

```sh
bwrap --die-with-parent --unshare-ipc --new-session \
  --ro-bind /nix/store /nix/store \
  --ro-bind /run/current-system /run/current-system \
  --ro-bind /lib64 /lib64 \
  --ro-bind /etc/resolv.conf /etc/resolv.conf \
  --ro-bind /etc/ssl /etc/ssl \
  --proc /proc --dev /dev --tmpfs /tmp --tmpfs "$HOME" \
  --bind "$HOME/.omp" "$HOME/.omp" \
  --ro-bind "$HOME/.pi/agent/models-store.json" "$HOME/.pi/agent/models-store.json" \
  --ro-bind "$HOME/.local" "$HOME/.local" \
  --bind "$HOME/.claude" "$HOME/.claude" \
  --bind "$HOME/.claude.json" "$HOME/.claude.json" \
  --bind "$HOME/.borant/work/w-warn" "$HOME/.borant/work/w-warn" \
  --bind "$HOME/.borant/host.sock" "$HOME/.borant/host.sock" \
  --chdir "$HOME/.borant/work/w-warn/borant" -- <program>
```

Each harness was started with the Borant variables unset
(`env -u BORANT_AGENT -u BORANT_CHANNEL -u BORANT_HOME -u BORANT_SOCKET
-u BORANT_ROLE -u BORANT_COLLECTION -u PI_SESSION_ID -u PI_SESSION_FILE
-u CLAUDECODE`), so no test run registered as another session.

## 1. Unprivileged user namespaces

```console
$ uname -r
6.18.57
$ nixos-version
26.05.20261021.da6dc0e (Yarara)
$ sysctl user.max_user_namespaces
user.max_user_namespaces = 30880
$ bwrap --ro-bind / / true; echo $?
0
```

`bwrap --ro-bind / / true` works as the normal user. No setuid bit, no
`kernel.unprivileged_userns_clone` override.

## 2. omp and claude inside the sandbox

Both answer. Each needed binds beyond the spec's argv.

Extra binds, in the order they were found:

| Harness | Extra bind | Why, and the failure without it |
|---|---|---|
| both | `--ro-bind /lib64 /lib64` | The harnesses are generic ELF binaries whose interpreter is `/lib64/ld-linux-x86-64.so.2`. On NixOS that is a symlink to `nix-ld`. Without the bind `bwrap: execvp omp: No such file or directory` (ENOENT from the missing interpreter). |
| omp | `--ro-bind ~/.local ~/.local` | `omp` is `~/.local/bin/omp`. |
| omp | `--bind ~/.omp ~/.omp` | Reads and writes `~/.omp/agent/{agent.db,sessions,config.yml,logs}` and `~/.omp/plugins`. Read-write, because omp writes the session and log. |
| omp | `--ro-bind ~/.pi/agent/models-store.json ~/.pi/agent/models-store.json` | The Lunaroute provider is a Pi extension; the model catalog lives in this JSON. Without it: `No models available. Set API keys in environment variables.` and `omp models` lists nothing. Binding all of `~/.pi` also works; the single file is enough and holds no credential (it is a model catalog, keys are `provider`/`model`/`max_tokens`). |
| claude | `--ro-bind ~/.local ~/.local` | `claude` is `~/.local/bin/claude` -> `~/.local/share/claude/versions/2.1.310`. |
| claude | `--bind ~/.claude ~/.claude` | `~/.claude` holds `.credentials.json`, settings, sessions, and projects. Read-write, because claude writes sessions. |
| claude | `--bind ~/.claude.json ~/.claude.json` | The file holds the per-project state claude writes at startup. |
| both | `--bind ~/.borant/work/<agent> ~/.borant/work/<agent>` | The clone of the agent (read-write). |
| both | `--bind ~/.borant/host.sock ~/.borant/host.sock` | The host socket, for the MCP server, hooks, and the omp extension. |

Runs:

```console
$ ... bwrap ... -- omp -p "reply with OK"
Working...
OK                                    # 8.8 s
$ ... bwrap ... -- claude -p "reply with OK"
OK                                    # 8.7 s
```

Not needed here: `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf`,
`/etc/hosts`. Without them `id` prints bare numbers and both harnesses still
work. `/etc/ssl` and `/etc/resolv.conf` were kept from the spec. DNS resolves
(no `--unshare-net`), and 127.0.0.1 is the host's, so a sandbox reaches host
services by IP.

### Socket and MCP inside the sandbox

With the socket bound, the CLI and the MCP server reach the host daemon from
inside:

```console
$ ... bwrap ... --bind ~/.borant/host.sock ... -- borant status | head -4
Local host: borant-2

Hosts:
  borant-1             online=true launcher=tmux     draining=false ...
```

Without the bind:

```console
$ ... bwrap ... -- borant status
borant: the Borant host daemon is not running: connect to /home/chris/.borant/host.sock
```

The MCP server, with newline-delimited JSON on stdin (`initialize`,
`notifications/initialized`, then a call), answers a real call:

```console
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18",...,"serverInfo":{"name":"rmcp","version":"3.7.0"}}}
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"[{\"name\":\"borant\",...}]"}]}}
```

(`tools/call project_list`; `tools/list` advertises `channel_*`, `members`,
`project_*`, `schema_*`, `send`, `session_history`, `spawn`, `task_*`,
`whoami`.)

The omp extension resolves the same socket: `BORANT_SOCKET`, else
`$HOME/.borant/host.sock` (`~/.omp/agent/extensions/borant.ts:144-145`). With
the socket bound and the default path it therefore works. The extension also
reads `BORANT_AGENT`, `BORANT_CHANNEL`, `BORANT_ROLE`, and `BORANT_COLLECTION`;
a launch that must not create a session leaves them unset.

## 3. What the sandbox hides

With the binds above, inside the sandbox:

```console
$ ls -A $HOME
.borant  .local  .omp          # plus .claude and .pi files when bound
$ ls -A $HOME/.borant
work
$ ls -A $HOME/.borant/work
w-warn                          # only this agent's clone
$ ls -A $HOME/.ssh
ls: cannot access '/home/chris/.ssh': No such file or directory
$ ls -A /tmp
                                # empty: the tmux socket (/tmp/tmux-1000/default) is gone
$ ls -l $HOME/.borant/host.creds
ls: cannot access '.../host.creds': No such file or directory
```

- `~/.ssh` (both keys) is invisible.
- The other eight clones in `~/.borant/work` (`b1`, `helper`, `w-add`,
  `w-events`, `w-fill`, `w-mirror`, `w-stores`, `w2`) are invisible.
- The tmux socket is invisible, because `--tmpfs /tmp` replaces `/tmp`.
- `host.creds` does not exist on borant-2; the NATS credentials are inside
  `host.toml`, which is a `/nix/store` symlink and is not bound, so it is
  invisible too. No NATS env file is in the harness environment.
- `~/.config/gh` is invisible unless it is bound (the spec's example of an
  extra read-only path).

## 4. `--unshare-pid` and the reported PID

Without `--unshare-pid` the PID is the real one:

```console
$ ... bwrap ... -- sh -c 'echo inside-pid=$$; exec sleep 3.14159' & sleep 0.4
inside-pid=473090
$ ps -eo pid=,args= | grep '[s]leep 3.14159'
473090 sleep 3.14159
```

With `--unshare-pid` it is not:

```console
inside-pid=2
473403 sleep 3.14160        # the outside PID
```

So the spec's choice to omit `--unshare-pid` is required for the hook PID:
a PID namespace renumbers the harness to 1 or 2 and the host's liveness
check would look at the wrong process.

## 5. Proxy honoring, outside any sandbox

Logging CONNECT proxy:

```console
$ uv run --with proxy.py -- proxy --hostname 127.0.0.1 --port 3128 \
    --num-workers 1 --log-level INFO --log-file /tmp/p5-proxy.log
$ env -u BORANT_AGENT ... HTTPS_PROXY=http://127.0.0.1:3128 \
    HTTP_PROXY=http://127.0.0.1:3128 claude -p "reply with OK"
OK
$ env -u BORANT_AGENT ... HTTPS_PROXY=http://127.0.0.1:3128 \
    HTTP_PROXY=http://127.0.0.1:3128 omp -p "reply with OK"
OK
$ HTTPS_PROXY=http://127.0.0.1:3128 git clone --depth 1 \
    https://github.com/willisrocks/borant /tmp/p5-git-test
```

CONNECT lines in `/tmp/p5-proxy.log`:

```text
14 CONNECT api.anthropic.com:443          # claude
 1 CONNECT mcp-proxy.anthropic.com:443    # claude
 1 CONNECT http-intake.logs.us5.datadoghq.com:443   # claude telemetry
 2 CONNECT gw.lunaroute.com:443           # omp model calls
 1 CONNECT mcp.lunaroute.com:443          # omp
 1 CONNECT zenmux.ai:443                  # omp/Pi model catalog
 1 CONNECT hyper.charm.land:443
 1 CONNECT coding-intl.dashscope.aliyuncs.com:443
 1 CONNECT catalog.stencil.so:443
 1 CONNECT api.venice.ai:443
 1 CONNECT api.kilo.ai:443
 1 CONNECT api.commandcode.ai:443
 1 CONNECT github.com:443                 # git clone
```

- Both harnesses honor `HTTPS_PROXY`/`HTTP_PROXY` with no
  `NODE_USE_ENV_PROXY`: they are Bun binaries, and Bun reads the proxy
  variables itself. (Node's built-in `fetch` from 24 needs
  `NODE_USE_ENV_PROXY=1`; no Node is installed here, so this is untested.)
- `git` over HTTPS honors the variables (`github.com` appears once).
- Egress allowlist for phase two must cover more than the LLM API: the omp
  Pi extension also calls a model catalog and provider health endpoints
  (`zenmux.ai`, `hyper.charm.land`, `catalog.stencil.so`, `api.venice.ai`,
  `api.kilo.ai`, `api.commandcode.ai`, `mcp.lunaroute.com`), and claude
  sends telemetry to `http-intake.logs.us5.datadoghq.com` unless disabled.
  An allowlist with only `api.anthropic.com`/`gw.lunaroute.com` breaks both
  harnesses.

## 6. Startup cost

`bwrap` itself:

```console
$ ... bwrap ... -- sh -c true     # 6.9, 9.6, 11.3 ms
$ sh -c true                      # 4.0 ms
```

`claude -p "reply with OK"`, three samples each:

| Outside | Inside bwrap |
|---|---|
| 3.54 s | 8.67 s |
| 8.15 s | 5.41 s |
| 3.81 s | 3.91 s |

The spread is API round-trip time, not the sandbox: bwrap adds 3 to 7 ms and
the start-up work of the harness is unchanged. `omp -p` took 8.8 s inside.

## Findings for the spec

1. Add `--ro-bind /lib64 /lib64` to the argv. Without it neither harness
   starts, because both are non-Nix binaries that use `nix-ld`.
2. The harness directories are harness-specific and read-write:
   `~/.omp` (omp) and `~/.claude` plus the `~/.claude.json` file (claude).
   The binaries need a read-only bind too (`~/.local` here).
3. omp needs the Pi model catalog `~/.pi/agent/models-store.json` read-only;
   without it the Lunaroute provider is absent, not merely unauthenticated.
4. The socket bind is what gives MCP, hooks, and the omp extension access.
   Unset `BORANT_*` and `PI_SESSION_*` for a harness that must not register.
5. No `--unshare-pid`, confirmed: the reported PID is the host PID without
   it and 2 with it.
6. The egress allowlist must include the Pi extension's catalog hosts and
   claude's telemetry host, not only the model APIs.
7. Credentials that stay inside the sandbox by design: `~/.claude/.credentials.json`
   and `~/.omp/agent/agent.db` are the harness's own logins. They are
   readable by the agent inside the sandbox, and `~/.pi/agent/models-store.json`
   holds no credential. Anything the agent must not read (`~/.ssh`,
   `host.toml` with the NATS credentials, other clones, tmux) is hidden by
   omission, as long as the host configuration does not bind it.
