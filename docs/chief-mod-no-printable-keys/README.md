# The mod takes no printable key outside board mode

Chris (v0.19.2): "the 1-9 keys are unusable in the claude prompt right now".
A bare digit at the empty prompt opened board mode and was eaten.

Real Claude Code 2.1.310 in tmux at 200x50, chief role, the fixture board.
Keys are typed one at a time (a burst is a paste, which no hook takes).

| Step | Before (v0.19.2) | After |
|---|---|---|
| type `1. fix the bug` | prompt empty, board mode on: `before-typed-1.png` | prompt `1. fix the bug`, board mode off: `after-typed-1.png` |
| type `0 tasks` | prompt empty, board mode on: `before-typed-0.png` | prompt `0 tasks`, board mode off: `after-typed-0.png` |

Board mode is entered by a click on the band's `/cos pane: board mode` button
or a row, or by typing `/cos pane`. Checked in tmux: both enter it with the
prompt empty. A letter hotkey was tried (`b`): it fires only while the band
holds the focus (ctrl+x tab), and from there the pane cannot take the keys
(Claude Code API limit: no call returns the focus to the prompt).

`scripts/tui/cos-scenario.sh` now types both strings and checks them. At
200x50: 59 PASS; at 120x40: 52 PASS. The same 4 checks FAIL at both sizes:
the Done and Ask turns and the running-turn click. They need a model, and the
tmux session on this host has no login.
