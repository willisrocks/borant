# Token pools design

Date: 2026-10-25. Status: decided by Chris on 2026-10-25 (see "Decisions").
It is built in roadmap phase 1, after the `SecretStore` port. This doc
extends the secrets and host bootstrap design
(`2026-10-24-secrets-and-host-bootstrap-design.md`, "the secrets doc" below).
It uses its `SecretStore` port, bundles, resolver mode, and harness logins as
default bundles.

## Decisions

Chris answered the questions of section 8 on 2026-10-25. Where a decision
differs from the recommendation below, the decision wins.

1. The pool has 3 Claude accounts. Chris keeps a 4th account in reserve,
   outside the pool. Borant never uses it.
2. Selection is fill first, not spread. The hub fills one account until its
   limit. It picks the account whose limit resets soonest, so that capacity
   which is about to reset gets used first. This replaces the pick rule of
   section 4.2:
   1. Keep only members that the target host defines and that are not
      limited now.
   2. Pick the member with the earliest next reset, from the latest usage
      snapshot. A member with no snapshot comes last.
   3. On a tie, pick the first name.
3. Usage data comes from a new account-usage port in `borant-core::ports`,
   with neutral types (an account, its usage, its next reset). The first
   adapter is an exec plugin around Chris's tool `aiusage` (`~/Code/aiusage`,
   `aiusage --json` and `aiusage --expiring`). `aiusage` reads the macOS
   Keychain, so the plugin runs on the laptop host. The laptop host sends
   usage snapshots to the hub. No core type or config key names `aiusage`.
4. When every account is limited, the agent waits until the earliest reset,
   and Borant sends an alert. There is no fallback to an `ANTHROPIC_API_KEY`.
5. The loss of the failed turn and of the native context at a restart is
   acceptable. An own AI gateway can come later.
6. LunaRoute needs no pool.

Open point for the phase 1 spec: since v2.1.234, a Claude Code session waits
for a usage-limit reset by itself (`autoContinueAtUsageLimit`). That conflicts
with a restart on another account. Phase 2 of section 5 sets it to `false` for
pool sessions. The phase 1 spec must make sure that this setting reaches every
Claude Code session that Borant launches.

## Summary

Chris runs Claude Code on several Claude Max subscriptions and omp workers on
DeepSeek 4.1 Flash through LunaRoute. A Max account stops a session when it
reaches its 5-hour or weekly usage limit. Today the secrets doc gives every
Claude Code agent the same `claude-auth` bundle, so one account carries all
the work.

Recommendation: add pools to Borant, with a restart on limit. A pool is a
bundle with several named members, one per account. The hub gives each agent
one member at launch and spreads the agents across the members. When a
session reports a usage limit, the hub marks that member as limited until its
reset. It then relaunches the agent on the same host with the next free
member. The new session continues from `session_history`, as after a move.
If every member is limited, the agent waits until the earliest reset.

Do not put a gateway in front for now. LiteLLM and OpenRouter cannot pool
subscription accounts, and Portkey is unproven. The tools that can (better-ccflare,
CLIProxyAPI) add an always-on service that holds every token and sits in the
path of every Claude request. LunaRoute needs no pool. It queues requests
above its concurrency limit and has no token cap to rotate around.

The smallest useful step is phase 1: members and a spread at launch, with no
limit detection. It makes a limit rarer with a small change.

## 1. Goals and non-goals

Goals:

1. A Claude Code agent that reaches a usage limit continues on another
   account within a few minutes, with no human step.
2. The agents use the accounts evenly, so that a limit comes later.
3. Token values never cross NATS. The hub stores and sends member names only.
4. When every account is limited, Borant waits until the earliest reset. It
   does not restart sessions in a loop.

Non-goals: a switch of account inside a running session, a gateway or proxy,
dollar budgets (observability doc, phase 4), LunaRoute pools, and the
interactive sessions that Chris starts on his laptops with `/login`.

## 2. Facts

### 2.1 Claude Code tokens

- `claude setup-token` makes a one-year OAuth token through the browser login
  flow. It needs a Pro, Max, Team, or Enterprise plan. Claude Code reads it
  from `CLAUDE_CODE_OAUTH_TOKEN` [S1].
- The token can only make model requests (scope `user:inference`), so it
  gets no structured `/usage` report [S1], [S2].
- `ANTHROPIC_API_KEY` wins over the token. In `-p` mode the key is always used
  when it is present [S1]. A stray API key in the env thus hides the pool.
- Unproven: whether one token works on many hosts at the same time, whether an
  account can have several tokens, and how to revoke one. The CLI has no list
  or revoke command [S3].

### 2.2 Claude usage limits

- Pro and Max have a 5-hour session limit and a weekly limit. Claude and
  Claude Code share them [S4]. Max also has a weekly limit for one model
  family [S4].
- Claude Code shows these messages [S5]:
  `You've hit your session limit · resets 3:45pm`,
  `You've hit your weekly limit · resets Mon 12:00am`, and
  `You've hit your Opus limit · resets 3:45pm`. At 85% it shows a warning.
- The `StopFailure` hook fires when a turn ends with an API error. Its matcher
  `rate_limit` selects usage limits. The input has `error`, `error_details`,
  and `last_assistant_message`. Claude Code ignores its output [S6].
- The status line input has `rate_limits.five_hour` and
  `rate_limits.seven_day`, each with `used_percentage` and `resets_at` in
  epoch seconds. They appear for Pro and Max only [S7].
- `--output-format stream-json` gives a `rate_limit_event` with
  `rate_limit_info.status` (`allowed`, `allowed_warning`, `rejected`) and
  `resetsAt`. The result has `is_error` and `api_error_status` (429) [S2].
  Two open bugs: `-p` can exit 1 on a weekly limit with no result at all
  [S8], and the retry watchdog can show a wrong "monthly spend limit" message
  [S9].
- Since v2.1.234, an interactive session waits for the reset and then
  continues by itself. This is on by default (`autoContinueAtUsageLimit`). It
  starts again at most twice in a row, and it does not start when the reset
  is more than 24 hours away [S10]. Borant launches interactive sessions in
  tmux, so this wait applies to them.
- Subscription responses carry `anthropic-ratelimit-unified-status` and
  `-reset` headers. They are not documented, but balancers read them [S11].

### 2.3 Pi, omp, and LunaRoute

- LunaRoute sells plans by the number of concurrent requests, from $19.99 a
  month for one to $299 for six [S13]. A request above the limit waits in a
  queue. The extension reads `x-lunaroute-queue-ms`, and `/lunaroute usage`
  shows the plan, the lanes, and the queued and rejected totals [S14]. No
  token budget was found. More capacity is a larger plan, not a second key.
- A Pi extension sees each provider response in `after_provider_response`,
  with `status` and `headers` [S15]. Pi retries 429 and 5xx errors with
  backoff before the turn fails [S16]. Unproven: that omp has the same event.

### 2.4 Proxies and gateways

| Tool | Pools | Pick rule | On 429 or limit | Claude Max OAuth | Cost for one person |
|---|---|---|---|---|---|
| LunaRoute (hosted) | Nothing, one `lr_` key | None | Queues the request [S14] | Not a Claude gateway | The plan price [S13] |
| LiteLLM proxy | API keys | Shuffle, least busy, usage, latency, cost, order [S17] | Cooldown, retries, fallbacks [S17] | Passes the token of each client. The docs say not to share one subscription token [S18] | Free, self-hosted, Postgres for budgets |
| claude-code-router | API keys, one local Claude login | Priority, weight, local limits [S19] | Retries and fallback models | One login only [S19] | Free, local |
| better-ccflare (fork of ccflare) | Claude OAuth accounts and API keys | One account for the 5-hour window, then the next, with priorities [S20] | Skips limited accounts, falls back when the reset passes [S20] | Yes | Free, self-hosted |
| claude-relay-service | Claude OAuth accounts, built for shared Max use | Priority, then least recently used, sticky for 1 hour [S21] | Waits for the reset of the limited window, capped at 1 hour [S21] | Yes | Free, Node and Redis |
| CLIProxyAPI | OAuth logins for Claude Code, Codex, Gemini, and keys | Round robin, weighted, fill first, optional session affinity [S22] | Retries on 429, cooldown per login [S22] | Yes | Free, Go binary |
| OpenRouter | Your provider API keys | Keys in order, then shared capacity [S23] | Falls back to shared capacity [S23] | No | 5% fee on your own keys [S23] |
| Portkey | Provider keys | Weighted, sticky [S24] | Fallback on chosen codes, for example 429 [S24] | Claims Max support. Unproven whether it pools [S25] | Free gateway, hosted from $49 a month |

Vercel AI Gateway and Tailscale Aperture pass the token of each user and do
not pool [S26]. Only the self-hosted Claude account balancers pool Max
accounts, and each one switches accounts per request.

## 3. Options

| | A. Pools in Borant, restart on limit | B. Account balancer in front | C. Both |
|---|---|---|---|
| What it is | Each agent gets one member at launch. A limit restarts the agent on the next member. | better-ccflare or CLIProxyAPI on one box. Claude Code points `ANTHROPIC_BASE_URL` at it. | B for live switches, A for the case where B is down |
| Work lost at a limit | The failed turn. The new session reads `session_history`. | Nothing. The next request goes to another account. | Nothing |
| Where the tokens live | The store, resolved per launch on each host | On the balancer box, all of them | Both |
| New moving parts | One hub rule, one hook, one KV key per member | One always-on service, its database, TLS, and updates | All of A and B |
| Failure of the new part | The hub is down: no relaunch, sessions wait | The balancer is down: every Claude session stops | Complex to reason about |
| Fit with the secrets doc | Bundles and resolver mode, values never in NATS | Breaks "the hub holds no secrets" if the balancer runs on the hub box | Mixed |
| Third-party code in the path of every request | None | Yes, with every token and every prompt | Yes |

Option A matches what Chris asked for ("restart a session on a new
account/auth-token when it hits its limit"), and it reuses the move path.
Option B gives a better result only when limits come often, because it saves
one turn per limit. Thus phase 3 measures the restarts before anyone builds
option B.

## 4. Recommended design

### 4.1 A pool is a bundle with members

A bundle in `host.toml` gets `members`. Each member is a name and a map of env
vars to refs, in the same shape as `vars`. Member vars win over the bundle
`vars`. The bundle name stays `claude-auth`, so the four-level merge and the
scheduler filter of the secrets doc do not change.

```toml
# host.toml
[secrets.bundles.claude-auth]
store = "op"
[secrets.bundles.claude-auth.members]
a = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-a/oauth-token" }
b = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-b/oauth-token" }
c = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-c/oauth-token" }

# hub.toml: rules only, no refs
[pools]
unknown_reset_mins = 60   # used when the limit gives no reset time
max_restarts_per_hour = 3 # per agent, then the agent waits
```

A flat list of refs is shorter, but the hub needs a name for each member
that is not a ref. A member name must mean the same account on every host.

Type changes, all with `#[serde(default)]`:

- `Bundle.members: BTreeMap<MemberName, BTreeMap<String, SecretRef>>`.
  `MemberName` is a safe-to-log string, like `BundleName`.
- `HostInfo.members: BTreeMap<BundleName, Vec<MemberName>>`, from `host.toml`.
- `LaunchOrder.members` and `Agent.members`:
  `BTreeMap<BundleName, MemberName>`, the member of each pool for this agent.
- The `secrets_issued` session event adds the member names.

The host resolves only the refs of the member in the `LaunchOrder`. If the
order names no member, or a member that the host does not define, the launch
fails with `invalid`.

### 4.2 Assignment per agent

The hub picks a member at each launch: spawn, move, takeover, and limit
restart. The rule is a pure function in `borant-core::rules::pools`, with
tests:

1. Keep only members that the target host defines and that are not limited
   now.
2. Pick the member with the fewest live agents.
3. On a tie, pick the member that was limited least recently, then the first
   name.

An agent keeps its member across moves while the member is free. Fill first
(one account until its limit) is the other choice (question 2).

### 4.3 Limit detection per harness

| Harness | Signal | Reset time | Status |
|---|---|---|---|
| Claude Code in tmux | `StopFailure` hook with matcher `rate_limit` [S6] | Parsed from `resets 3:45pm` or `resets Mon 12:00am` in `last_assistant_message`, in the host time zone | Hook documented. The text in the hook input is unproven. |
| Claude Code headless (`-p`) | `rate_limit_event` with status `rejected`, or a result with `api_error_status` 429 [S2] | `resetsAt` | Documented, with the bugs of section 2.2 |
| Pi and omp | `after_provider_response` with status 429, then a failed turn [S15] | `anthropic-ratelimit-unified-reset` or `retry-after` header | Not needed for LunaRoute. Cut until a pool uses it. |

Rules for Claude Code:

- The launched sessions set `autoContinueAtUsageLimit: false` in the managed
  settings that the seed unit writes. Otherwise the session waits in place
  and races the restart. Borant owns the wait instead.
- Only `rate_limit` counts. `overloaded` and `server_error` are not a problem
  of the account, so they start no restart.
- `authentication_failed` marks the member as broken. A broken member gets no
  agents until a human runs `borant pools clear <bundle> <member>`, and the
  hub sends a needs-human alert.
- The words in the message give the kind: `session limit`, `weekly limit`,
  or a model limit. If the parse fails, the reset is now plus
  `unknown_reset_mins`.

Later, a `borant statusline` command can send `rate_limits.*.resets_at` [S7]
to the host daemon, for an exact reset and a switch at 95% (phase 3).

### 4.4 The restart flow

The hub is the only writer. The host reports, the hub decides, and only
member names cross NATS.

1. The `StopFailure` hook runs `borant hook stop-failure`. The handler sends
   `LocalEvent::Limit { kind, resets_at, detail }` to the host daemon and
   exits 0. If the daemon is down, it exits 0 with no output.
2. The host daemon knows the member of the session from its `LaunchOrder`. It
   sends `CtlRequest::MemberLimited { agent, session, token, bundle, member,
   kind, resets_at }` to the hub. The `detail` text goes through `redact()`.
3. The hub writes the member state (section 4.5). A second report for a member
   that is already limited only updates the reset if it is later.
4. The hub runs the rule of section 4.2 for this agent on the same host.
5. If a member is free, the hub sends `PrepareMove` with the new reason
   `MoveReason::Limit`. The session is idle, because its turn ended, so the
   host writes the final checkpoint, stops the session, and sends `MoveReady`
   at once. The hub then sends `Launch` with the new member and this first
   prompt: "You continue agent X on another account after a usage limit. Read
   your history with `session_history`, then continue."
6. If no member is free, the hub keeps the session alive and idle, and
   records the agent as waiting. At the earliest reset, the hub tick acts:
   if the free member is the member of the agent, the hub sends a wake message
   to the same session ("The usage limit reset. Continue."), and nothing is
   lost. Otherwise it runs step 5 with that member.

Today `crates/borant-hub/src/moves.rs` skips a move to the current host when
the harness does not change. `MoveReason::Limit` must pass that test, and a
launch on the same host must reuse the worktree of the agent.

Loop guards:

- At most `max_restarts_per_hour` limit restarts per agent. After that, the
  agent waits for the earliest reset, and the hub sends one alert.
- If a member reports a limit within 10 minutes after the hub chose it as
  free, the hub doubles its wait each time, from `unknown_reset_mins` up to
  24 hours. This covers a reset time that is wrong.
- Other agents on a limited member keep their sessions until each one reports
  its own limit.

### 4.5 State

One hub KV key per member, `pool.{bundle}.{member}`:

```rust
pub struct MemberState {
    pub limited_until: Option<Timestamp>, // None means free
    pub last_limited: Option<Timestamp>,  // for the tie rule and the loop guard
    pub kind: Option<LimitKind>,          // Session, Weekly, Model, Unknown
    pub broken: bool,                     // authentication failed
}
```

The live agents per member come from `Agent.members` and the leases.
`borant pools` prints each member, its state, its reset, and its agents.
`borant pools clear` removes a limit or a broken mark. Each change makes a
swarm event (`pool.limited`, `pool.reset`, `pool.broken`).

## 5. Phased plan

Each phase needs phase 1 of the secrets doc (the port, bundles, and resolver
mode) first.

| Phase | Work | Done when |
|---|---|---|
| 0. Facts | On `borant-2`: two hosts on one token at the same time, a second token for one account, a change of token in the same `~/.claude`, the `StopFailure` input at a real limit with `autoContinueAtUsageLimit: false` | Each unproven item of section 2.1 and 4.3 has a result |
| 1. Members and spread | `Bundle.members`, `HostInfo.members`, `LaunchOrder.members`, `Agent.members`, the pick rule without limits, `borant pools`, `borant move <agent> --member <name>` for a manual switch | Three Claude Code agents land on three accounts, and no token shows in NATS |
| 2. Restart on limit | The `stop-failure` hook, `MemberLimited`, `MemberState`, `MoveReason::Limit`, the same-host relaunch, the wait and wake, the loop guards, `autoContinueAtUsageLimit: false`, alerts | A fake harness that reports a limit moves to the next member, and with all members limited it waits and wakes at the reset |
| 3. Measure | Count limit restarts per week. If they are frequent, decide on the status line switch at 95%, or a balancer (option B) | A decision record in this doc |

## 6. What this doc cut

- A balancer (option B) and a switch inside a running session, until phase 3
  shows a need.
- LunaRoute pools, `files` members, and Pi and omp limit detection.
- Limits per model family. Phase 2 treats each limit as a limit of the member.
- A move in advance of the other agents on a limited member, and a reserved
  account for Chris's own use (question 1).

## 7. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Claude Code changes the `StopFailure` matcher or the limit text | No restart. The session sits idle. | Pinned Claude Code, a fake-harness e2e test, and the stall alert of the observability doc |
| Every member reaches its weekly limit | Claude work stops for days | The alert at "all members limited", and question 3 |
| Chris uses an account by hand on a laptop | Borant does not see that use, and the limit comes early | Question 1 |
| A restart loses the native context and the prompt cache | The new session has less context and spends more usage | `session_history`, and the count in phase 3 |
| A wrong reset time | Restarts into a limited account | The 10-minute loop guard with doubling |
| A revoked or expired token | Launches fail on that member | The broken mark, the alert, and `borant doctor` |

## 8. Open questions for Chris

1. How many Max accounts go in the pool? Do you also use one of them by hand
   on a laptop? If yes, keep it out of the pool, or tell Borant which member
   your laptop login uses.
2. Spread (all accounts used evenly, the default here) or fill first (one
   account until its limit, the others stay fresh)?
3. When every account is limited, is waiting with an alert enough? Or do you
   want a paid fallback, an `ANTHROPIC_API_KEY` member that the hub uses
   last?
4. Is the loss of the failed turn and the native context at a restart
   acceptable? If not, option B (a balancer) is the alternative.
5. LunaRoute: do you see queueing only, and never a hard limit? If yes, it
   needs no pool.

## Sources

- [S1] Claude Code authentication: https://code.claude.com/docs/en/authentication
- [S2] Agent SDK TypeScript reference: https://code.claude.com/docs/en/agent-sdk/typescript
- [S3] Token list and revoke requests: https://github.com/anthropics/claude-code/issues/48373, https://github.com/anthropics/claude-code/issues/22995
- [S4] Claude Code with Pro or Max: https://support.claude.com/en/articles/11145838-using-claude-code-with-your-pro-or-max-plan
- [S5] Claude Code errors, usage limits: https://code.claude.com/docs/en/errors
- [S6] Claude Code hooks: https://code.claude.com/docs/en/hooks
- [S7] Claude Code status line: https://code.claude.com/docs/en/statusline
- [S8] `-p` exits on a weekly limit with no result: https://github.com/anthropics/claude-code/issues/103820. [S9] Retry watchdog message on Max: https://github.com/anthropics/claude-code/issues/104690
- [S10] Claude Code interactive mode, wait for a usage limit: https://code.claude.com/docs/en/interactive-mode
- [S11] ccflare header handling: https://github.com/snipeship/ccflare/blob/main/packages/providers/src/base.ts
- [S12] claude-relay-service: https://github.com/Wei-Shaw/claude-relay-service/blob/main/config/config.example.js
- [S13] LunaRoute: https://www.lunaroute.com
- [S14] LunaRoute Pi extension: https://github.com/lunaroute/lunaroute-pi-extension
- [S15] Pi extension types: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/extensions/types.ts
- [S16] Pi settings, retry: https://app.unpkg.com/@earendil-works/pi-coding-agent@0.83.0/files/docs/settings.md
- [S17] LiteLLM routing: https://docs.litellm.ai/docs/routing
- [S18] LiteLLM with Claude Max: https://docs.litellm.ai/docs/tutorials/claude_code_max_subscription
- [S19] claude-code-router: https://github.com/musistudio/claude-code-router
- [S20] better-ccflare: https://github.com/tombii/better-ccflare, https://github.com/tombii/better-ccflare/blob/main/docs/load-balancing.md
- [S21] claude-relay-service: https://github.com/Wei-Shaw/claude-relay-service, and [S12]
- [S22] CLIProxyAPI: https://github.com/router-for-me/CLIProxyAPI/blob/main/config.example.yaml
- [S23] OpenRouter BYOK: https://openrouter.ai/docs/guides/overview/auth/byok
- [S24] Portkey load balancing and fallbacks: https://portkey.ai/docs/product/ai-gateway/load-balancing, https://portkey.ai/docs/product/ai-gateway/fallbacks. [S25] Portkey with Claude Code: https://portkey.ai/docs/integrations/libraries/claude-code
- [S26] Vercel and Tailscale passthrough: https://vercel.com/changelog/claude-code-max-via-ai-gateway-available-now-for-claude-code, https://tailscale.com/docs/aperture/how-to/use-passthrough-mode/claude-subscriptions
- Local, read on 2026-10-25: `crates/borant-core/src/proto.rs`, `crates/borant-hub/src/moves.rs`, `crates/borant-host/src/commands.rs`, and `crates/borant-host/src/sessions.rs`.
