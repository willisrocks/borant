# Chief mod v3

The chief mod v3 is the band above the chief's prompt and the panel beside it.
Chris looks at it instead of asking the chief "what's going on". The design and
its five review rounds are in `docs/superpowers/specs/2026-11-02-chief-mod-v3/`.

## Keys

At the prompt, the mod takes no key except one chord:

| Key | What it does |
|---|---|
| `ctrl+x b` | Gives Borant the keys: the panel opens, and its keys work |
| `/borant` | The same, typed as a command. It always works |
| `/borant help` | Prints every key |

`ctrl+x b` is the default chord of the Claude Code action `app:cycleDiffBase`.
The mod's band Button names that action, so Claude Code presses the Button when
you type the chord. Two cases make the chord do something else:

- While the diff panel is open, the chord cycles the diff base, as it always did.
- If you bound `app:cycleDiffBase` to another chord in `~/.claude/keybindings.json`,
  your chord opens the panel, but the band and the footer still say `ctrl+x b`.
  The mod cannot read your key bindings.

In both cases, use `/borant`.

Claude Code gives the panel the keys only over an empty prompt. With text in the
prompt, the panel opens and says that the keys stay on the prompt.

## Evidence

The videos and the full evidence are assets of the pre-release
[`chief-mod-v3-media`](https://github.com/willisrocks/borant/releases/tag/chief-mod-v3-media),
so they stay out of the repository's history:

| Asset | What it holds |
|---|---|
| `flows-120.mp4`, `flows-200.mp4`, `flows-320.mp4` | The whole flow in a real Claude Code 2.1.316 session at each width. The pane was captured every 200 ms, and each frame shows as long as it was on screen |
| `flows-*.gif` | The same videos, smaller |
| `chief-mod-v3-evidence.zip` | Every still, the plain-text capture of every step, the PASS/FAIL logs, the before stills, and the design rounds' mockups |

`stills/` holds ten stills. `after-*` comes from the final run, and `before-*` from the
v0.19.3 mod on the same sample swarm.

To record again, run `scripts/tui/cos-scenario.sh` with `RECORD=1`, for
example `RECORD=1 scripts/tui/cos-scenario.sh integrations/claude-code /tmp/cos 200 50`.
Start the session in a folder that Claude Code already trusts (`TUI_CWD`). The
scenario never answers a trust dialog. It runs 28 PASS/FAIL checks, and each width
had 0 FAIL.
