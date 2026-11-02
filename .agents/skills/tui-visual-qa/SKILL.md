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
   narrow size (100x40). For the mod, that is Claude Code with the branch's
   plugin (`--plugin-dir integrations/claude-code`). Use the helpers in
   `scripts/tui/lib.sh`: `tui_start`, `tui_keys`, `tui_type`, `tui_click`,
   `tui_drag`, `tui_find`, `tui_column`, `tui_shot`, `tui_stop`.
2. Drive it with real input, as a person would: keys, chords with a human
   pause (1 s between `ctrl+x` and `tab`), mouse clicks, and drags.
3. After each step, render a PNG (`tui_shot`) and Read it. Judge it against
   the goal of the change, not against "it rendered". Compare it with the
   step before: a focus move must change what the screen shows.
4. Fix, and run the same steps again.

`just cos-visual` runs the basic mod states at both sizes.
`just cos-scenario [plugin] [out] [width] [height] [board]` runs the full
interaction scenario at one size, on the `big` board (12 projects, 6 need you)
or the `fixture`. It drives board mode with a PNG after every key: 0, each
digit, j and k, a live board update in the middle of navigation, Enter and
the details, ?, Done (closed and still open), Ask, Esc, typing at the prompt
after Esc, Tab and Shift+Tab, and vim insert and normal mode. Then the
refresh time with an event, an unavailable board, a resize, the empty board,
and `/cos help`. Each run writes `<out>/<width>/NN-<state>.png` and
`log.txt`, so runs at several sizes can share one `<out>`. A log line that
starts with FAIL is a defect: a key that reached the prompt in board mode, a
key that did not reach it after Esc, a column that moved across the live
update (`scripts/tui/columns.mjs` compares the plain captures), or a slow
refresh. LIMIT names what the mod API cannot do. Read the log and the PNGs.
Where `freeze` is not installed (a Mac without nix), `tui_shot` renders the
PNG with headless Chrome in a profile of its own; run the script directly.

## Safety

A Claude Code session under test is a real session on a machine where real
agents run. Start it with `"${TUI_CLAUDE_FLAGS[@]}"` from lib.sh:
- **Default permission mode:** the model asks before it acts on a prompt the
  mod submits.
- **SendMessage and ListAgents disallowed:** Claude Code lets a session list
  and message the other sessions on the machine, and removing variables does
  not hide them.

`tui_start` drops your `BORANT_*` and `CLAUDE*` variables, so the session
never registers as you or sees your credentials. Pass what it needs inside
the command: `tui_start name 200 50 env PATH=... BORANT_ROLE=chief claude ...`.
Kill only the tmux servers that you started.

## Realistic states

- **Data:** the fixture (`BORANT_COS_BOARD=fixture`), or a live board served
  by `scripts/tui/borant-stub` (`BORANT_COS_BOARD=cli`, the stub's folder
  first on PATH). Change the stub's files during the run to test refresh.
  Try long names, 10+ rows, and empty states.
- **Waits:** shorten them for a run. `BORANT_COS_DONE_WAIT_S=20` makes
  "still open" show after 20 s.
- **Host chrome:** Claude Code's footer chips (a background shell, an
  artifact) take arrow keys and space. Run the steps with them present.
- **Pane placement:** at 110 columns and up the pane docks beside the
  transcript, and narrower it sits above the prompt. Claude Code remembers a
  width the person dragged, across sessions.
- **The person's terminal is not tmux.** tmux cannot show the person's font,
  colors, or key encoding. Say what you could not check, and ask the person.

## Checklist for each change

- Contrast is readable in the selected and unselected states. Bold black
  shows as grey in many terminals.
- No text runs under another column. A cut ends in "…".
- Focus is visible as soon as a site takes the keyboard, and it moves where
  the person expects after a digit, Enter, and a click.
- Every action gives feedback at once ("marking done…"), and the result
  shows when it lands, or an error shows if it does not.
- Refresh time is measured, with numbers, and the header shows the data's age.
- The keys that the hints name really reach the mod with chips present.

## Evidence

Attach before and after PNGs of each state to the PR. Make the "before"
set from a worktree of `main` (`git worktree add <dir> main`, then pass its
`integrations/claude-code`). `tui_shot` takes a shot again when freeze cuts
it short. If it still warns "no full PNG", do not use that PNG. Where Claude
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
- A mod's own `$.ui.focus` does not come back through its `ui.focus` hook.
  Record the move yourself.
- A focused pane or band gives a key to the mod only when a Button has it as
  its hotkey. Any other letter goes to the prompt. A focused `Input` gets
  every printable key, the arrows, and Enter: board mode is a pane whose only
  focusable element is an Input.
- Tab in a focused pane moves the ring to the pane's close mark, and a letter
  then goes to the prompt. Deny that move in a `ui.focus` hook.
- Claude Code 2.1.316 wraps a prompt that a mod submits in words of its own,
  before and after it. Match a submitted prompt in `turn.start` by
  `includes`, not by equality.
- In vim normal mode a bare digit goes to vim, and no event reaches a mod.
- An empty prompt holds a no-break space or a dim suggestion. A check for
  "nothing typed" looks for the key it typed.
