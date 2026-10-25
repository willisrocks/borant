# Secrets and host bootstrap design

Date: 2026-10-24. Status: draft for review. This is track 1 of five parallel
design docs. The others, in this folder:

- `2026-10-24-elastic-compute-design.md` (the elastic-compute doc)
- `2026-10-24-orchestration-api-and-sandboxes-design.md`
- `2026-10-24-observability-design.md`
- `2026-10-24-durable-orchestration-vision-design.md`

## Summary

A new Borant host must start work with no browser login and no first-run
screen. 1Password stays the store of record. A new `Borant` vault holds only
the secrets that hosts need, and a new read-only service account
(`borant-hosts`) can read only that vault. Its token is the one secret that a
host gets at creation ("secret zero"). At boot, a systemd unit runs `op
inject` and writes a tmpfs env file. The host daemon and each harness session
read that file through a small wrapper. The harnesses use long-lived tokens:
`CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`, the LunaRoute `lr_` key
through `omp auth import`, and a fine-grained GitHub token in `GH_TOKEN`. A
seed unit writes the first-run flags of Claude Code and omp. Nix pins both
harness binaries. A Hetzner snapshot cuts the create time later, but
zero-touch does not depend on it. `borant doctor` tests each host, and the
host daemon advertises only the harnesses that pass, so the scheduler skips
hosts that are not ready. Phase 1 changes only `borant-infra`.

## 1. Goals and non-goals

Goals:

1. A new VPS host runs Claude Code and omp agents with zero interactive steps.
2. Each secret has one source, one rotation step, and a known blast radius.
3. An agent can get a per-task secret, such as a database password, without
   the value in a prompt, a message, or the session log.
4. The scheduler skips a host that is not ready.
5. Harness versions are pinned in the flake, the same on every host.

Non-goals:

- A highly available hub. This design keeps the hub out of the secret path,
  so a hub outage does not stop a host from booting.
- Creation of hosts by the hub. The elastic-compute doc owns it. This doc
  gives it the create steps and the secret path.
- Sandboxes and per-agent isolation (orchestration-api-and-sandboxes doc).
- Laptop setup. Laptops keep their interactive logins.

## 2. Headless auth for each tool

### 2.1 Claude Code

Recommendation: run `claude setup-token` once on the laptop, store the output
in `op://Borant/claude-code/oauth-token`, and deliver it as
`CLAUDE_CODE_OAUTH_TOKEN`. `ANTHROPIC_API_KEY` is the switch for API billing
(open question 1).

- `setup-token` runs the browser flow once and prints an OAuth token that
  lasts one year. It needs a Pro, Max, Team, or Enterprise plan, and it can
  only make model requests [S1].
- Precedence: cloud provider flags, `ANTHROPIC_AUTH_TOKEN`,
  `ANTHROPIC_API_KEY`, `apiKeyHelper`, `CLAUDE_CODE_OAUTH_TOKEN`, then the
  `/login` credentials [S1]. An API key on a host thus wins over the token.
- The legal page says that subscription OAuth is for "ordinary use of Claude
  Code", and that Pro and Max limits "assume ordinary, individual usage".
  Products built on Claude must use an API key [S3], [S41]. Borant runs real
  Claude Code for one person, but many unattended sessions on many hosts is a
  gray area. No source rules on it.
- One token on many hosts at once: no source forbids it or allows it
  (unverified). Rate limits are per account, so all hosts share one budget
  (unverified, expected).
- On 2.1.100, a token-only session still showed the login screen until
  `hasCompletedOnboarding` was true [S8]. Section 3 seeds that key.

### 2.2 LunaRoute and omp

Recommendation: store one `lr_` key in `op://Borant/lunaroute/api-key` and
import it with `omp auth import` at boot. If the extension is Chris's own
code, add `LUNAROUTE_API_KEY` support and a catalog fetch on omp, and delete
the import step (open question 4).

- The extension stores the key as an OAuth-shaped record: `access` is the
  `lr_` key, `refresh` is empty, `expires` is far in the future, and
  `refreshToken` returns the record unchanged. Each browser login mints a new
  key, labeled with the host name (local source, 0.14.1, `src/login.ts`). The
  key is thus a static API key. One shared key works. One key per host needs
  one browser login per key.
- On omp, login offers only the browser flow, and an omp-only host has an
  empty model list, because omp does not refresh the catalog [S11].
- 0.14.1 reads no `LUNAROUTE_API_KEY` variable (local grep). 0.15.0 was not
  inspected (unverified).
- omp 18.12.6 has `omp auth export` and `omp auth import <file|->` (local
  help). The export shape is `{version, credentials: [{provider, credential:
  {type, access, refresh, expires}}]}`. Only key names were read, not values.
- omp keeps credentials in `auth_credentials` in `~/.omp/agent/agent.db`, and
  it has `omp config set <key> <value>` (local schema and help).

### 2.3 GitHub

Recommendation: one fine-grained personal access token (PAT), limited to the
repositories that agents work in, with an expiry of 366 days, stored in
`op://Borant/github/token` and delivered as `GH_TOKEN`. Make `gh` the git
credential helper and rewrite SSH GitHub URLs to HTTPS in the system
gitconfig. Then pushes, `gh pr create`, and checkpoint pushes use one token,
and no box needs a GitHub SSH key.

| Option | Lifetime | Scope | Rate limit | Upkeep |
|---|---|---|---|---|
| Fine-grained PAT (chosen) | 1 to 366 days, or none if the org allows it [S13] | Named repositories of one owner | 5,000 per hour, shared with Chris [S16] | One renewal a year |
| Classic PAT | Any | Every repository Chris can reach [S13] | Same | Large blast radius |
| GitHub App installation token | 1 hour [S14] | Up to 500 repositories per token [S14] | 5,000 to 12,500 per hour, its own budget [S16] | A timer that mints a token each hour |

- Permissions: Contents, Pull requests, and Workflows, all write. A push that
  changes `.github/workflows/` fails without Workflows (user report, names
  unverified).
- `gh` recommends `GH_TOKEN` for fine-grained tokens and headless use [S15].
- No GitHub doc limits a push to `refs/borant/*` (unverified). The Actions
  `push` event covers branches and tags only [S17], so WIP refs most likely
  start no workflow. Phase 0 tests the push.
- From 2026-04-27, App tokens can use a longer format [S14]. The redaction
  pattern `gh[pousr]_[A-Za-z0-9]{20,}` can leave its tail. Section 5 adds
  exact-value redaction.

### 2.4 Tailscale

Recommendation: replace the reusable auth key with a Tailscale OAuth client
(scope `auth_keys`, tag `tag:borant`) in the laptop-only `Agents` vault. `just
create` mints one single-use, pre-authorized, tagged key with a one-hour
expiry for each new box. Turn on Tailscale SSH with an ACL rule from
`tag:borant` and Chris's devices to `tag:borant` as `chris`, and delete
`keys/borant-1.pub` and `keys/borant-2.pub`.

- Auth keys last 1 to 90 days and can be single-use, ephemeral,
  pre-authorized, and tagged. Node key expiry is off for tagged nodes, so a
  box stays joined after its key ends [S18].
- The OAuth client secret has no stated expiry. Its access tokens last one
  hour. The secret also works directly as an auth key [S19], but a box needs
  only a one-off key, so the secret stays on the laptop.
- The NixOS module runs `tailscale up` only when the node is logged out, and
  has `authKeyParameters` for `ephemeral` and `preauthorized` [S21].
- Tailscale SSH takes access from ACL `ssh` rules. A rule from a tagged source
  must use `accept` [S20]. NixOS support is not documented (unverified), but
  `--ssh` is a normal `extraUpFlags` value.
- Workload identity federation needs a public OIDC issuer [S40]. Hetzner gives
  servers none, so this doc cuts it.

### 2.5 Hetzner

Recommendation: keep the Hetzner token on the laptop in `Agents`, and put the
Borant boxes in their own Hetzner project. If the hub creates hosts later, it
gets its own token in that project. Tokens are Read or Read and Write and show
once [S22]. Project scope and expiry are not stated (unverified).

## 3. First-run screens

Recommendation: a oneshot unit `borant-harness-seed` runs as `chris` before
`borant host` at each boot. It merges keys into files that the harnesses own,
and never replaces a whole file, because the harnesses write to these files.
Settings that Nix can own go into `/etc/claude-code/managed-settings.json`.

### 3.1 Claude Code

| Screen | Key | File |
|---|---|---|
| Onboarding and theme | `hasCompletedOnboarding: true`, `lastOnboardingVersion` | `~/.claude.json` [S8] |
| Bypass warning | `skipDangerousModePermissionPrompt: true` | Managed settings [S4] |
| Folder trust | `projects."<cwd>".hasTrustDialogAccepted: true` | `~/.claude.json` [S5] |
| Auto-update | `env.DISABLE_AUTOUPDATER: "1"` | Managed settings [S2] |

- The first interactive bypass session shows a warning. Background sessions
  honor the key only in user or managed settings [S4]. Bypass mode refuses
  root [S4], and Borant runs sessions as `chris`.
- Trust is saved per directory, and never for the home directory [S5]. Trust
  of a parent folder is not documented (unverified).
- The key names come from `~/.claude.json` and `~/.claude/settings.json` on
  2.1.310 on the laptop. Older names such as `bypassPermissionsModeAccepted`
  are absent there.

Borant makes a new worktree under `~/.borant/work/{agent_id}/{repo}` for each
spawn and move. The launch wrapper (section 4.3) thus adds the trust key for
`$PWD` just before it starts Claude Code. It takes a `flock`, writes with
`jq` to a temporary file, and renames it. If phase 0 shows that trust of
`~/.borant/work` covers its children, the seed unit writes one key and the
wrapper step goes away.

### 3.2 omp and LunaRoute

The seed unit runs:

```sh
omp config set setupVersion 2
omp config set modelRoles.default lunaroute/deepseek-4.1-flash:auto
omp auth import /run/borant/omp-auth.json && rm /run/borant/omp-auth.json
jq -s '.[0] * .[1]' ~/.pi/agent/models-store.json /etc/borant/lunaroute-catalog.json \
  > "$tmp" && mv "$tmp" ~/.pi/agent/models-store.json
```

- `setupVersion: 2` skips the wizard on omp 18.12.6. A nested key in `omp
  config set` is unverified, and a `yq` merge into `config.yml` is the
  fallback.
- `lunaroute-catalog.json` is the `lunaroute` entry of the laptop
  `models-store.json`. It holds no secret, so it lives in `borant-infra`, and
  `just catalog` refreshes it. The extension reads this file (local source,
  `src/lunaroute.ts`). A stale catalog only hides new models.
- `omp install @lunaroute/pi-extension@0.15.0` runs only if the extension is
  missing.

## 4. Secret delivery to hosts

### 4.1 Options

| Option | Rotation | Blast radius of one host | Secret zero | Complexity |
|---|---|---|---|---|
| 1Password service account, `op inject` at boot (chosen) | Edit the item. Hosts take it at the next refresh. | Every item in `Borant` | The service account token | Low: one unit and one CLI |
| sops-nix with host age keys [S28] | Edit, commit, deploy to every host | Only that host's secrets | The host SSH key, known before the first deploy | Medium: `.sops.yaml` grows, and `sops updatekeys` for each new host [S29] |
| agenix or agenix-rekey [S30] | Same as sops-nix | Same | Same | Medium |
| Laptop push over the tailnet | `just push-secrets` to all hosts | Only what was pushed | None | Low, but no laptop means no new host |
| Hetzner user-data | A new server | All | The Hetzner API | Low, but any local process reads it (section 6.2) |
| Secrets broker on the hub | Edit on the hub | Per-host policy | Tailscale node identity | High, and the hub is a single point of failure |
| OpenBao, Vault, or Infisical | Strong | Per-host policy | An AppRole or machine identity | High. Infisical rotation needs a paid plan [S38]. |

systemd credentials [S34] are not in the table, because they protect data on
disk and do not deliver it. The design uses them inside the host.

### 4.2 Why the service account wins

- Chris keeps every secret in 1Password, and the laptop already uses a
  read-only service account. This adds one vault and one account.
- A host from a snapshot, made by the laptop or the hub, needs two small
  values: a one-off Tailscale key and the service account token. sops-nix
  needs a rekey and a commit for each new host.
- Agent secret refs (section 5) need `op` access on the host at run time.
  sops-nix cannot resolve a reference that an agent asks for.
- Rotation is one edit in 1Password, with no deploy.

Costs and limits:

- Reads per token per hour: 10,000 on Business, 1,000 on Teams, Families, and
  Individual. Reads per account per day: 50,000 on Business, 5,000 on Teams,
  1,000 on Families or Individual [S31]. Ten hosts with three secrets and two
  refreshes a day use about 60 reads a day.
- A service account cannot change its vault access after creation. Its
  expiry is set at creation with `--expires-in` [S32]. Whether "never" is
  allowed is unverified.
- 1Password Environments give service accounts read access in a beta [S33].
  This doc cuts them until they leave beta.

The trade-off: root or `chris` on any host can read every item in `Borant`,
and one shared token means a revoke stops all hosts. Per-host service
accounts fix both, but each needs a human to create it (open question 3).

### 4.3 Concrete design

| Vault | Item and field | Reader | Use |
|---|---|---|---|
| `Borant` (new) | `claude-code/oauth-token` | `borant-hosts` | `CLAUDE_CODE_OAUTH_TOKEN` |
| `Borant` | `lunaroute/api-key` | `borant-hosts` | omp import |
| `Borant` | `github/token` | `borant-hosts` | `GH_TOKEN` |
| `Borant` | Task items, for example `staging-db/url` | `borant-hosts` | Agent secret refs |
| `Agents` | Hetzner token, Tailscale OAuth client, `borant-hosts` token | Laptop only | `just create` |

A new `modules/secrets.nix` in `borant-infra` (a sketch):

```nix
let
  envTpl = pkgs.writeText "secrets.env.tpl" ''
    CLAUDE_CODE_OAUTH_TOKEN={{ op://Borant/claude-code/oauth-token }}
    GH_TOKEN={{ op://Borant/github/token }}
  '';
  ompTpl = pkgs.writeText "omp-auth.json.tpl" ''
    {"version": 1, "credentials": [{"provider": "lunaroute", "credential": {"type": "oauth",
      "access": "{{ op://Borant/lunaroute/api-key }}", "refresh": "", "expires": 4102444800000}}]}
  '';
in {
  systemd.services.borant-secrets = {
    wantedBy = [ "multi-user.target" ];
    before = [ "borant-harness-seed.service" "borant-host.service" ];
    wants = [ "network-online.target" ];
    after = [ "network-online.target" ];
    path = [ pkgs._1password-cli ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      LoadCredential = "op-token:/var/lib/borant-secrets/op-token";
      RuntimeDirectory = "borant";
      RuntimeDirectoryPreserve = "yes";
    };
    script = ''
      umask 077
      export OP_SERVICE_ACCOUNT_TOKEN="$(< "$CREDENTIALS_DIRECTORY/op-token")"
      op inject -f -i ${envTpl} -o /run/borant/secrets.env
      op inject -f -i ${ompTpl} -o /run/borant/omp-auth.json
      chown ${user} /run/borant/secrets.env /run/borant/omp-auth.json
    '';
  };
  systemd.timers.borant-secrets.timerConfig.OnCalendar = "*-*-* 04,16:00:00";
  systemd.services.borant-host.serviceConfig.EnvironmentFile = "/run/borant/secrets.env";
}
```

- `LoadCredential` gives the root-only token file to this unit only. The
  token never enters the env of `borant host` or of agents.
- The `version` in the omp template must match `omp auth export` (unverified).
- After a timed refresh, the unit restarts `borant-host`. The restart is safe,
  because tmux sessions survive it (`KillMode = "process"`).

The tmux server keeps the env from its own start. A session must thus read
the env file when it starts. The wrapper also keeps values out of `tmux
new-session -e` arguments, which show in `ps`. In `modules/borant.nix`:

```nix
withSecrets = pkgs.writeShellScript "borant-with-secrets" ''
  set -a; . /run/borant/secrets.env; set +a
  if [ "$1" = claude ]; then ${trustCwd}; fi   # section 3.1
  exec "$@"
'';
commands = {
  claude = [ "${withSecrets}" "claude" "--dangerously-skip-permissions" "--plugin-dir" "${share}/claude-code" ];
  omp = [ "${withSecrets}" "omp" ];
};
programs.git.config = {
  user = { name = "<open question 5>"; email = "<open question 5>"; };
  credential."https://github.com".helper = "!${pkgs.gh}/bin/gh auth git-credential";
  url."https://github.com/".insteadOf = [ "git@github.com:" "ssh://git@github.com/" ];
};
```

Borant appends `-- <prompt>` to these commands
(`crates/borant-launch/src/harness.rs`), and the wrapper passes it on through
`"$@"`. Phase 1 needs no change to Borant code.

On the laptop, `just create <name>` becomes:

1. Mint a one-off key with the Tailscale OAuth client: `POST
   /api/v2/tailnet/-/keys` with `reusable: false`, `preauthorized: true`,
   `tags: ["tag:borant"]`, and `expirySeconds: 3600` [S19].
2. Create the server. Today, `nixos-anywhere --extra-files` copies the
   Tailscale key and the service account token. With a snapshot, see
   section 6.
3. Wait until `tailscale ping <name>` answers.
4. Run `ssh chris@<name> borant doctor`, which prints any failed checks.

## 5. Secrets for agents

Recommendation: a Borant feature, "secret refs". A `spawn` call names env vars
and 1Password references, never values. The host daemon resolves them at
launch, only for that agent, and redacts each value from that agent's log.

```json
{"tool": "spawn", "harness": "omp", "prompt": "Run the migration test",
 "secrets": {"DATABASE_URL": "op://Borant/staging-db/url"}}
```

```toml
# host.toml
[secrets]
op = "/run/current-system/sw/bin/op"   # on a laptop: the op-agents wrapper
token_file = "/var/lib/borant-secrets/op-token"
```

Rules (calls for the builder):

1. The agent record stores the refs. A move carries them, and the new host
   resolves them again.
2. Refs must start with `op://Borant/`. The service account scope is the real
   limit, and the prefix is a second fence.
3. The daemon writes the values to a mode 0600 file,
   `/run/borant/agents/<agent>.env`. The wrapper reads it and deletes it
   before `exec`. Values never go into tmux arguments or the control plane.
4. `redact()` in `crates/borant-core/src/rules/redact.rs` is pattern-only. The
   session log and messages pass through it (`delivery.rs` and `sessions.rs`
   in `borant-host`). Add a per-agent set of exact values, plus the values of
   `secrets.env`, and remove them before the patterns run.
5. Add patterns for `lr_`, `tskey-`, and `ops_` keys and the longer GitHub App
   token format. The `ops_` prefix for 1Password service account tokens is
   unverified.
6. The prompt names the variable, never the value: "The database URL is in
   `$DATABASE_URL`."
7. If a ref fails to resolve, the launch fails and the hub tries another
   host. A host with no `[secrets]` section does not get agents with refs.

Limits: an agent can print a value in a form that the exact match misses, for
example base64. Every agent runs as `chris`, so it can read the env of other
agent processes. Per-agent isolation belongs to the
orchestration-api-and-sandboxes doc.

## 6. Golden image

Recommendation: do zero-touch secrets first, on the current `nixos-anywhere`
path. Then add a Hetzner snapshot of a generic `borant-host` configuration to
cut create time from about ten minutes to one or two. The snapshot is a speed
step, not a requirement.

### 6.1 Build and contents

| Option | How | Trade-off |
|---|---|---|
| `nixos-anywhere` on a throwaway box, then snapshot (chosen) | Install the generic configuration with no secrets, delete the SSH host keys and `/etc/machine-id`, shut down, `hcloud server create-image --type snapshot` | Uses the path that works today, on x86-64 Linux. One clean-up step. |
| disko image and upload | `system.build.diskoImagesScript` [S36], then `hcloud-upload-image --location hil` [S27] | Clean and reproducible, but needs an x86-64 Linux builder with a VM. A 4 GB upload takes 8 to 12 minutes [S37]. |
| nixpkgs `raw-efi` variant | `nixos-rebuild build-image --image-variant raw-efi`. nixos-generators is archived [S35]. | No Hetzner variant [S39]. Same builder need. |

- Snapshots from Ashburn, Hillsboro, and Singapore stay in that location
  [S23], so make the images in `hil`. Snapshots survive the deletion of the
  source server [S23].
- The snapshot price after the 2026 price changes [S25] was not found
  (unverified). The elastic-compute doc owns prices.

| In the image | At first boot |
|---|---|
| NixOS with the flake system of `borant-host` | Host name, from Hetzner metadata |
| borant, Claude Code, omp, gh, tmux, git, op, and the tools of `hosts/common.nix` | One-off Tailscale key, from user-data |
| The seed units, the wrapper, managed settings, the catalog, the gitconfig | The `borant-hosts` token, pushed over the tailnet |

The generic configuration sets `networking.hostName = ""`. A oneshot
`borant-firstboot` reads `http://169.254.169.254/hetzner/v1/metadata/hostname`
and `.../userdata`, sets the host name, and writes the Tailscale key file. The
path comes from the cloud-init Hetzner datasource [S26]. No official Hetzner
page for it was found (unverified). This doc cuts `services.cloud-init` for a
ten-line unit.

### 6.2 Why the token is not in user-data

User-data cannot change after creation, and every local process can read the
metadata service (unverified for Hetzner, true on most clouds). Agents run as
`chris`. Thus user-data carries only the single-use Tailscale key, which is
worthless after the join. The laptop, or the hub for elastic hosts, pushes the
service account token over Tailscale SSH after the join. An nftables rule also
drops metadata traffic from every UID except root, and phase 2 tests it:

```nix
networking.nftables.tables.metadata = {
  family = "inet";
  content = ''
    chain output {
      type filter hook output priority 0;
      ip daddr 169.254.169.254 meta skuid != 0 drop
    }
  '';
};
```

### 6.3 Pinned harnesses and freshness

Recommendation: add `pkgs/claude-code.nix` and `pkgs/omp.nix` in the style of
`pkgs/borant.nix`: a release binary with a fixed hash and
`autoPatchelfHook`. Keep `programs.nix-ld` for other binaries.

- Claude Code publishes a `manifest.json` for each version at
  `https://downloads.claude.ai/claude-code-releases/<version>/manifest.json`,
  with a SHA-256 for each platform, and a GPG signature from 2.1.89 [S6],
  [S7]. The binary is at `.../<version>/linux-x64/claude`. At research time,
  `stable` was 2.1.308 and `latest` was 2.1.310.
- nixpkgs `claude-code` fetches the same binary and lagged about one version
  [S9]. `sadjow/claude-code-nix` updates every hour [S10]. A 20-line local
  file avoids a third-party flake input, and nixpkgs is the fallback.
- The omp release asset URL is unverified [S12].
- `just bump-claude <version>` and `just bump-omp <version>` write the new
  hashes.

Rebuild the snapshot when `flake.lock` or a pin changes, and keep the two
newest. A host from an older snapshot still works, because `just deploy`
brings it to the current flake. `borant doctor` reports the flake revision of
the host. This doc cuts `system.autoUpgrade`, because deploys are push-based.

## 7. `borant doctor` and host readiness

Recommendation: a `borant doctor [--json] [--fix] [--strict]` command, and the
same checks inside `borant host`, at start and every ten minutes. The daemon
advertises in `host.{host}` only the harnesses that pass, plus a list of host
problems. The scheduler already filters by harness
(`crates/borant-core/src/rules/schedule.rs`). It needs one new filter: a host
with a problem does not qualify.

| Check | Scope | How | `--fix` |
|---|---|---|---|
| Binary and version | Harness | `claude --version`, `omp --version` against the pin | No |
| Claude auth | Claude Code | A token or key in the env, then `claude auth status` (behavior with an env token unverified). `--strict`: one `claude -p` turn. | No |
| Claude first-run state | Claude Code | `hasCompletedOnboarding`, `skipDangerousModePermissionPrompt` | Yes |
| Claude trust | Claude Code | Trust key for `~/.borant/work`, or the wrapper step is on | Yes |
| omp setup | omp | `omp config get setupVersion` is at least the expected value | Yes |
| LunaRoute | omp | A `lunaroute` credential exists, and the catalog has the default model. `--strict`: one `omp -p` turn. | Catalog only |
| GitHub | Host | `gh auth status`, then `gh api repos/<o>/<r> --jq .permissions.push` | No |
| Git identity | Host | `user.name` and `user.email` are set | No |
| Checkpoint remote | Host | `git ls-remote` of the remote | No |
| NATS | Host | Connected, round trip under one second | No |
| Clock | Host | `timedatectl` reports NTP sync, and skew to the hub reply time is under two seconds | No |
| Disk | Host | More than 5 GB free in `BORANT_HOME` and `/nix/store` | No |
| Launcher | Host | Start and stop a test tmux session | No |
| Secrets | Host | `/run/borant/secrets.env` exists and is younger than 24 hours | No |

`HostInfo` gets a new field, `problems: Vec<String>`, with `#[serde(default)]`.
Its `harnesses` field becomes computed. The `harnesses` list in `host.toml`
becomes the candidates, and `doctor`
removes the ones that fail. A host that loses Claude auth thus stays in
service for omp work. The notifier tells Chris when a host gets a problem.
The display of readiness belongs to the observability doc. `--strict` costs a
small amount of model quota, so the daemon runs it at most once an hour.

## 8. Phased plan

| Phase | Work | Repo | Done when |
|---|---|---|---|
| 0. Facts | On `borant-2`, test: parent-folder trust, `omp auth import` of a LunaRoute record, nested `omp config set`, a token-only Claude Code session on 2.1.310, and a push of `refs/borant/test/wip` with the PAT | None | Each unverified item in sections 2 and 3 has a result |
| 1. Zero-touch secrets | `Borant` vault and service account, `modules/secrets.nix`, the wrapper, the seed unit, managed settings, the gitconfig, Tailscale key minting and Tailscale SSH | `borant-infra` | `just create borant-3` gives a working host with no login |
| 2. Pins and snapshot | `pkgs/claude-code.nix`, `pkgs/omp.nix`, the generic `borant-host`, `borant-firstboot`, the metadata rule, `just image` | `borant-infra` | A host from the snapshot passes `borant doctor` in three minutes |
| 3. Doctor and readiness | `borant doctor`, computed `harnesses`, `problems`, the scheduler filter | `borant` | The scheduler skips a host that fails a check |
| 4. Agent secret refs | `secrets` on `spawn`, launch resolution, exact-value redaction, new patterns | `borant` | A spawned agent uses a password that never shows in its session log |

## 9. What this doc cut

- Per-host service accounts, because each needs a human to create it.
- sops-nix and agenix as the main path. sops-nix is the fallback if Chris
  wants no internet dependency at boot.
- OpenBao, Vault, Infisical, a broker on the hub, and 1Password Environments.
- GitHub App tokens, until the shared PAT rate limit hurts.
- Tailscale workload identity federation.
- The disko and `raw-efi` image builds, until the snapshot path is too slow.
- `system.autoUpgrade`, cloud-init, and Packer.
- NATS credentials per host. NATS on `borant-1` has no auth today, and the
  tailnet is the only fence. The small fix is a tailnet ACL that opens port
  4222 only to `tag:borant` and Chris's devices. NATS user credentials can
  later come through the same `op inject` path.

## 10. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Anthropic treats a subscription token on many unattended hosts as outside "ordinary, individual usage" | The account is limited or blocked | Open question 1. The switch to `ANTHROPIC_API_KEY` is one template line. |
| A compromised host | Every item in `Borant` leaks | Laptop-only secrets stay in `Agents`. Rotate the token and the three harness secrets. |
| The wrapper and a live Claude session write `~/.claude.json` at the same time | A lost trust key, and a blocked session | `flock` and an atomic rename. Parent trust, if phase 0 shows it works, removes the write. |
| An omp upgrade changes `setupVersion` or the credential format | omp sessions block or have no model | Pinned omp, and `borant doctor` tests both |
| 1Password is down at boot | A new host has no secrets | The unit retries. Running hosts keep the old env file. |
| A secret printed in a form that redaction misses | The value reaches the hub session log | JetStream encryption at rest, exact-value redaction, short retention |

## 11. Open questions for Chris

1. Claude billing: keep the Max subscription token on all hosts, or move
   unattended hosts to an `ANTHROPIC_API_KEY`? The terms favor an API key for
   heavy automated use.
2. Which 1Password plan do you have? Teams caps the whole account at 5,000
   service account reads a day.
3. Is one shared `borant-hosts` token acceptable, given that a revoke stops
   every host at once?
4. Is the LunaRoute extension your code? If yes, `LUNAROUTE_API_KEY` support
   and a catalog fetch on omp remove the import and the catalog file.
5. Which git identity do agents commit as: you with your GitHub noreply
   address, or a bot identity?
6. May phase 0 create a scratch GitHub repository to test the push of
   `refs/borant/*`? The main design also waits on this test.
7. May `just create` switch to Tailscale SSH with a new ACL rule, so the
   per-box SSH keys go away?

## Sources

- [S1] Claude Code authentication: https://code.claude.com/docs/en/authentication
- [S2] Claude Code environment variables: https://code.claude.com/docs/en/env-vars
- [S3] Claude Code legal and compliance: https://code.claude.com/docs/en/legal-and-compliance
- [S4] Claude Code permission modes: https://code.claude.com/docs/en/permission-modes
- [S5] Claude Code security, trust verification: https://code.claude.com/docs/en/security
- [S6] Claude Code setup and binary integrity: https://code.claude.com/docs/en/setup
- [S7] Claude Code 2.1.310 manifest: https://downloads.claude.ai/claude-code-releases/2.1.310/manifest.json
- [S8] Onboarding screen with a token: https://github.com/anthropics/claude-code/issues/46259
- [S9] nixpkgs `claude-code`: https://github.com/NixOS/nixpkgs/tree/nixos-unstable/pkgs/by-name/cl/claude-code
- [S10] claude-code-nix: https://github.com/sadjow/claude-code-nix
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
- [S24] Hetzner servers FAQ, cloud-init on Hetzner images only: https://docs.hetzner.com/cloud/servers/faq/
- [S25] Hetzner price adjustment: https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/
- [S26] cloud-init Hetzner datasource patch: https://archive.lists.launchpad.net/cloud-init-dev/msg04260.html
- [S27] hcloud-upload-image: https://github.com/apricote/hcloud-upload-image
- [S28] sops-nix: https://github.com/Mic92/sops-nix
- [S29] Secret management with sops-nix: https://michael.stapelberg.ch/posts/2025-08-24-secret-management-with-sops-nix/
- [S30] agenix: https://github.com/ryantm/agenix, agenix-rekey: https://github.com/oddlama/agenix-rekey
- [S31] 1Password service account rate limits: https://www.1password.dev/service-accounts/rate-limits/
- [S32] 1Password service accounts: https://www.1password.dev/service-accounts/get-started/
- [S33] 1Password Environments: https://www.1password.dev/environments
- [S34] systemd credentials: https://systemd.io/CREDENTIALS/
- [S35] nixos-generators, archived: https://github.com/nix-community/nixos-generators
- [S36] disko images: https://github.com/nix-community/disko/blob/master/docs/disko-images.md
- [S37] NixOS Hetzner images, Outskirts Labs: https://docs.outskirtslabs.com/nixos-hetzner/next/index.md
- [S38] Infisical pricing: https://infisical.com/pricing.md
- [S39] nixpkgs image variants: https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/image/images.nix
- [S40] Tailscale workload identity federation: https://tailscale.com/kb/1581/workload-identity-federation
- [S41] February 2026 OAuth clarification: https://winbuzzer.com/2026/02/19/anthropic-bans-claude-subscription-oauth-in-third-party-apps-xcxwbn/
- Local, read on 2026-10-24: `@lunaroute/pi-extension` 0.14.1 `src/login.ts` and `src/lunaroute.ts`, `omp` 18.12.6 help and `agent.db` schema, `claude` 2.1.310 key names, and the Borant crates named in the text.
