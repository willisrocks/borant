# Chief delegation: evidence

The checks ran in real Claude Code 2.1.310 sessions in tmux, with `BORANT_ROLE=chief`. They used the
stub `borant` (`scripts/tui/borant-stub`), so no message left the box.

| Shot | What it shows |
|---|---|
| `fresh-rule-and-refusal.png` | A new chief session quotes the rule from its system prompt. Asked to run `./steve-688.sh inspect`, it refuses and offers to send it to the orchestrator. |
| `ask-sent-to-owner.png` | Ask orchestrator: the stub logged one `borant send orchestrator …`, with a toast and an "asked orchestrator ·" mark on the row. No prompt is submitted. |
| `ask-chief-told.png` | The chief knows the board sent the question, and knows not to answer it itself. |
| `old-mod-no-guard.png` | A session on v0.18.5 (`main`). `npm --version` is pre-allowed and runs with no prompt. |
| `reload-rule.png` | The same session after `/reload-plugins` onto this branch. It quotes the rule, which came as a row with the reload; its system prompt stays as it started. |
| `reload-guard-prompt.png` | The same session: the pre-allowed `npm --version` now gets a permission prompt with the reason "Chief guard: …". |

What I learned while testing:

- A running session keeps the system prompt it started with. A reload therefore delivers the rule as a
  conversation row (`$.session.append`), and the mod sends that row again after each compaction.
- `/reload-plugins` loads the module without raising `session.start` at once. The mod now starts
  itself from the first hook that needs it.
