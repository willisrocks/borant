# Spike: wake an idle Claude Code session with an asyncRewake hook

Date: 2026-10-23. Claude Code 2.1.310 on macOS. Throwaway code.

## Question

Can a hook start a new turn in an idle interactive Claude Code session when a
message arrives from outside the session, with no special launch flag?

## Setup

A `Stop` hook with `"asyncRewake": true` runs `wait-msg.sh` in the background.
The script polls an `inbox/` folder. When a `.msg` file appears, the script
prints the message to stderr and exits with code 2. The session ran in tmux
with `claude --model haiku`. See `settings.json` and `wait-msg.sh`.

## Results

| Case | Result |
|---|---|
| Message while the session is idle | The session woke in about 2 seconds and answered. The next `Stop` started a new waiter. |
| Message while the user has an unsent draft | The session woke. The draft stayed in the input box. |
| Message during a busy turn | An older waiter delivered it mid-turn. Claude answered in the same turn. |
| User prompt while a waiter runs | Waiters pile up. Each `Stop` starts one more waiter, and the old one keeps running. |
| Waiter passes its `timeout` | Claude Code kills the waiter with no message. Later messages do not wake the session. |

The message shows in the session as "Stop hook feedback".

## Consequences for the design

- The hook must kill the earlier waiter for the same session before it waits.
- The waiter needs a long `timeout`. A second test on 2026-10-24 used
  `"timeout": 604800` (one week). The waiter was still alive after 62 minutes,
  past 3600 seconds, and its message woke the session.
- The hook that delivers a message must log it, because the message does not
  pass through `UserPromptSubmit`.

## Hook input (Stop)

The `Stop` hook received `session_id`, `transcript_path`, `cwd`,
`permission_mode`, `hook_event_name`, `stop_hook_active`,
`last_assistant_message`, `background_tasks`, and `session_crons`.
