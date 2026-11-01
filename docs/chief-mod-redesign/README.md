# Chief mod redesign: before and after

Real Claude Code 2.1.316 sessions in tmux, with a live board from
`scripts/tui/borant-stub` and a "1 shell" chip in the footer.

- Before: `main` at 98bfcc8, driven by its own `scripts/tui/cos-scenario.sh`
  on the fixture board. The 12-project shots of the band and the Needs-you tab
  come from a run on main of the same mod code.
- After: this branch, driven by the new `scripts/tui/cos-scenario.sh` on the
  12-project board (`scripts/tui/board-variants.mjs big`), at 200x50, 100x40,
  and 110x40, where the docked preview is narrow.

The PNGs are not from `freeze` or `pngquant`, which are not installed on the
laptop that made them. They come from tmux `capture-pane -e`, an ANSI-to-HTML
step, and a browser screenshot. tmux draws 256 colors, so a person's terminal
can show the greys differently.

## 200 columns

| State | Before | After |
|---|---|---|
| The band at rest | ![](before-200-band.png) ![](before-200-band-big.png) | ![](after-200-band.png) |
| ctrl+x tab, then j | ![](before-200-band-j.png) | ![](after-200-band-j.png) |
| A digit at the empty prompt | ![](before-200-digit.png) | ![](after-200-digit.png) |
| Done and Ask on an item | ![](before-200-needs.png) | ![](after-200-done-closing.png) |
| Done, closed by the hub | | ![](after-200-done-closed.png) |
| Ask sent | | ![](after-200-ask.png) |
| A refresh from an event | | ![](after-200-reading.png) |
| Done, still open after the wait | | ![](after-200-still-open.png) |
| `/cos help` | ![](before-200-help.png) | ![](after-200-help.png) |

## 100 columns

| State | Before | After |
|---|---|---|
| The band at rest | | ![](after-100-band.png) |
| ctrl+x tab, then j | ![](before-100-band-j.png) | ![](after-100-band-j.png) |
| A digit at the empty prompt | ![](before-100-pane.png) | ![](after-100-digit.png) |
| Ask sent | | ![](after-100-ask.png) |
| Done, still open after the wait | | ![](after-100-still-open.png) |
| `/cos help` | | ![](after-100-help.png) |

## 110 columns: a narrow dock

| State | After |
|---|---|
| A digit, then j: the compact preview in a narrow dock | ![](after-110-narrow-dock.png) |
| Esc out of the band: it lets the keys go | ![](after-110-band-esc.png) |
