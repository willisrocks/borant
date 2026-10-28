# Chief mod feedback on 0.18.4

Real Claude Code 2.1.310 sessions in tmux, with the fixture board
(`BORANT_COS_BOARD=fixture`), driven with the `tui-visual-qa` loop. Each
"before" is `main` at 57a3aa8 with the same fixture data.

| Defect | Before | After |
|---|---|---|
| A narrow pane dropped the Help tab without "…" | `before-tabs-narrow.png`: only `[ Needs you (2) ]` | `after-tabs-narrow.png`: `Needs 2 · [Proj] · Help` |
| No mark on the open tab | `before-tabs-wide.png`: all three look the same | `after-tabs-wide.png`: the open tab is the primary color; in the compact form it is bracketed |
| The selected pane row moved two columns | the `▌ ` prefix pushed the row's text right | the mark takes the gap cell after `▸`; the text stays put |
| Idle projects filled the band, `/cos`, and the Projects tab | `before-1-band-200.png`, `before-2-digest-200.png`, `before-3-projects-tab-200.png`, `before-band-100.png`: boopmark, all done, has a row; the header says `3 projects` | `after-*`: only borant and notes; the header says `2 active · 1 idle`; the tab ends with `1 idle project, no open work` |
| A project with plain tasks only showed `no saga ○ no epic yet` and an empty bar | notes: `no saga  ○ no epic yet  —` | notes: `plain tasks  ● 3 open tasks`, with no bar |

`/cos boopmark` still shows an idle project. The board names no channel for
a project's plain tasks, so a row cannot name the top plain task; it shows
the count of open ones (open, running, and waiting on a person).
