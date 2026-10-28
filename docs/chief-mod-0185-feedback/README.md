# Chief mod feedback on 0.18.5: the Needs-you tab

Real Claude Code 2.1.310 sessions in tmux with the fixture board, at 200x50
(the pane docked) and 100x40 (the pane above the prompt). Each "before" is
`main` at 668cd79 (v0.18.5). The steps: `1` opens the pane on Needs you, `j` selects
the second question, Enter opens it, esc goes back.

| Defect | Before | After |
|---|---|---|
| Done and Ask sat at column 0, level with the next task's ▸, with a blank row between a task and its own buttons | `before-1-needs-*.png` | `after-1-needs-*.png`: the buttons sit under the title text, right below it, and a blank row ends each group |
| Enter on a question changed only the breadcrumb: the list stayed, the cursor went to the first item, and the crumb named the second | `before-3-needs-enter-*.png` | `after-3-needs-enter-*.png`: Enter opens the task's detail, the crumb names it, and esc goes back to the list with the cursor where it was |
| A stray `esc` after `l: →` in the footer | `esc` alone, and "esc back" in the hint line | `esc: back`, drawn like the hotkeys; the hint line drops it |

The blank row came from the button row's own empty text line, which the pane
drew before the buttons. A button row now draws only its buttons.
