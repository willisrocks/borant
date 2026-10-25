# Secrets and host bootstrap design

Date: 2026-10-24. Status: draft for review, revision 2. This is track 1 of
five parallel design docs in this folder. The others are the elastic-compute
doc (`2026-10-24-elastic-compute-design.md`), the orchestration doc
(`2026-10-24-orchestration-api-and-sandboxes-design.md`), the observability
doc, and the durable orchestration vision.

Revision 2 follows Chris's direction: Borant wraps the agent with its secrets,
so the running agent does not handle them, and the store is a port with
adapters. Revision 1 made a boot-time `op inject` of all host secrets the main
path. That path is now one way to fill the `file` adapter (section 5.1).

## Summary

Borant gets a `SecretStore` port in `borant-core`: a trait that the core owns
and that adapters implement. The user-facing concept is a bundle, a named set
of env vars and files whose values come from one store. Bundles are defined in
`host.toml`. Harness defaults, profiles, channels, and spawns attach bundles by
name. The hub stores and sends only names, so values never travel through
NATS.

The host daemon is the only caller. In resolver mode, it reads the values and
gives them to the harness as env vars and as files on a per-agent tmpfs. In
wrapper mode, it puts a provider CLI such as `doppler run --` in front of the
harness and never holds the values. The `env` and `file` adapters are compiled
in, and the `exec` adapter runs a plugin that speaks the exec plugin protocol
of the elastic-compute doc. `borant-infra` ships a 1Password plugin as the
default. Harness logins become ordinary bundles, so the seed unit handles only
first-run screens. The host still needs one store credential ("secret zero"),
delivered with systemd `LoadCredential`.

## 1. Goals and non-goals

Goals:

1. A new VPS host runs Claude Code and omp agents with zero interactive steps.
2. A running agent never fetches a secret. Borant gives it what it needs.
3. 1Password is the default store. Doppler, secretspec, Infisical, or a plain
   file work with no change to the core.
4. Values never reach prompts, the session log, NATS, tmux arguments, or
   crash dumps.
5. The scheduler skips a host that is not ready.

Non-goals: a secret service on the hub (a hub outage must not stop a launch),
creation of hosts by the hub (elastic-compute doc), sandbox internals
(orchestration doc), and laptop setup (laptops keep their interactive logins).

## 2. The `SecretStore` port

### 2.1 Types and trait

A new file, `crates/borant-core/src/ports/secrets.rs`. The types name no
backend. A `SecretRef` is an opaque string that one backend understands, for
example `op://Borant/github/token`, a Doppler key, or a file name.

```rust
// Both: Debug, Clone, Eq, Ord, Hash, Serialize, Deserialize, #[serde(transparent)]
pub struct SecretRef(pub String);   // a ref that one backend understands; never parsed
pub struct BundleName(pub String);  // for example `github`; safe to log and send to the hub

/// No Display, Serialize, or Clone. A hand-written Debug prints `Secret(..)`.
/// Zeroed on drop. `expose(&self) -> &str` is the only reader; grep for it in review.
pub struct Secret(zeroize::Zeroizing<String>);

/// Env vars and files from one store. Defined in host.toml.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Bundle {
    pub name: BundleName,
    pub store: String,                                      // a store in host.toml
    #[serde(default)] pub vars: BTreeMap<String, SecretRef>,  // ENV_NAME -> ref
    #[serde(default)] pub files: BTreeMap<String, SecretRef>, // file name -> ref
    #[serde(default)] pub params: BTreeMap<String, String>,   // not secret, wrapper mode
    #[serde(default)] pub pre: Vec<String>,                   // runs before the harness
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities { pub resolve: bool, pub wrap: bool, pub refresh: bool }

pub struct Resolved { pub values: BTreeMap<SecretRef, Secret>, pub ttl: Option<Duration> }

/// Wrapper mode: the argv before the harness program, never with a value in it.
pub struct Wrap { pub prefix: Vec<String>, pub env: Vec<(String, Secret)>, pub redact: Vec<Secret> }

#[async_trait::async_trait]
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn kind(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// All refs or an error. One missing ref fails the whole call.
    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved>;
    async fn wrap(&self, bundle: &Bundle) -> PortResult<Wrap> {
        Err(PortError::new(PortErrorKind::Invalid, "no wrapper mode"))
    }
}
```

`PortErrorKind` gets `Invalid` and `Denied`, so that the plugin error codes
map one to one (section 5.2). If the elastic-compute revision adds them first,
this doc uses those. `zeroize` does no I/O [S58], so the no-I/O rule of
`borant-core` holds. `Secret` has no `Serialize`, so the compiler stops a
value in any NATS type.

### 2.2 Who calls it

Only the host daemon calls the port. It calls `resolve` or `wrap` in the
launch path in `crates/borant-host/src/commands.rs`, just before it builds the
`LaunchSpec`, and again when a file must refresh (section 8.3). The hub never
calls it. A move carries bundle names, and the new host resolves them with its
own store. The daemon caches values per store for `cache_secs` (default 600),
so a wake or a quick relaunch does not read the backend again.

### 2.3 Contract suite

`borant-core::testing` gets one suite. Each adapter crate runs it.

```rust
pub async fn secret_store_contract(store: &dyn SecretStore, known: &[(SecretRef, &str)], missing: SecretRef)
```

It asserts these rules:

1. `resolve` of all known refs returns each exact value, twice in a row.
2. An empty list gives an empty map, and a duplicate ref gives one entry.
3. A call with one missing ref fails with `NotFound` and returns no values.
4. The Debug output of the result contains no known value.
5. If `capabilities().wrap` is true, `wrap` returns a non-empty `prefix` with
   no known value and no value from its own `env`.

## 3. Bundles

### 3.1 Configuration

Stores and bundle definitions live in `host.toml`, because a ref is only
useful to the store on that host. `hub.toml` names bundles but never defines
them.

```toml
# host.toml
[secrets.stores.op]
kind = "exec"
command = ["borant-secrets-1password"]   # from borant-infra
credential = "store-token"              # a file in $CREDENTIALS_DIRECTORY (section 7)
timeout_secs = 10

[secrets.bundles.github]
store = "op"
vars = { GH_TOKEN = "op://Borant/github/token" }

[secrets.bundles.claude-auth]
store = "op"
vars = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-code/oauth-token" }

[secrets.bundles.lunaroute]
store = "op"
files = { "omp-auth.json" = "op://Borant/lunaroute/omp-auth-record" }
pre = ["omp", "auth", "import", "{dir}/omp-auth.json"]

[secrets.bundles.staging-db]
store = "op"
vars = { DATABASE_URL = "op://Borant/staging-db/url" }

[secrets.bundles.app-stg]                # wrapper mode, through a Doppler plugin
store = "doppler"
params = { project = "app", config = "stg" }

[secrets.harness]                        # harness defaults, "*" is every harness
"*" = ["github"]
claude-code = ["claude-auth"]
omp = ["lunaroute"]

# hub.toml: names only. Profiles are from the orchestration doc, section 4.
[profiles.worker]
bundles = ["staging-db"]
[channels.payments]
bundles = ["app-stg"]
```

A spawn names bundles too: `{"tool": "spawn", "profile": "worker", "bundles":
["staging-db"], ...}`. `{dir}` is the agent secret directory (section 4.1). A
bundle with `vars` or `files` runs in resolver mode. A bundle with only
`params` on a store that has `wrap` runs in wrapper mode.

### 3.2 Four levels and the merge

The host daemon merges four levels in this order. A later level wins.

| Order | Level | Defined in | Example |
|---|---|---|---|
| 1 | Harness default | `host.toml` `[secrets.harness]` | `claude-code` gets `claude-auth`, all get `github` |
| 2 | Profile | `hub.toml` `[profiles.<name>]` | `worker` gets `staging-db` |
| 3 | Channel | `hub.toml` `[channels.<name>]` | Agents in `payments` get `app-stg` |
| 4 | Spawn | `bundles` on `spawn` | One agent gets `prod-readonly` |

Rules:

1. The list is the union of the levels in order, without duplicates. If two
   bundles set the same var or file, the later one wins, and the daemon logs
   both bundle names.
2. A launch has at most one wrapper-mode bundle, because stacked wrappers are
   hard to debug. A second one fails the launch with `invalid`.
3. Level 1 comes from the host that runs the launch. A move from omp to
   Claude Code thus drops `lunaroute` and adds `claude-auth`.
4. An agent that spawns an agent can name only bundles that it holds. A human
   can name any bundle. Thus a child never gets more access than its parent.

### 3.3 What the hub stores

The hub stores names, never values or refs. All new fields use
`#[serde(default)]`, so old records still parse.

- `Agent.bundles`: the spawn level only. Profile and channel levels come from
  `hub.toml` at each launch, so a change there applies at the next launch.
- `LaunchOrder.bundles`: levels 2 to 4, merged. The host adds level 1.
- `HostInfo.bundles`: the bundles that the host defines. The scheduler
  (`crates/borant-core/src/rules/schedule.rs`) skips a host that does not
  define every bundle of the launch.

## 4. Injection modes

### 4.1 Resolver mode

The host daemon reads the values through the store and starts the harness
with them. This is the default.

1. The daemon calls `resolve` once for each store, with all its refs.
2. It creates `/run/borant/agents/<agent>/` (0700) on the `/run` tmpfs. On
   macOS the directory is under `$TMPDIR`, which is not a tmpfs.
3. It writes each `files` entry, and the env vars to an `env` file, all 0600.
4. It sets only `BORANT_SECRETS_DIR` in `LaunchSpec.env`, and puts a shim in
   front of the program: `borant secret-exec <dir> -- claude ...`.
5. The shim, a new subcommand of the `borant` binary, reads `env`, deletes
   it, sets the vars, runs each `pre` command, and calls `exec` on the
   harness.
6. When the session ends, the daemon deletes the directory. At start, it
   deletes each directory with no live session.

The shim is needed because the tmux launcher puts each `LaunchSpec.env` pair
in a `-e KEY=VALUE` argument (`new_session_args` in
`crates/borant-launch/src/tmux.rs`), and `ps` shows it. Every agent runs as
`chris`, so an unsandboxed agent can still read the env of another agent
through `/proc/<pid>/environ`. Only a sandbox closes this (section 8.4).

### 4.2 Wrapper mode

The host daemon puts a provider CLI in front of the harness. The CLI fetches
the values and calls `exec`. Borant never holds them.

| Provider | Prefix |
|---|---|
| Doppler | `doppler run --project P --config C --` [S46] |
| secretspec | `secretspec run --profile P --` [S44] |
| 1Password | `op run --no-masking --env-file F --` [S50] |
| Infisical | `infisical run --projectId ID --` [S54] |

The store returns the prefix from `wrap`, with `params` filled in. The argv
is `borant secret-exec <dir> -- <prefix> <harness program>`. The shim still
runs, because the provider token and any resolver bundles go through `env`.

Wrapper mode has three costs:

- Redaction. Borant cannot remove a value that it never saw. A plugin can
  return the values in `Wrap.redact`, for example from `doppler secrets
  download --no-file --format json` [S47], and Borant keeps them in memory
  for redaction only. Without them, only the patterns of section 8.2 apply,
  and a value with no known shape reaches the session log if an agent prints
  it.
- The provider token. The CLI needs its token in its env, for example
  `DOPPLER_TOKEN` [S46], and the harness most likely inherits it (unverified).
  The agent can then read every secret that the token reaches, so a wrapper
  token must be scoped to one config or one project.
- The mask of `op run` [S50]. Borant takes the session log from hooks and
  transcripts, not from the terminal, so the mask does not protect the log,
  and it can take the terminal away from a TUI harness (unverified). The
  preset passes `--no-masking`.

secretspec asks for a `--reason` when it detects an AI agent [S42]. If that
fires for a launch, the preset adds `--reason "borant launch <agent>"`.

### 4.3 When to use each mode

| Use | Mode | Why |
|---|---|---|
| Harness logins, GitHub, task secrets on Chris's hosts | Resolver | Exact-value redaction, files on tmpfs |
| A team that keeps app secrets in Doppler, secretspec, or Infisical and wants its own tool to fetch them | Wrapper | Borant holds no values |
| A secret with a short TTL in a long session | Resolver, `files` | Only a file can change under a running process |
| A laptop | Resolver, `env` adapter | No backend and no network |

## 5. Adapters

### 5.1 Compiled-in adapters

A new crate, `borant-secrets`, holds two adapters with no network:

- `env`: a ref is the name of a variable in the env of the host daemon. For
  laptops and tests.
- `file`: a ref is a file name in a directory, for example
  `/run/borant/secrets/`. It refuses a file that other users can read. The
  `op inject` unit of revision 1 [S51], sops-nix [S28], or agenix [S30] can
  fill it, for a host that must boot with no access to the store.

Both run the contract suite. Neither has `wrap` or `refresh`.

### 5.2 The `exec` adapter and the plugin protocol

The `exec` adapter runs a plugin program for each operation. It speaks the
exec plugin protocol of the elastic-compute doc (section "Exec plugin
protocol"). That doc owns the protocol. The convention, restated exactly:

1. Request: one JSON object on stdin, `{"v": 1, "op": "<name>", ...args}`.
2. Response: one JSON object on stdout, either `{"ok": <value>}` or
   `{"error": {"code": "<code>", "message": "<text>"}}`.
3. Error codes include `unavailable`, `not_found`, `invalid`, `denied`, and
   `internal`. They map to `PortErrorKind` `Unavailable`, `NotFound`,
   `Invalid`, `Denied`, and `Other`.
4. A non-zero exit with no JSON counts as `internal`.
5. stderr is for logs only. The adapter redacts it, passes it to `tracing`,
   and never parses it.
6. Each op has a timeout (`timeout_secs`, default 10). The adapter then kills
   the plugin and returns `Timeout`.
7. No secret goes in an argument.

| Op | Args | `ok` value | Required |
|---|---|---|---|
| `describe` | none | `{"v": 1, "kind": "secrets", "backend": name, "capabilities": ["resolve", "wrap", "refresh"]}`, the same handshake shape as the provisioner `describe` in the elastic compute doc | Yes |
| `resolve` | `"refs": [string]` | `{"values": {ref: value}, "ttl_secs": number or null}` | With `resolve` |
| `wrap` | `"bundle": name, "params": {k: v}` | `{"prefix": [string], "env": {k: value}, "redact": [value]}` | With `wrap` |

```json
{"v": 1, "op": "resolve", "refs": ["op://Borant/github/token"]}
{"ok": {"values": {"op://Borant/github/token": "github_pat_..."}, "ttl_secs": null}}
```

The adapter calls `describe` at daemon start, and a failure is a host problem
(section 10). It reads the stdout of `resolve` and `wrap` straight into
`Secret` values and never logs it. If the store names a `credential`, it sets
`BORANT_CREDENTIALS_DIRECTORY` in the plugin env (section 7). Its tests run
the contract suite against a shell plugin fixture, plus bad JSON, a non-zero
exit, a timeout, and each error code.

### 5.3 The 1Password plugin in `borant-infra`

`borant-secrets-1password` is a short script that Nix packages with
`pkgs._1password-cli` and `jq`.

- `describe` returns `{"v": 1, "kind": "secrets", "backend": "1password", "capabilities": ["resolve", "wrap"]}`.
- `resolve` reads the token file into `OP_SERVICE_ACCOUNT_TOKEN` and runs `op
  read <ref>` for each ref [S52]. One read per ref keeps the JSON safe for
  values with quotes. Refs must start with `op://Borant/`. The service account
  scope is the real limit, and the prefix is a second fence.
- `wrap` writes an env file of refs to the agent directory and returns `op run
  --no-masking --env-file <file> --` [S50]. It exists to prove the protocol.

| Vault | Item and field | Reader | Bundle |
|---|---|---|---|
| `Borant` (new) | `claude-code/oauth-token` | `borant-hosts` | `claude-auth` |
| `Borant` | `lunaroute/omp-auth-record` | `borant-hosts` | `lunaroute` |
| `Borant` | `github/token` | `borant-hosts` | `github` |
| `Borant` | Task items, for example `staging-db/url` | `borant-hosts` | Task bundles |
| `Agents` | Hetzner token, Tailscale OAuth client, `borant-hosts` token | Laptop only | None |

Service account reads per account per day: 1,000 on Individual and Families,
5,000 on Teams, and 50,000 on Business [S31]. With the cache, 50 launches a
day with 3 refs each use at most 150 reads. Vault access is fixed at creation
[S32].

### 5.4 Other providers

Doppler, secretspec, and Infisical ship as examples in `docs/secrets/`: a
short plugin or a wrapper preset, not code in Borant.

| Tool | Resolver mode | Wrapper mode | Machine credential | Masks child output | Offline | Cost for one person | Rust library |
|---|---|---|---|---|---|---|---|
| 1Password (default) | `op read` per ref [S52] | `op run --env-file F --` [S50] | Service account, vault scope fixed at creation [S32] | Yes, by default [S50] | No | Chris's plan, rate limits [S31] | No official SDK [S53] |
| Doppler | `doppler secrets download --no-file --format json` [S47] | `doppler run -p P -c C --` [S46] | Service token for one config, `DOPPLER_TOKEN` [S46] | None found | Encrypted fallback file, on by default [S48] | Free for up to 3 users [S49] | None found |
| secretspec | `secretspec get KEY` [S44] | `secretspec run --profile P --` [S44] | Its provider's, for example `OP_SERVICE_ACCOUNT_TOKEN` [S43] | None found | With keyring, dotenv, or age providers [S42] | Free, Apache-2.0 [S44] | `secretspec` 0.21.1 [S45] |
| Infisical | Rust SDK [S56] | `infisical run --projectId ID --` [S54] | Machine identity, `INFISICAL_TOKEN` [S55] | None found | Not researched | Free for 5 identities [S38], MIT core [S57] | `infisical`, early stage [S56] |

secretspec fronts about 35 providers, 1Password and Doppler among them, with
secrets declared per profile in `secretspec.toml` [S42]. One secretspec plugin
thus reaches many stores. Its crate is before 1.0 [S45], so this doc uses its
CLI.

## 6. Harness auth as bundles

### 6.1 Claude Code: `claude-auth`

Chris runs `claude setup-token` once and stores the output in 1Password. The
bundle sets `CLAUDE_CODE_OAUTH_TOKEN`.

- The token lasts one year, needs a Pro, Max, Team, or Enterprise plan, and
  can only make model requests [S1]. `ANTHROPIC_API_KEY` wins over it [S1],
  so a move to API billing is one bundle edit (open question 1).
- Subscription OAuth is for "ordinary use of Claude Code", and products built
  on Claude must use an API key [S3], [S41]. Many unattended sessions for one
  person is a gray area, and no source rules on one token on many hosts.
- A token-only session showed the login screen until
  `hasCompletedOnboarding` was true [S8]. The seed unit sets it.

### 6.2 LunaRoute and omp: `lunaroute`

omp does not read the LunaRoute key from the env. The extension (0.14.1,
local source `src/login.ts`) keeps it in omp's credential store as an
OAuth-shaped record: `access` is the `lr_` key, `refresh` is empty, and
`expires` is far in the future. It is thus a static key, and one shared key
works. omp 18.12.6 has `omp auth import <file|->` and keeps credentials in
`~/.omp/agent/agent.db` (local help). 0.14.1 reads no `LUNAROUTE_API_KEY`
(local grep), and 0.15.0 was not inspected.

The bundle covers this with a `files` entry and a `pre` step. The 1Password
item holds the whole import record as one value, so Borant needs no
templates. The shim runs `omp auth import {dir}/omp-auth.json` before `exec`.
The cost: the import copies the key into `agent.db` on disk, as on the laptop
today. If the extension can read an env var (open question 4), the bundle
becomes one `vars` line and the key stays off the disk.

### 6.3 GitHub: `github`

One fine-grained personal access token (PAT), limited to the repositories
that agents work in, with a 366-day expiry. The bundle sets `GH_TOKEN`, which
`gh` recommends for headless use [S15]. The system gitconfig makes `gh` the
git credential helper and rewrites SSH GitHub URLs to HTTPS.

Permissions: Contents, Pull requests, and Workflows, all write [S13]. The
token shares the 5,000 requests per hour of Chris [S16]. A classic PAT reaches
every repository, so this doc cuts it. A GitHub App token lasts one hour
[S14], and becomes possible later through `refresh`: the plugin returns
`ttl_secs = 3600`, and the daemon rewrites a token file before it ends. The
Actions `push` event covers branches and tags only [S17], so a push to
`refs/borant/*` most likely starts no workflow.

Tailscale and Hetzner credentials stay on the laptop. `just create` mints a
single-use, tagged Tailscale key with a one-hour expiry [S18], [S19], and
Tailscale SSH replaces the per-box SSH keys [S20], [S21], [S22].

## 7. Secret zero

The host needs one credential for its store: a 1Password service account
token, a Doppler service token, or an Infisical machine identity. The delivery
does not depend on the backend, because it is one file.

1. `just create`, or the hub for elastic hosts, pushes the file over Tailscale
   SSH to `/var/lib/borant/credentials/store-token`, mode 0400, owner root.
   User-data never carries it (section 11).
2. The `borant-host` unit loads it with `LoadCredential` [S34]:

```nix
systemd.services.borant-host.serviceConfig = {
  LoadCredential = [ "store-token:/var/lib/borant/credentials/store-token" ];
  LimitCORE = 0;
  RuntimeDirectory = "borant/agents";
  RuntimeDirectoryMode = "0700";
};
```

3. The daemon never reads the file. It passes `$CREDENTIALS_DIRECTORY` to the
   plugin as `BORANT_CREDENTIALS_DIRECTORY`. The token never enters
   `LaunchSpec.env` or an agent directory.

The gap: systemd lets the user of the unit read the credential, and
`borant-host` runs as `chris`, like every agent. An unsandboxed agent can thus
most likely read the store token (unverified). A sandboxed agent cannot,
because bubblewrap binds only the paths that Borant names (open question 3).
To rotate secret zero, push a new file and restart `borant-host`. tmux
sessions survive the restart (`KillMode = "process"`).

## 8. Security rules

### 8.1 Where values never go

| Place | Rule |
|---|---|
| Prompts | The prompt names the variable, never the value: "The database URL is in `$DATABASE_URL`." |
| The session log | Exact-value redaction, then patterns (section 8.2) |
| NATS | Only bundle names cross. `Secret` has no `Serialize`. |
| tmux arguments and `ps` | Only `BORANT_SECRETS_DIR` and the shim argv. A wrapper prefix holds params only. |
| Crash dumps | `LimitCORE = 0`, and `prctl(PR_SET_DUMPABLE, 0)` at daemon start [S59]. The second also stops agents of the same user from reading daemon memory through `ptrace`. |
| Daemon logs | `Secret` prints `Secret(..)`. Plugin stderr is redacted. |

### 8.2 Redaction

`redact()` in `crates/borant-core/src/rules/redact.rs` is pattern-only today.
The session log and messages pass through it (`delivery.rs` and `sessions.rs`
in `borant-host`). The change:

1. A per-agent `Redactor` holds the exact values of that agent: all resolved
   values, plus `Wrap.redact` in wrapper mode. It removes them first, longest
   first, and then runs the patterns.
2. Values shorter than 8 characters are not matched, because they match too
   much text.
3. New patterns: `lr_`, `tskey-`, `ops_` (1Password service accounts,
   unverified), `dp.st.` (Doppler, unverified), and the longer GitHub App
   token format [S14].

A value printed as base64 still escapes. Encryption at rest and short
retention limit that.

### 8.3 Refresh and rotation

- Each launch resolves again, after the cache. A rotation thus reaches each
  new session within `cache_secs`.
- A running session keeps its env. A rotating secret for a long session goes
  in `files`. The daemon rewrites file entries every `refresh_secs` (default
  12 hours), or before `ttl_secs` ends.
- A failed refresh keeps the old file and adds a host problem.

### 8.4 Sandboxes and the audit log

The bubblewrap plan of the orchestration doc (section 6) binds
`/run/borant/agents/<agent>` read-only through `SandboxPlan.ro`, and nothing
else under `/run/borant` or `/run/credentials`. A sandboxed agent thus sees
only its own files.

Audit: at each launch, the daemon records a `secrets_issued` session event
with the agent, host, store names, bundle names, and mode, but no values and
no refs.

## 9. First-run screens

The seed unit no longer touches secrets. `borant-harness-seed` is a oneshot
that runs as `chris` before `borant host` at each boot. It merges keys into
files that the harnesses own, and never replaces a whole file.

| Harness | Screen | Key or step | File |
|---|---|---|---|
| Claude Code | Onboarding | `hasCompletedOnboarding: true`, `lastOnboardingVersion` | `~/.claude.json` [S8] |
| Claude Code | Bypass warning | `skipDangerousModePermissionPrompt: true` | Managed settings [S4] |
| Claude Code | Auto-update | `env.DISABLE_AUTOUPDATER: "1"` | Managed settings [S2] |
| Claude Code | Folder trust | `projects."<cwd>".hasTrustDialogAccepted: true` | `~/.claude.json` [S5] |
| omp | Setup wizard | `omp config set setupVersion 2` | `~/.omp/agent/config.yml` |
| omp | Default model | `omp config set modelRoles.default lunaroute/deepseek-4.1-flash:auto` | Same |
| omp | Model catalog | Merge `/etc/borant/lunaroute-catalog.json` | `~/.pi/agent/models-store.json` |

Each spawn gets a new worktree, and trust is saved per directory [S5]. The
pre-launch hook of the orchestration doc (section 7) thus writes the trust key
for the cwd, with a `flock` and an atomic rename, unless phase 0 shows that
parent trust works. omp does not refresh the LunaRoute catalog [S11], so the
catalog lives in `borant-infra`.

## 10. `borant doctor` and host readiness

`borant doctor [--json] [--fix] [--strict]` runs the same tests as `borant
host`, at start and every ten minutes. The daemon advertises only the
harnesses that pass, and `HostInfo` gets `problems: Vec<String>`. The
scheduler skips a host with a problem. A host that loses Claude auth stays in
service for omp work.

| Test | Scope | How | `--fix` |
|---|---|---|---|
| Binary and version | Harness | `claude --version`, `omp --version` against the pin | No |
| Store | Host | `describe` on each store | No |
| Bundles | Harness | `resolve` of each harness default bundle, values dropped, at most once an hour | No |
| Claude and omp state | Harness | The seed keys, a `lunaroute` credential, the default model in the catalog. `--strict`: one `-p` turn. | Yes, except auth |
| GitHub | Host | `gh api repos/<o>/<r> --jq .permissions.push` with the `github` bundle | No |
| Git, NATS, clock, disk, launcher | Host | Identity set, `git ls-remote`, NATS connected, NTP sync, 5 GB free, a test tmux session | No |

## 11. Golden image

A Hetzner snapshot cuts the create time from about ten minutes to one or two.
It is a speed step after zero-touch, not a requirement. The build runs `nixos-anywhere` on a throwaway box with no secrets, deletes
the SSH host keys and `/etc/machine-id`, and makes a snapshot in `hil` [S23].
At first boot, `borant-firstboot` reads the host name and the single-use
Tailscale key from the metadata service [S26] (unverified). The store token
comes over the tailnet (section 7), because every local process can most
likely read user-data, and an nftables rule drops metadata traffic from every
UID except root. `pkgs/claude-code.nix` and `pkgs/omp.nix` pin the harnesses
with the SHA-256 of each release [S6], [S7], [S12].

## 12. Phased plan

| Phase | Work | Repo | Done when |
|---|---|---|---|
| 0. Facts | On `borant-2`, test parent-folder trust, `omp auth import` of a LunaRoute record, a token-only Claude Code session, a push of `refs/borant/test/wip`, `op run --no-masking` around a TUI, and whether `doppler run` passes `DOPPLER_TOKEN` to the child | None | Each unverified item has a result |
| 1. Port and resolver mode | `SecretStore`, `Secret`, `Bundle`, the contract suite, the `env`, `file`, and `exec` adapters, bundles in `host.toml` and `hub.toml`, the merge, `bundles` on `Agent`, `LaunchOrder`, `HostInfo`, and `spawn`, the scheduler filter, `borant secret-exec`, exact-value redaction, `secrets_issued` | `borant` | An agent uses `DATABASE_URL` from a `file` bundle, and the value never shows in its session log, `ps`, or NATS |
| 2. Wrapper mode | `wrap` in the port and protocol, the one-wrapper rule, `Wrap.redact`, presets and a Doppler example in `docs/secrets/` | `borant` | An agent starts under `doppler run` with a test config |
| 3. Hosts on the store | The `Borant` vault and `borant-hosts` account, `borant-secrets-1password`, stores and bundles in `modules/borant.nix`, `LoadCredential`, the seed unit, managed settings, the gitconfig, Tailscale minting and SSH | `borant-infra` | `just create borant-3` gives a working host with no login |
| 4. Doctor | `borant doctor`, computed `harnesses`, `problems`, the store and bundle tests | `borant` | The scheduler skips a host whose store fails |
| 5. Golden image | Pins, the generic `borant-host`, `borant-firstboot`, the metadata rule, `just image` | `borant-infra` | A host from the snapshot passes `borant doctor` in three minutes |

Phase 3 can start before phase 2. The roadmap names `modules/secrets.nix`
with `op inject` at boot for its phase 1, "zero-touch hosts". It needs one
edit: phase 1 of this doc comes first, and the plugin replaces the boot unit.

## 13. What this doc cut

- Compiled-in 1Password, Doppler, Infisical, or secretspec adapters. `exec`
  covers them, and no Rust library for them is both official and past 1.0
  [S45], [S53], [S56].
- Raw refs on `spawn`. Revision 1 let a spawn name `op://` refs. A spawn now
  names bundles, so refs never cross NATS. A new secret needs one `host.toml`
  edit.
- Bundle definitions in `hub.toml`, templates in file entries, stacked
  wrappers, and a separate user for the store plugin.
- Per-host service accounts (each needs a human), and sops-nix or agenix as
  the main path (they can fill the `file` adapter).
- OpenBao, Vault, a broker on the hub, 1Password Environments (beta) [S33],
  GitHub App tokens, Tailscale workload identity federation [S40], disko and
  `raw-efi` images [S36], cloud-init, `system.autoUpgrade`, Packer, and NATS
  credentials per host (a tailnet ACL on port 4222 is the small fix).

## 14. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Anthropic treats a subscription token on many unattended hosts as outside "ordinary, individual usage" | The account is limited or blocked | Open question 1. API billing is one bundle edit. |
| An unsandboxed agent reads the store token or the env of another agent | Every item that the token reaches leaks | A scoped vault, sandboxes for untrusted work, a plugin user later |
| Wrapper mode with no `redact` values | A printed value reaches the session log | Resolver mode for secrets that agents print, and patterns as a floor |
| The store is down at launch | The launch fails | The cache, then `unavailable`, and the hub tries another host. The `file` adapter for offline boots. |
| An omp upgrade changes `setupVersion` or the credential format | omp sessions block or have no model | Pinned omp, and `borant doctor` tests both |

## 15. Open questions for Chris

1. Claude billing: keep the Max subscription token on all hosts, or move
   unattended hosts to an `ANTHROPIC_API_KEY` bundle?
2. Which 1Password plan do you have? Individual and Families cap the account
   at 1,000 service account reads a day, and Teams at 5,000.
3. Is it acceptable that an unsandboxed agent can most likely read the store
   token, and that one shared `borant-hosts` token means a revoke stops every
   host? If not, phase 3 adds a plugin user or per-host accounts.
4. Is the LunaRoute extension your code? If yes, `LUNAROUTE_API_KEY` support
   makes `lunaroute` one env var, and the import and the copy in `agent.db`
   go away.
5. Do you want wrapper mode in the first release, or resolver mode only until
   a Doppler or secretspec user asks?
6. Which git identity do agents commit as: you, or a bot identity?
7. Can phase 0 create a scratch GitHub repository to test the push of
   `refs/borant/*`?

## Sources

- [S1] Claude Code authentication: https://code.claude.com/docs/en/authentication
- [S2] Claude Code environment variables: https://code.claude.com/docs/en/env-vars
- [S3] Claude Code legal and compliance: https://code.claude.com/docs/en/legal-and-compliance
- [S4] Claude Code permission modes: https://code.claude.com/docs/en/permission-modes
- [S5] Claude Code security, trust verification: https://code.claude.com/docs/en/security
- [S6] Claude Code setup and binary integrity: https://code.claude.com/docs/en/setup
- [S7] Claude Code 2.1.310 manifest: https://downloads.claude.ai/claude-code-releases/2.1.310/manifest.json
- [S8] Onboarding screen with a token: https://github.com/anthropics/claude-code/issues/46259
- [S11] LunaRoute Pi extension: https://github.com/lunaroute/lunaroute-pi-extension
- [S12] oh-my-pi (omp): https://github.com/can1357/oh-my-pi
- [S13] GitHub personal access tokens: https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens
- [S14] GitHub App installation tokens: https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app
- [S15] `gh auth login`: https://cli.github.com/manual/gh_auth_login
- [S16] GitHub REST rate limits: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- [S17] Events that trigger workflows: https://docs.github.com/en/actions/writing-workflows/choosing-when-your-workflow-runs/events-that-trigger-workflows
- [S18] Tailscale auth keys: https://tailscale.com/kb/1085/auth-keys
- [S19] Tailscale OAuth clients: https://tailscale.com/kb/1215/oauth-clients
- [S20] Tailscale SSH: https://tailscale.com/kb/1193/tailscale-ssh
- [S21] NixOS Tailscale module: https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/services/networking/tailscale.nix
- [S22] Hetzner API tokens: https://docs.hetzner.com/cloud/api/getting-started/generating-api-token/
- [S23] Hetzner snapshots FAQ: https://docs.hetzner.com/cloud/servers/backups-snapshots/faq/
- [S26] cloud-init Hetzner datasource patch: https://archive.lists.launchpad.net/cloud-init-dev/msg04260.html
- [S28] sops-nix: https://github.com/Mic92/sops-nix
- [S30] agenix: https://github.com/ryantm/agenix
- [S31] 1Password service account rate limits: https://www.1password.dev/service-accounts/rate-limits/
- [S32] 1Password service accounts: https://www.1password.dev/service-accounts/get-started/
- [S33] 1Password Environments: https://www.1password.dev/environments
- [S34] systemd credentials: https://systemd.io/CREDENTIALS/
- [S36] disko images: https://github.com/nix-community/disko/blob/master/docs/disko-images.md
- [S38] Infisical pricing: https://infisical.com/pricing
- [S40] Tailscale workload identity federation: https://tailscale.com/kb/1581/workload-identity-federation
- [S41] February 2026 OAuth clarification: https://winbuzzer.com/2026/02/19/anthropic-bans-claude-subscription-oauth-in-third-party-apps-xcxwbn/
- [S42] secretspec: https://secretspec.dev/
- [S43] secretspec 1Password provider: https://secretspec.dev/providers/onepassword/
- [S44] secretspec repository and CLI: https://github.com/cachix/secretspec
- [S45] secretspec crate 0.21.1: https://docs.rs/secretspec/latest/secretspec/
- [S46] Doppler CLI, `doppler run` and service tokens: https://docs.doppler.com/docs/cli
- [S47] Doppler accessing secrets: https://docs.doppler.com/docs/accessing-secrets
- [S48] Doppler automatic fallbacks: https://docs.doppler.com/docs/automatic-fallbacks
- [S49] Doppler pricing: https://www.doppler.com/pricing
- [S50] 1Password `op run`: https://www.1password.dev/cli/reference/commands/run/
- [S51] 1Password `op inject`: https://www.1password.dev/cli/reference/commands/inject/
- [S52] 1Password `op read`: https://www.1password.dev/cli/reference/commands/read/
- [S53] 1Password SDKs: https://www.1password.dev/sdks/
- [S54] Infisical `infisical run`: https://infisical.com/docs/cli/commands/run
- [S55] Infisical CLI usage, machine identities: https://infisical.com/docs/cli/usage
- [S56] Infisical Rust SDK: https://infisical.com/docs/sdks/languages/rust
- [S57] Infisical repository and license: https://github.com/Infisical/infisical
- [S58] zeroize crate: https://docs.rs/zeroize
- [S59] Linux `prctl`, `PR_SET_DUMPABLE`: https://man7.org/linux/man-pages/man2/prctl.2.html
- Local, read on 2026-10-24: `@lunaroute/pi-extension` 0.14.1, `omp` 18.12.6 help and `agent.db` schema, `claude` 2.1.310 key names, and the Borant files named in the text. Revision 1 sources S9, S10, S24, S25, S27, S29, S35, S37, and S39 are cut with the text that cited them.
