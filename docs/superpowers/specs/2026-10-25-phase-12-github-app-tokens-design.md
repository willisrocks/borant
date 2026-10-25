# Phase 12: fresh GitHub tokens for agents, from the `borant-swarm` GitHub App

Chris's decision of 2026-10-25 17:10 UTC: GitHub refuses a personal access
token for this work, so agents authenticate as the `borant-swarm` GitHub App
(owner `willisrocks`, created 17:40 UTC), the pattern the steve-bot sandboxes
use. The installation covers all 23 repositories of `willisrocks` until Chris
narrows it, so every token is narrowed at mint time to `borant` and
`borant-infra` with a fixed permission set, and the plugin refuses to mint
without that list. An installation token lasts one hour, so a `GH_TOKEN` at
launch is not enough: git and `gh` in a session ask the host daemon for a
token, which asks a plugin that mints one from the App key in 1Password.
Section 6.3 of the
[secrets doc](2026-10-24-secrets-and-host-bootstrap-design.md) (one PAT in the
`github` bundle) is superseded.

The minting plugin is an exec plugin in `borant-infra`, behind the exec plugin
protocol of the [elastic compute doc](2026-10-24-elastic-compute-design.md)
("Exec plugin protocol"). No GitHub code and no GitHub name enters
`borant-core` (ports and adapters, `AGENTS.md`).

## 1. The port: a secret that a backend mints, with a life

Minting is a second operation on the store that already holds the App key, not
a second port: one plugin program, one `describe`, one config section. The
types go in `crates/borant-core/src/ports/secrets.rs`; they name no backend.

```rust
// ids.rs, via name_type!: safe to log, like BundleName.
pub struct CredentialName(pub String);   // for example `github`, or `ci`

/// `[secrets.credentials.<name>]` of host.toml.
pub struct CredentialSpec { pub name: CredentialName, pub store: String, pub target: SecretRef }

/// One mint. `ttl` is how long the value lives. It is required: the host
/// daemon needs an expiry to cache and to hand out, so a plugin that cannot
/// say how long its value lives is not a minting store.
#[derive(Debug)]
pub struct Minted { pub value: Secret, pub ttl: Duration }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretCapabilities { pub resolve: bool, pub mint: bool }

pub trait SecretStore {
    // ... resolve, unchanged ...
    /// Mints a value from `target`, the opaque ref of the minting material.
    async fn mint(&self, target: &SecretRef) -> PortResult<Minted> {
        Err(PortError::new(PortErrorKind::Invalid, "no mint"))
    }
}
```

Exec protocol: `describe` lists `"mint"` in `capabilities`, and the op carries
the ref, never a name that a plugin parses:

```json
{"v": 1, "op": "mint", "target": "op://Borant/github-app"}
{"ok": {"value": "ghs_...", "ttl_secs": 3540}}
```

The reply uses `ttl_secs` (the remaining life, computed by the plugin from
GitHub's `expires_at` and the plugin's own clock) so clock skew stays inside
one process. Errors follow the existing table: `unavailable` (GitHub 5xx, a
network error, a timeout), `not_found` (the item is gone), `invalid` (a key
that does not parse), `denied` (GitHub 401/403: app revoked, installation
suspended, permission missing), `internal`. A `value` that is absent, or a
`ttl_secs` that is absent or zero, is `internal`. `timeout_secs` for this
store is 20, because one mint is two 1Password reads, one signature, and one
HTTPS POST.

Two existing checks change with it. `open_store` in
`crates/borant-secrets/src/config.rs` today refuses any exec plugin whose
`describe` does not report `resolve: true` (line 88), so a mint-only plugin
would not open: it must accept a store with `resolve` or `mint`, and an op that
the plugin does not report stays `Invalid` at the port. The `env` and `file`
stores keep `resolve: true, mint: false`.

`borant-core::testing` gains `mint_contract(store, target, max_ttl)`, run by
every adapter that reports `mint`: a mint returns a non-empty value and a ttl
at most `max_ttl`, and a second mint may return the same value or a new one;
the `Debug` of the result and of a `PortError` holds no value; a store without
`mint` returns `Invalid`; an unknown target is `NotFound`. Every
`SecretCapabilities` literal in `borant-secrets` and in the host's fake stores
gets the new field.

*Accepted when:* the contract suite passes against a shell fixture plugin
whose `describe` lists `mint` only and whose target is `fixture://app` (so a
mint-only plugin opens), and the new types and the `mint` op name no backend:
the names `github`, `GITHUB`, `ghs_`, and `op://` appear in no line that task 1
adds to `crates/borant-core/src` or `crates/borant-secrets/src` (the redaction
pattern that task 1 edits keeps its `gh[pousr]_` shape, a pattern, not a
backend).

## 2. How a session asks: one host API call, two helper programs

The host daemon caches per credential name and answers a loopback call on its
existing 0600 Unix socket. The hub is not involved; nothing crosses NATS.

```
POST /v1/credential            {"name": "github"}
200 {"ok": {"value": "ghs_...", "expires": 1792952400000}}
400 invalid / 403 denied / 404 not_found / 503 unavailable / 500 internal
```

`expires` is the end of the token (`mint` time + `ttl`), not the earlier
moment the cache stops reusing it; a helper does not need it, and a client may
use it to warn. The reply wraps in `Reply::Ok` like every other route.

`crates/borant-host/src/credential.rs` holds the cache: `BTreeMap<CredentialName,
Cached { value: Arc<Secret>, refresh_at: Instant, expires: Timestamp }`, one
`tokio::sync::Mutex` per name, and a 5 s negative cache. The rules, in order:

1. A hit whose `refresh_at` is still ahead returns the cached `Arc<Secret>`.
2. A miss takes the per-name lock and checks the cache again: a waiter that a
   finished mint already served returns that value, so N concurrent calls make
   one mint.
3. Otherwise the daemon mints, sets `expires` = now + `ttl` (wall clock) and
   `refresh_at` = `Instant::now()` + `ttl` - `min(4 min, ttl / 10)`, and stores
   it. A `ttl` under 60 s is handed out but not stored.
4. A failed mint starts the 5 s negative cache for that name: within 5 s
   another call answers `unavailable` without starting the plugin.
5. `refresh_at` is a monotonic `std::time::Instant`, so a wall-clock jump
   neither pins a token past its end nor forces an early mint; `expires` stays
   a `Timestamp` for the reply. Tests use `tokio::time::pause`, which drives
   `Instant::now()`.

The whole handler, including the wait for the lock, runs under a 20 s
`tokio::time::timeout`, so a waiter queued behind a mint can be answered
`unavailable` instead of a value: that is the honest answer, and the helper's
deadline is later than the daemon's.

The reply type `IssuedCredential` lives in the host crate (not in `proto.rs`),
has `Serialize` on a `String`, and a hand-written `Debug` that prints
`IssuedCredential(..)`; a unit test asserts both. But `Reply::ok` turns it into
a `serde_json::Value`, whose `Debug` shows the whole token: the rule is
therefore "no code path logs a `Reply`, a `Value`, or an error body of this
route", not "the type hides it". `IntoResponse` writes the body and logs
nothing, and no session event carries it. A credential whose `store` is not in
`[secrets.stores]`, or whose store failed to open, logs one `error` at start
and answers 404: a typo in a new host file must not stop a host that otherwise
works.

The daemon puts two things into `LaunchSpec.env` when `[secrets.credentials]`
is not empty: `BORANT_SOCKET` (the socket the daemon serves, so a helper never
guesses `~/.borant/host.sock`) and a `PATH` whose first entry is
`[secrets] helpers = "/run/borant/bin"`. That directory holds two programs
built from this repo, `crates/borant/src/bin/`:

- `borant-git-credential <name>`, a git credential helper. Git appends the
  operation, so the argv is `<name> get|store|erase`; for `get` it reads the
  attributes on stdin, asks the daemon, and prints
  `username=x-access-token` and `password=<value>` plus the blank line. `store`
  and `erase` print nothing and exit 0. On any failure it prints one line to
  stderr and `quit=true` on stdout: per `gitcredentials(7)`, `quit` stops the
  next helper and the prompt, so git fails with that line above it instead of
  asking for a password. Git's config, from `borant-infra` in `/etc/gitconfig`:

  ```
  [credential "https://github.com"]
      helper =            # reset first: an empty value clears the list so far
      helper = "!borant-git-credential github"
  ```

  The order matters: an empty `helper` clears the helpers seen so far, so it
  must come before ours, and `quit=true` still stops any helper a user or repo
  config adds after ours. Without the reset, `gh auth git-credential` (from
  `gh auth setup-git`, today's boxes) would answer with the stored login
  first.

- `borant-gh`, the `gh` wrapper, symlinked as `/run/borant/bin/gh`. It fetches
  the value, sets `GH_TOKEN` in its own environment (`GITHUB_TOKEN` removed,
  `GH_PROMPT_DISABLED=1`), and `exec`s the real binary named by `BORANT_GH`
  (set by `borant-infra` to the pinned Nix path), so no wrapper process stays
  in front. On failure it prints one line to stderr and exits 1.
  `GH_TOKEN` wins over stored credentials (`gh help environment`), and it is
  in the environment of `gh` only, never in the agent's environment, argv, or
  a dump. The promise is narrow: `gh`'s own HTTPS clone and push go through
  `gh auth git-credential`, which reads `GH_TOKEN`, so they reuse the wrapper's
  token and inherit its one-hour life. A `gh` command that outlives the token
  fails with a 401 from GitHub; rerun it. Git remotes must be HTTPS for the
  wrapper to matter at cutover (`gh repo clone` writes HTTPS); an SSH remote
  bypasses both helpers.

`borant credential <name>` is the same call as a subcommand of the `borant`
binary: it prints only the value on stdout, so an agent can run
`TOKEN=$(borant credential github)` when a tool needs the variable itself. It
is the client that both helpers and the tests use.

The daemon's own git needs the helper too. `checkpoint::restore` in
`crates/borant-host/src/commands.rs` clones and fetches a project before the
session env exists, and a checkpoint push runs from the daemon as well, so the
daemon exports `BORANT_SOCKET` and the helper directory for its own git
children (or passes `-c credential.helper=...` to each call). The host-wide
`/etc/gitconfig` rule then applies to it unchanged. This is task 2's work, not
the session's.

No value in argv (`get` and the gh args only), in the agent environment, in
the session log, in NATS, or in a file. The sandbox already binds the host
socket; `system_paths()` in `crates/borant-host/src/sandbox.rs` adds the helper
directory and `/etc/gitconfig` when they exist, read-only. Both helpers use a
2 s connect timeout and a 25 s whole-call timeout, above the store's 20 s, so
the plugin's timeout always wins and the message is the plugin's.

*Accepted when:* `just test-crate borant credential` shows git getting a token
from a fake daemon, the `quit=true` line on 503, and a fake `gh` printing a
`GH_TOKEN` that is absent from `/proc/self/cmdline`; and a host-side test
shows the daemon's own `git clone` reaching the fake daemon through the
helper.

## 3. Where the App key lives: 1Password, read by the plugin per mint

Chris stores one Secure Note `github-app` in the `Borant` vault:

| Ref | Value |
|---|---|
| `op://Borant/github-app/app-id` | the App ID |
| `op://Borant/github-app/installation-id` | the installation ID |
| `op://Borant/github-app/borant-swarm.2026-10-25.private-key.pem` | the `.pem`, attached to the note |

The credential's `target` is the note, `op://Borant/github-app`. The plugin
composes each field ref from the target and a field name, and reads the
attached file's name, the repository list, and the permission object from its
own `env` table, so a rotated key or a narrowed installation is a `host.toml`
edit, never a plugin edit and never a core change. The key exists in the
plugin's memory and its stdin pipe for one mint; the rule is controlled
handling, not "nothing else ever holds the key", and every path that could
leave a copy is named below.

```toml
# host.toml
[secrets.stores.github-app]
kind = "exec"
command = ["borant-secrets-github-app"]   # from borant-infra
credential = "store-token"                # the existing secret zero
timeout_secs = 20
env = { KEY_FILE = "borant-swarm.2026-10-25.private-key.pem",
        REPOSITORIES = "borant,borant-infra",
        PERMISSIONS = "contents=write,pull_requests=write,metadata=read,statuses=read" }

[secrets.credentials.github]
store = "github-app"
target = "op://Borant/github-app"
```

The plugin runs as a fresh process with an empty environment plus `PATH`,
`HOME`, its `env` table, and `BORANT_CREDENTIALS_DIRECTORY`, so the service
account token in `$CREDENTIALS_DIRECTORY/store-token` is its only credential
(the exec protocol, rule 7). It signs an RS256 JWT (`iat` now - 60, `exp` now
+ 540, `iss` = `app-id`; GitHub caps a JWT at 10 minutes) and POSTs
`/app/installations/<installation-id>/access_tokens` with a body of exactly:

```json
{"repositories": ["borant", "borant-infra"],
 "permissions": {"contents": "write", "pull_requests": "write",
                 "metadata": "read", "statuses": "read"}}
```

This body is the fence. The installation reaches all 23 repositories, so the
token must be narrowed at mint time, and `workflows` and `actions` are absent
on purpose: a worker must not change CI, and the token enforces that where a
prompt cannot. Both objects come from `env`. A store without `REPOSITORIES`
or `PERMISSIONS` returns `invalid` and never POSTs, so a token for every
repository cannot be made by a config slip. The mint returns `token` and
`ttl_secs` from the reply's `expires_at`.

No named file holds the key, because no cleanup is reliable under the
timeout: the caller kills the plugin with `SIGKILL`, so a shell `trap` never
runs and a temporary `.pem` would survive. The bytes go from `op` to the
signer and never to the filesystem:

- `op read "op://Borant/github-app/$KEY_FILE"` prints the attached PEM on
  stdout, and the pipe ends in `openssl dgst -sha256 -sign /dev/stdin`, which
  reads the key from stdin.
- The JWT goes to `curl --config -` on stdin as `header = "Authorization:
  Bearer ..."`, so neither the JWT nor the key appears in an argv (`ps` shows a
  plugin process for its 20 s), and the payload and body are built with `jq`.
- stdout carries only the protocol JSON; stderr carries logs with no key and
  no token (the adapter copies stderr at `debug` after redaction).

Rejected: `LoadCredential` of the `.pem` into `borant-host`. It needs a
root-owned push path, rotation needs a restart, and the daemon would hold a
signing key for its whole life, readable by the same user as every agent,
whereas the current path keeps the key in a 20 s process. Rejected: the daemon
signs the JWT, because that puts GitHub's API in the core.

What an agent can read on the box, stated plainly. With no sandbox, an agent
runs as `chris` and can read the service account token, can `op read` the App
key with it, can read the env and `/proc/<pid>/environ` of any process it
owns, and can reach other processes through `/proc/<pid>/root`; the sandbox
mounts `/proc` (`crates/borant-launch/src/sandbox.rs`, `--proc /proc`) without
`--unshare-pid`, so a sandboxed agent sees host PIDs too. The sandbox is
therefore not the key boundary; the `--unshare-pid` task narrows it, and the
real fence stays the user separation of a later phase. What the sandbox and
the helper design do buy: the key is never on disk, the token is never in the
agent's environment or argv, it reaches only the two repositories it was
minted for, with four permissions and no `workflows` or `actions`, and it dies
within the hour; and the `GH_TOKEN` that reaches `gh`'s children (git hooks, a
subprocess) is only the short-lived token, not the App key. Per-agent
authorization of the API needs peer credentials plus a session map, and it is
cut.

*Accepted when:* a search of `/etc`, `/run/borant`, and the plugin's own temp
space finds no PEM after a mint, including a mint killed at the 20 s timeout;
the key is absent from every bundle and every session directory; a token
minted in a session lists exactly `willisrocks/borant` and
`willisrocks/borant-infra` at `GET /installation/repositories`; a push to a
third repository of the installation is refused; and a commit that edits
`.github/workflows` is refused, which proves the narrowed token rather than
the URL.

## 4. When the plugin or GitHub is down

| Failure | Plugin | Daemon | git / gh in a session |
|---|---|---|---|
| GitHub 5xx, network, timeout | `unavailable` | 503, one log line, no value | stderr `borant: no token for credential 'github': the store is unavailable`; git prints `quit=true`, gh exits 1 |
| App revoked, installation gone, permission missing | `denied` | 403 | names `denied`, never a value |
| Item, field, or attached key file missing | `not_found` | 404 | names the missing 1Password ref |
| Key does not parse | `invalid` | 400 | names `invalid` and the item, never a value |
| `describe` fails at host start | - | store left out, one `error` log | same as unavailable |
| Daemon down | - | - | `borant: no token for credential 'github': the host daemon is not reachable at <socket>` |

The harness never breaks, by construction:

1. Nothing calls the API at launch, in a hook, or in the shim; the launch path
   gains two env entries and one sandbox flag, and nothing that can fail.
2. The credential store is named by no bundle, so `SecretConfig::open` leaves
   out no harness when it fails (unlike a bundle store).
3. The handler is one async task with the plugin's 20 s timeout; a hung plugin
   costs one request, not the daemon.
4. Failure is per operation: a `git fetch` fails with the line above while the
   rest of the session works, and the next call retries after the 5 s negative
   cache.

*Accepted when:* with the App key renamed, a clean host still launches and
registers agents, `gh` and `git` fail with one clear line that names the
failure and carries no value, and no harness record loses a bundle or a
harness kind; a unit test proves the same with a store that returns
`unavailable`.

## 5. Cutover

Until this lands, hosts keep the current `gh` login and the `github` PAT
bundle, and `[secrets.credentials]` is absent, so `host.toml` is today's file.
At cutover: add the store and credential above, delete `[secrets.bundles.github]`
and its `[secrets.harness]` entry, install the gitconfig and
`/run/borant/bin`, set every project `remote_url` in `hub.toml` to HTTPS (the
mirror and every agent clone use it, and SSH bypasses both helpers), and
`gh auth logout --hostname github.com` on the box.
Consequences to expect: pushes and PRs act as `borant-swarm[bot]`, so the PR
author is the App and commits keep the `GIT_AUTHOR_*` identity that Borant
already sets; installation tokens have their own hourly limit, so the agents
stop sharing Chris's 5,000 requests per hour
([rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)).

*Accepted when:* a fresh host reaches the two repositories with no login, and a
host that still runs the old file behaves exactly as today.

## Cuts

- Per-agent authorization of the API, peer-credential checks, a plugin
  user, and user separation. `--unshare-pid` narrows what a sandboxed agent
  can see; it does not close the same-user gap.
- A GitHub name, a URL, or JWT code in `borant-core`, `borant-host`, or
  `borant-secrets`; the credential name lives in `host.toml` only.
- Wrapper mode, `refresh` of a bundle file, and a `credential-cache` helper
  (a disk cache of a one-hour token buys nothing).
- Validation against a second GitHub Enterprise host, and a token for Actions
  or for a deploy key (the installation scope is the answer).
- Reading the App key into the daemon, an HSM, or a long-lived JWT cache.

## Tasks

Six workers. Task 0 (the sandbox flag) and task 1 land first and in parallel;
2 and 4 follow task 1; 3 follows 2; 5 waits for 0, 2, 3, and 4. The App
`borant-swarm` and the `github-app` note exist (created 2026-10-25 17:40 UTC),
so nothing waits for Chris. Each `borant` task ends with `cargo check
--workspace --all-targets`, `just fmt`, and `just lint`.

| Task | Blocked by | Repo | Files | Proof |
|---|---|---|---|---|
| 0. Sandbox: `--unshare-pid` | - | `borant` | `crates/borant-launch/src/sandbox.rs`, its tests | the argv carries `--unshare-pid` together with the existing `--proc /proc`; an integration test starts a host-visible `sleep`, wraps it, and shows the sandboxed process sees neither that PID nor `/proc/<pid>/root`, while the harness and the socket still work; `just test-crate borant-launch sandbox` |
| 1. Core: the mint port, the exec op, the store-open rule, and the token redaction | - | `borant` | `borant-core/src/ports/secrets.rs`, `ids.rs`, `ports/mod.rs`, `testing.rs`, `borant-secrets/src/{exec,config,env,file,lib}.rs`, `rules/redact.rs` | `just test-crate borant-core mint` + `just test-crate borant-secrets mint`: the contract passes on a shell fixture; a mint-only `describe` opens (config.rs accepts `resolve` or `mint`); `describe` without `mint` gives `Invalid`; `mint_without_ttl_is_internal` covers a malformed reply. Redaction: `\bgh[pousr]_[A-Za-z0-9]{20,}` misses the stateless `ghs_<appid>_<jwt>` token of 2026-10-02 (underscores, about 520 chars), so the pattern becomes `[A-Za-z0-9_-]{20,}`; a synthetic stateless token is fully `[redacted]`, and `lr_`/`sk-`/PEM cases still pass |
| 2. Host: the cache, the API, the session env, and the daemon's git | 1 | `borant` | new `borant-host/src/credential.rs`, `api.rs`, `commands.rs`, `lib.rs`, `crates/borant/src/config.rs` (`[secrets.credentials]`, `[secrets] helpers`), `crates/borant/src/cli.rs` (`borant credential`), `borant-host/src/sandbox.rs` (`system_paths`), one `borant doctor` check | `just test-crate borant-host credential` with a fake store and paused tokio time: reuse before `refresh_at`, a remint past it, `ttl < 60 s` handed out but not stored, two concurrent calls mint once, a waiter gets the value the first call minted, a failure gives 503 and a second call within 5 s mints nothing, no log or `Debug` holds a value (the `Reply` becomes a `Value`, so the test greps the captured log), a failed credential store leaves `harnesses` and `bundles` of `HostInfo` unchanged, the launch spec carries `BORANT_SOCKET` plus the helper `PATH`, and the daemon's own `git clone` reaches the fake daemon. Doctor: ok, warn when the store is missing, one mint an hour at most |
| 3. Helpers: `borant-git-credential` and `borant-gh` | 2 | `borant` | `crates/borant/src/bin/borant-git-credential.rs`, `borant-gh.rs`, their tests | `just test-crate borant credential`: git's `get` stdin gives `username=x-access-token` and the password; 503 gives one stderr line and `quit=true`, exit 0; a dead socket names it; `borant credential github` prints the value and nothing else; the wrapper execs a fake `gh` that prints `$GH_TOKEN` and `/proc/self/cmdline`, so the value is in the env and absent from argv, and `GITHUB_TOKEN` is gone |
| 4. The plugin in `borant-infra` | 1 | `borant-infra` (private) | `pkgs/borant-secrets-github-app.nix`, `modules/borant.nix` | `nix build`, then its test with a fake `op`, a fake `curl`/`gh api` and no network: `describe` lists `mint`; `mint` returns a token and `ttl_secs` ≤ 3600; the JWT decodes to `iss` = `app-id` and an `exp` - `iat` of 600 s; the recorded POST body is exactly `{"repositories":["borant","borant-infra"],"permissions":{"contents":"write","pull_requests":"write","metadata":"read","statuses":"read"}}`; a store without `REPOSITORIES` or `PERMISSIONS` gives `invalid` and makes no request; the fake `curl` records argv holding neither the key nor the JWT; 401 → `denied`, 500 → `unavailable`, a missing ref → `not_found`; a killed mint leaves no PEM under `/run/borant`; the token is absent from `describe`'s stdout and from stderr |
| 5. Live on `borant-2`, then cut over the fleet | 0, 2, 3, 4 | `borant-infra` | `modules/borant.nix` (gitconfig, `/run/borant/bin`, `PATH`, HTTPS remotes), `host.toml` (the store `env` and the credential) | `gh auth logout`; `gh repo clone` of `willisrocks/borant` with the pinned `gh` works, and `gh pr create --draft` from a scratch branch opens a PR authored by `borant-swarm[bot]`, then closes it; `git ls-remote` and `git push --dry-run` work; `GET /installation/repositories` lists exactly `borant` and `borant-infra`; a push to a third repository of the installation is refused, and a push that edits `.github/workflows` is refused; two `borant credential github` inside a minute return the same value (cache); `BORANT_SOCKET=/tmp/none git ls-remote ...` fails with the clean line; the `github` bundle is gone from `HostInfo.bundles` |

Task 5 waits only on the code. The App and the note exist, and the store's
`env` names the key file as attached today; a rotated key is one `env` edit.

## Sources

- [1] Authentication as a GitHub App installation (one-hour tokens):
  https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-as-a-github-app-installation
- [2] Generating an installation access token:
  https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app
- [3] Stateless `ghs_APPID_JWT` tokens, 2026-10-02:
  https://github.blog/changelog/2026-10-02-stateless-github-app-installation-tokens-rolled-out/
- [4] `gitcredentials(7)`, credential helpers and `quit`:
  https://git-scm.com/docs/gitcredentials
- [5] REST rate limits, installation tokens:
  https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- Local, read on 2026-10-25: `gh help environment` (`GH_TOKEN` wins over
  `GITHUB_TOKEN` and stored credentials), `crates/borant-core/src/rules/redact.rs`,
  `crates/borant-host/src/{api,secrets,commands,sandbox}.rs`,
  `crates/borant-secrets/src/exec.rs`.
