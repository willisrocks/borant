# Chief mod: feedback on 0.18.3, before and after

Captures from `scripts/tui/cos-scenario.sh` (`just cos-scenario`), run on
`main` (before) and on the fix (after) in Claude Code 2.1.310, at 200x50 and
120x40, with a background shell chip in the footer and a live board served by
`scripts/tui/borant-stub`. Each size's `log.txt` has the key tests, the pane
edge, and the refresh times. The 100x40 logs are here too. The PNGs are
compressed with pngquant.

| State | What it shows |
|---|---|
| 02-band-focused | ctrl+x, a 1 s pause, tab: the focus ring |
| 04-band-jj | j j in the focused band |
| 05a-band-enter-j | Enter on a band row, then j |
| 07-digit3-j | 3 from an empty prompt, then j |
| 08-zero-j | 0 from an empty prompt, then j |
| 09-dragged, 10-ctrlx-left | the pane edge dragged 15 columns right, then ctrl+x left |
| 12-done-clicked, 13-done-closed | a click on Done, then the hub closes the task |
| 14-refreshed | a board change shown |
| after/200/16-still-open | a Done the hub did not close (wait shortened to 20 s) |
| after/200/17-help | `/cos help` |
