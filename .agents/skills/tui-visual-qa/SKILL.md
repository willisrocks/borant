---
name: tui-visual-qa
description: Use when changing the Claude Code chief mod (integrations/claude-code/hooks, mod/) or any Borant TUI, or when checking how one looks or reacts to keys, mouse, focus, or refresh.
---

# TUI visual QA

You must see the screen. Unit tests say what a tree holds, and a plain text
capture says which characters are where. Neither shows color, the selected
row, the focus ring, text under another column, a cut name, or a key that
Claude Code takes before the mod gets it. Every one of Chris's bug reports on
the mod so far was one of these.

## The loop

1. Start the real program in tmux at fixed sizes: 200x50, 120x40, and one
   narrow size (100 columns). For the mod, that is Claude Code with the
   branch's plugin (`--plugin-dir integrations/claude-code`).
   `scripts/tui/lib.sh` has the helpers: `tui_start`, `tui_keys`,
   `tui_type`, `tui_click`, `tui_drag`, `tui_find`, `tui_shot`. It strips
   your `BORANT_*` and `CLAUDE*` variables from the program it starts.
2. Drive it with real input, as a person would: keys, chords with a human
   pause (1 s between `ctrl+x` and `tab`), mouse clicks, and drags.
3. After each step, render a PNG (`tui_shot`) and Read it. Judge it against
   the goal of the change, not against "it rendered".
4. Fix, and run the same steps again.

`just cos-visual` runs the basic mod states at both sizes.
`just cos-scenario` runs the full interaction scenario: footer chips, the band
keys, digit and `0` focus, a pane drag and resize, a Done click, and the
refresh time with and without an event. Read its `log.txt` and its PNGs.

## Realistic states

- Data: the fixture (`BORANT_COS_BOARD=fixture`) or a live board served by
  `scripts/tui/borant-stub`. Change the stub's files during the run to test
  refresh. Try long names, 10+ rows, and empty states.
- Host chrome: Claude Code's footer chips (a background shell, an artifact)
  take arrow keys and space. Run the steps with them present.
- The person's terminal is not tmux. Note what tmux cannot show you, and ask
  the person to check that.

## Checklist for each change

- Contrast is readable in the selected and unselected states.
- No text runs under another column. A cut ends in "…".
- Focus is visible and moves where the person expects, including after a
  digit, Enter, and a click.
- Every action gives feedback at once ("marking done…"), and the result
  shows when it lands, or an error shows if it does not.
- Refresh time is measured, with numbers, and the header shows the data's age.
- The keys that the hints name really reach the mod with chips present.

## Evidence

Attach before and after PNGs of each state to the PR. Make the "before"
set from a worktree of `main` (`git worktree add <dir> main`). Where Claude
Code's mod API cannot do something, say which limit, quote the type docs
(`plugin-authoring` skill), and give the closest option. Do not guess.

## Lessons so far

- j/k in the band did nothing: the band had no j/k keys, and the hint said
  it had. Every key that a hint names must be the band's own Button hotkey.
- Down from an empty prompt goes to the footer chips, not the band.
- `ctrl+x tab` is a chord with a fixed 3 s timeout in Claude Code. A pause
  breaks it. Give a single-key way (`0`, the digits) as well.
- Done gave no feedback, and the chief could not close the task, because only
  a channel's owner or a person may close it as done. Feedback needs the full
  path: the press, the prompt, the hub, and the refresh.
- A digit opened the pane but left the keys in the band. Ask for focus again
  until the pane's own drawing says it has the keyboard.
