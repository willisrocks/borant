# Research: message delivery into each harness

Date: 2026-10-23. Research by subagents and by hand. Items marked "unverified"
were not confirmed.

## Versions on the research machine

| Harness | Version |
|---|---|
| Claude Code | 2.1.310 |
| Pi (`@earendil-works/pi-coding-agent`) | 1.4.0 |
| omp (oh-my-pi) | 18.12.6 |
| Codex CLI | 0.163.0 (upstream 0.164.0) |

## Summary

| Harness | Busy, inside a turn | About to stop | Idle at the prompt |
|---|---|---|---|
| Claude Code | `PostToolUse` returns `hookSpecificOutput.additionalContext` | `Stop` returns `{"decision":"block","reason":...}` | `asyncRewake` hook that exits with code 2 (see the spike) |
| Pi | Extension sends a steer message | Extension adds messages at `turn_end` | `pi.sendUserMessage()` starts a turn |
| omp | Same as Pi, plus `deliverAs: "aside"` | Same as Pi | Same as Pi |
| Codex | `PostToolUse` hook, or app-server `turn/steer` | `Stop` block makes a continuation prompt | `codex queue --thread <id>` (needs the shared Codex daemon) |

## Claude Code

- Hooks: `additionalContext` works on `SessionStart`, `UserPromptSubmit`,
  `PreToolUse`, `PostToolUse`, `Stop`, and others. Values over 10,000
  characters go to a file, and Claude gets the path and a preview.
- `Stop`: `stop_hook_active` guards against loops. Claude Code caps
  consecutive stop-hook continuations at 8 (`CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`).
- `asyncRewake`: runs in the background and wakes Claude on exit code 2. The
  stderr (or stdout) shows as a system reminder. Claude Code enforces
  `timeout` on it. Source: https://code.claude.com/docs/en/hooks.md
- `SessionEnd`: default timeout 1.5 seconds, up to 60 seconds for each hook.
- Channels (`claude/channel` MCP capability) can push into a live session, but
  a custom channel needs `--dangerously-load-development-channels` and a dialog
  at each launch. Research preview. stdio only.
- Built-in cross-session messaging (v2.1.224+) works only between Claude Code
  sessions on one machine. The socket wire format is not documented.
- A plugin can bundle hooks and an MCP server.

## Pi and omp

- One extension file can serve both. omp keeps the Pi extension API under the
  `@oh-my-pi/pi-coding-agent` package name. Orca already ships one file for
  both (`orca-agent-status.ts`).
- Open connections in `session_start` and close them in `session_shutdown`.
  `session_shutdown` also fires on `/new`, `/resume`, `/fork`, and reload, so
  the extension must reconnect on each `session_start`.
- Pi: `pi.sendUserMessage(content, { deliverAs })` starts a turn when idle.
  When busy without `deliverAs`, it throws and the message is lost. Always
  pass `deliverAs` or check `ctx.isIdle()` first.
- omp: leave out `deliverAs` to wake now. `deliverAs: "aside"` injects at the
  next step boundary. `attribution: "agent"` marks relayed messages.
- omp re-binds extensions in each subagent session. Connect only when
  `ctx.agent.kind === "main"`.
- omp `session_shutdown` handlers get 2 seconds.
- Session identity: `ctx.sessionManager.getSessionId()`, `getSessionFile()`,
  `ctx.cwd`.
- Both register custom tools with `pi.registerTool()`, so the extension needs
  no MCP.

## Codex (bonus)

- Hooks are stable since 0.124.0. Events include `SessionStart`, `PostToolUse`,
  `Stop`, and `SessionEnd` (0.145). Async hooks do not wake an idle session.
- `codex queue --thread <id> --message <text>` (0.149.0+) starts a turn in an
  idle TUI that uses the shared Codex daemon. The `thread/queue/*` RPCs are
  experimental.
- Orca gives Codex its own `CODEX_HOME`, and there the shared daemon was off.
- MCP notifications do not reach the model.
- `CODEX_THREAD_ID` is set in tool shells, not in hook env (unverified).

## Prior art

- Omnara (github.com/omnara-ai/omnara): own agent loop on stateless workers,
  Postgres event log, a daemon on each machine that connects out.
- Pi Durable (`@earendil-works/pi-durable`): durable Pi harness library with
  pluggable storage, task trees, and reset with a handoff note. Experimental.
  One process owns a storage at a time.
- Vercel durable-harness (github.com/vercel-labs/durable-harness): runs
  harnesses as workflow runs. Claude Code and Codex run only inside a Vercel
  Sandbox.
- MCP Agent Mail: local HTTP MCP server with inboxes for coding agents. Not
  studied in detail.
- Kata: task tracker with claim, `work.attention`, `kata wait` for fan-out and
  join. The task model of Borant follows it.
