# Phase 12: fresh GitHub tokens for agents, from the `borant-hosts` GitHub App

Chris's decision of 2026-10-25 17:10 UTC: GitHub refuses a personal access
token for this work, so agents authenticate as the `borant-hosts` GitHub App
(account `willisrocks`, installed on `willisrocks/borant` and
`willisrocks/borant-infra` only), the pattern the steve-bot sandboxes use. An
installation token lasts one hour, so a `GH_TOKEN` at launch is not enough:
git and `gh` in a session ask the host daemon for a token, which asks a plugin
that mints one from the App key in 1Password. Section 6.3 of the
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

/// One mint. `ttl` is how long the value lives; `None` means "do not cache".
#[derive(Debug)]
pub struct Minted { pub value: Secret, pub ttl: Option<Duration> }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretCapabilities { pub resolve: bool, pub mint: bool }

pub trait SecretStore {
    // … resolve, unchanged …
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
{"ok": {"value": "ghs_…", "ttl_secs": 3540}}
```

The reply uses `ttl_secs` (the remaining life, computed by the plugin from
GitHub's `expires_at` and the plugin's own clock) so clock skew stays inside
one process. Errors follow the existing table: `unavailable` (GitHub 5xx, a
network error, a timeout), `not_found` (the item is gone), `invalid` (a key
that does not parse), `denied` (GitHub 401/403: app revoked, installation
suspended, permission missing), `internal`. A missing `value` or `ttl_secs` is
`internal`. `timeout_secs` for this store is 20, because one mint is two
1Password reads, one signature, and one HTTPS POST.

`borant-core::testing` gains `mint_contract(store, target, max_ttl)`, run by
every adapter that reports `mint`: a mint returns a non-empty value and a ttl
at most `max_ttl`, and a second mint may return the same value or a new one;
the `Debug` of the result and of a `PortError` holds no value; a store without
`mint` returns `Invalid`; an unknown target is `NotFound`. The `env` and
`file` stores report `mint: false`, and every `SecretCapabilities` literal in
`borant-secrets` and in the host's fake stores gets the new field.

*Accepted when:* the contract suite passes against a shell fixture plugin
whose target is `fixture://app`, and the new types and the `mint` op name no
backend: the names `github`, `GITHUB`, `ghs_`, and `op://` appear in no line
that task 1 adds to `crates/borant-core/src` or `crates/borant-secrets/src`
(the redaction pattern that task 1 edits keeps its `gh[pousr]_` shape, a
pattern, not a backend).

## 2. How a session asks: one host API call, two helper programs

The host daemon caches per credential name and answers a loopback call on its
existing 0600 Unix socket. The hub is not involved; nothing crosses NATS.

```
POST /v1/credential            {"name": "github"}
200 {"ok": {"value": "ghs_…", "expires": 1792952400000}}
400 invalid / 403 denied / 404 not_found / 503 unavailable / 500 internal
```

`expires` is the end of the token (`mint` time + `ttl`), not the earlier
moment the cache stops reusing it; a helper does not need it, and a client may
use it to warn. The reply wraps in `Reply::Ok` like every other route.

`crates/borant-host/src/credential.rs` holds the cache: `BTreeMap<CredentialName,
Cached { value: Arc<Secret>, expires: Timestamp }>`, a `tokio::sync::Mutex` per
name for single-flight, and a 5 s negative cache so a failing build loop mints
once. Reuse until `expires - min(4 min, ttl / 10)`; a `ttl` under 60 s is not
cached. Successes only. The reply type `IssuedCredential` lives in the host
crate (not in `proto.rs`), has `Serialize` on a `String`, and a hand-written
`Debug` that prints `IssuedCredential(..)`; a unit test asserts both. The body
is not logged, no session event carries it, and the value exists in the daemon
only as the `Arc<Secret>` of the cache and of the response being written. A
credential whose `store` is not in `[secrets.stores]`, or whose store failed
to open, logs one `error` at start and answers 404: a typo in a new host file
must not stop a host that otherwise works.

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
      helper = "!borant-git-credential github"
      helper =            # reset: no stored login, no `gh auth git-credential`
  ```

- `borant-gh`, the `gh` wrapper, symlinked as `/run/borant/bin/gh`. It fetches
  the value, sets `GH_TOKEN` in its own environment (`GITHUB_TOKEN` removed,
  `GH_PROMPT_DISABLED=1`), and `exec`s the real binary named by `BORANT_GH`
  (set by `borant-infra` to the pinned Nix path), so no wrapper process stays
  in front. On failure it prints one line to stderr and exits 1.
  `GH_TOKEN` wins over stored credentials (`gh help environment`), and it is
  in the environment of `gh` only, never in the agent's environment, argv, or
  a dump.

No value in argv (`get` and the gh args only), in the agent environment, in
the session log, in NATS, or in a file. The sandbox already binds the host
socket; `system_paths()` in `crates/borant-host/src/sandbox.rs` adds the helper
directory and `/etc/gitconfig` when they exist, read-only. Both helpers use a
2 s connect timeout and a 25 s whole-call timeout, above the store's 20 s, so
the plugin's timeout always wins and the message is the plugin's.

*Accepted when:* `just test-crate borant credential` shows git getting a token
from a fake daemon, the `quit=true` line on 503, and a fake `gh` printing a
`GH_TOKEN` that is absent from `/proc/self/cmdline`; on borant-2,
`git clone` and `git push --dry-run` to `willisrocks/borant` work with no gh
login and no token in the environment.

## 3. Where the App key lives: 1Password, read by the plugin per mint

Chris stores a Secure Note `github-app` (fields `app-id`, `installation-id`)
and a Document `github-app-pk` (the `.pem`) in the `Borant` vault. The plugin
reads both on each mint and returns a token; nothing else ever holds the key.

```toml
# host.toml
[secrets.stores.github-app]
kind = "exec"
command = ["borant-secrets-github-app"]   # from borant-infra
credential = "store-token"                # the existing secret zero
timeout_secs = 20

[secrets.credentials.github]
store = "github-app"
target = "op://Borant/github-app"
```

The plugin runs as a fresh process with an empty environment plus `PATH`,
`HOME`, its `env` table, and `BORANT_CREDENTIALS_DIRECTORY`, so the service
account token in `$CREDENTIALS_DIRECTORY/store-token` is its only credential
(the exec protocol, rule 7). It signs an RS256 JWT (`iat` now − 60, `exp` now
+ 540, `iss` = `app-id`; GitHub caps a JWT at 10 minutes), POSTs
`/app/installations/<installation-id>/access_tokens`, and returns `token` and
`ttl_secs` from the reply's `expires_at`. The Document is unpacked to a 0600
file under a private `/run/borant/tmp` directory, signed, and deleted on every
path (`trap`); the key never enters `host.toml`, an env var, a `LaunchSpec`, or
a bundle.

Rejected: `LoadCredential` of the `.pem` into `borant-host`. It needs a
root-owned push path, rotation needs a restart, and the daemon would hold a
signing key for its whole life, readable by the same user as every agent,
whereas the current path keeps the key in a 20 s process. Rejected: the daemon
signs the JWT, because that puts GitHub's API in the core.

What an agent can read on the box: its own secret directory; the service
account token (the known gap of the secrets doc, section 7) and, through it,
the App key, unless it is sandboxed; and any credential that the daemon has
configured, because the API has no per-agent authorization in this phase. The
limits are deliberate: a sandboxed agent sees neither `/run/borant/tmp` nor
`/run/credentials`, and an installation token reaches the two repositories of
the installation and dies within the hour. Per-agent authorization needs peer
credentials plus a session map, and it is cut.

*Accepted when:* `grep -r github-app /etc /run/borant` on the host finds only
`host.toml`, the key is absent from every bundle and every session directory,
and a token minted in a session is valid for `GET
/installation/repositories` and nothing else.

## 4. When the plugin or GitHub is down

| Failure | Plugin | Daemon | git / gh in a session |
|---|---|---|---|
| GitHub 5xx, network, timeout | `unavailable` | 503, one log line, no value | stderr `borant: no token for credential 'github': the store is unavailable`; git prints `quit=true`, gh exits 1 |
| App revoked, installation gone, permission missing | `denied` | 403 | names `denied`, never a value |
| Item or Document missing | `not_found` | 404 | names the missing 1Password item |
| Key does not parse | `invalid` | 400 | names `invalid` and the item, never a value |
| `describe` fails at host start | - | store left out, one `error` log | same as unavailable |
| Daemon down | - | - | `borant: no token for credential 'github': the host daemon is not reachable at <socket>` |

The harness never breaks, by construction:

1. Nothing calls the API at launch, in a hook, or in the shim; `borant
   secret-exec` and the launch path do not change.
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

Until the App exists, hosts keep the current `gh` login and the `github` PAT
bundle, and `[secrets.credentials]` is absent, so `host.toml` is today's file.
At cutover: add the store and credential above, delete `[secrets.bundles.github]`
and its `[secrets.harness]` entry, install the gitconfig and
`/run/borant/bin`, and `gh auth logout --hostname github.com` on the box.
Consequences to expect: pushes and PRs act as `borant-hosts[bot]`, so the PR
author is the App and commits keep the `GIT_AUTHOR_*` identity that Borant
already sets; installation tokens have their own hourly limit, so the agents
stop sharing Chris's 5,000 requests per hour
([rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)).

*Accepted when:* a fresh host reaches the two repositories with no login, and a
host that still runs the old file behaves exactly as today.

## Cuts

- Per-agent authorization of the API, peer-credential checks, and a plugin
  user (the sandbox is the fence this phase).
- A GitHub name, a URL, or JWT code in `borant-core`, `borant-host`, or
  `borant-secrets`; the credential name lives in `host.toml` only.
- Wrapper mode, `refresh` of a bundle file, and a `credential-cache` helper
  (a disk cache of a one-hour token buys nothing).
- Validation against a second GitHub Enterprise host, and a token for Actions
  or for a deploy key (the installation scope is the answer).
- Reading the App key into the daemon, an HSM, or a long-lived JWT cache.

## Tasks

Five workers. Task 1 lands first; 2 and 4 follow it in parallel; 3 follows 2;
5 waits for 4 and for Chris's App. Each `borant` task ends with `cargo check
--workspace --all-targets`, `just fmt`, and `just lint`.

| Task | Blocked by | Repo | Files | Proof |
|---|---|---|---|---|
| 1. Core: the mint port, the exec op, and the token redaction | - | `borant` | `borant-core/src/ports/secrets.rs`, `ids.rs`, `ports/mod.rs`, `testing.rs`, `borant-secrets/src/{exec,env,file,lib}.rs`, `rules/redact.rs` | `just test-crate borant-core mint` + `just test-crate borant-secrets mint`: the contract passes on a shell fixture; `describe` without `mint` gives `Invalid`; `mint_without_ttl_is_internal` covers a malformed reply. Redaction: `\bgh[pousr]_[A-Za-z0-9]{20,}` misses the stateless `ghs_<appid>_<jwt>` token of 2026-10-02 (underscores, about 520 chars), so the pattern becomes `[A-Za-z0-9_-]{20,}`; a synthetic stateless token is fully `[redacted]`, and `lr_`/`sk-`/PEM cases still pass |
| 2. Host: the cache, the API, and the session env | 1 | `borant` | new `borant-host/src/credential.rs`, `api.rs`, `commands.rs`, `lib.rs`, `crates/borant/src/config.rs` (`[secrets.credentials]`, `[secrets] helpers`), `borant-host/src/sandbox.rs` (`system_paths`), one `borant doctor` check | `just test-crate borant-host credential` with a fake store and a `ManualClock`: reuse before the margin, a remint past it, `ttl < 60 s` never cached, two concurrent calls mint once, a failure gives 503 and two calls in 5 s mint once, `IssuedCredential`'s `Debug` and the log hold no value, a failed credential store leaves `harnesses` and `bundles` of `HostInfo` unchanged, and the launch spec carries `BORANT_SOCKET` plus the helper `PATH`. Doctor: ok, warn when the store is missing, one mint an hour at most |
| 3. Helpers: `borant-git-credential` and `borant-gh` | 2 | `borant` | `crates/borant/src/bin/borant-git-credential.rs`, `borant-gh.rs`, their tests | `just test-crate borant credential`: git's `get` stdin gives `username=x-access-token` and the password; 503 gives one stderr line and `quit=true`, exit 0; a dead socket names it; the wrapper execs a fake `gh` that prints `$GH_TOKEN` and `/proc/self/cmdline`, so the value is in the env and absent from argv, and `GITHUB_TOKEN` is gone |
| 4. The plugin in `borant-infra` | 1 | `borant-infra` (private) | `pkgs/borant-secrets-github-app.nix`, `modules/borant.nix` | `nix build`, then its test with a fake `op`, a fake `curl`/`gh api` and no network: `describe` lists `mint`; `mint` returns 40+ chars and `ttl_secs` ≤ 3600; the JWT decodes to `iss` = `app-id` and an `exp` − `iat` of 600 s; 401 → `denied`, 500 → `unavailable`, a missing item → `not_found`; the key and the token are absent from stdout of `describe` and from stderr, and no secret is in argv |
| 5. Live on `borant-2`, then cut over the fleet | 4, Chris's App | `borant-infra` | `modules/borant.nix` (gitconfig, `/run/borant/bin`, `PATH`), `host.toml` | `gh auth logout` then `gh api /installation/repositories` lists exactly the two repos; `git ls-remote` and `git push --dry-run` work; two `borant credential github` inside a minute return one value while a direct plugin call returns another (cache, not mint); `BORANT_SOCKET=/tmp/none git ls-remote …` fails with the clean line; the `github` bundle is gone from `HostInfo.bundles` |

Task 5 cannot start before Chris creates the App, installs it on the two
repositories, and stores the note and the document; until then tasks 1 to 4
are testable with fixtures and a fake store.

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
