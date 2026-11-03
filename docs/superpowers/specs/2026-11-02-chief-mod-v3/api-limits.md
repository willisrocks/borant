# What the Claude Code mod API allows (2.1.316)

Each fact below comes from the type docs of the `plugin-authoring` skill, from
a probe mod run in a real session at 120 and 320 columns on 2026-11-02, or
from both. The quotes are from the type docs.

## Keys at the prompt

| Fact | Source |
|---|---|
| The mod captures no printable key at the prompt. This is Chris's rule. | chief-personal, 2026-11-02 |
| A band Button with a digit `hotkey` takes that digit at an empty prompt: "never from the composer, save a bare digit in an empty one pressing a band Button". The band must have no digit hotkeys. | type docs; probe: `1` at the empty prompt pressed the band Button |
| A band Button with a letter `hotkey` gets nothing at the prompt. The letter goes to the prompt. | probe: `q` |
| A Button with `action: "app:cycleDiffBase"` is pressed by that action's chord from the prompt: "An engine keybinding action ... whose chord presses the Button from the prompt". The default chord is `ctrl+x b`. | type docs; probe: `ctrl+x b` pressed it, with an empty prompt and with text in it |
| The chord does not reach the mod "while an engine handler of that action is mounted": with the diff panel open, `ctrl+x b` cycles the diff base. | type docs |
| The mod cannot read which chord the person bound. A hint can only name the default, `ctrl+x b`. | no API in the types |
| `ctrl+x tab` (engine) gives the band the keys. Then its Button hotkeys work. | type docs |
| A slash command (`immediate: true`) that calls `$.ui.open({ focus: true })` opens the panel with the keys, because the prompt is empty after the command. This is the fallback when the diff panel holds `ctrl+x b`. | probe: `/probe` then `j a` reached the mod |

## Keys in the panel

| Fact | Source |
|---|---|
| `$.ui.open({ focus: true })` gives the pane the keys only "while the prompt has the keys over an empty composer". With text in the prompt, the pane opens without the keys. | type docs; probe |
| In a focused pane, a Button `hotkey` (one digit or one lowercase letter) presses it. Shift+letter is the same letter. | type docs; probe: `j a o 1` all reached the mod |
| A key that no Button holds goes to the prompt. | tui-visual-qa lessons |
| Tab and the arrows walk the pane's Buttons, and Enter presses the focused one. Tab can reach the engine's close mark; a `ui.focus` hook can deny that move. | type docs; tui-visual-qa lessons |
| A focused `Input` gets every printable key and Enter. A reply can be typed in the pane. | type docs; v0.19 board mode |
| With `closeOnEscape`, Esc gives the keys back and closes the pane. | type docs; probe |

## Sizes

| Fact | Source |
|---|---|
| The band gets the full column: `bodyColumns` is "its column's width less the engine's five at the right end". At 320 columns the band had 315. The v0.19.2 cut at about 60% is the mod's own cap, not an API limit. | type docs; probe |
| The band's height is at most `maxRows`: "in fullscreen, what the bottom slot has left above the prompt", "capped at half the terminal's rows". Probe: 24 rows at 60 rows, 19 at 50, 14 at 40. A taller tree scrolls. | type docs; probe |
| The pane docks beside the transcript in fullscreen from 110 columns, else it sits inline above the prompt. The mod cannot choose. | type docs |
| The dock is at most 45% of the terminal and at least 24 columns: 54 at 120, 90 at 200, 144 at 320. A width that the person dragged wins. | type docs; probe: 143 at 320, 53 at 120 |
| The dock is as tall as the transcript: about 56 rows at 60. | probe |
| With the dock open, the band shares the transcript column: 171 of 320, 61 of 120. | probe |
| A pane that the mod opens on its own (not after a key or command) is placed only from 144 columns (110 once the person opened it before). It can stay open without the keys. | type docs |

## Other abilities used by the designs

| Fact | Source |
|---|---|
| `$.prompt.fill({ text })` puts a draft in the prompt. | type docs |
| `PromptHint` can add a dim `tail` to the footer line under the prompt, so the hotkey can show there too. | type docs |
| `$.process.run` runs `borant` and `open <url>` (a PR in the browser). | type docs; v0.19 mod |
| `$.store` keeps values across sessions: the "last looked" time per item. | type docs |
| `$.ui.toast` shows a short notice. | type docs; v0.19 mod |
