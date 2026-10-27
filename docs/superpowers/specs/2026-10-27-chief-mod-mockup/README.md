# Chief-of-staff mod v0.0.1: the approved mockup

Chris approved this mockup on 2026-10-26 (through chief-personal). The source
is the design canvas "Chief of staff mod"
(https://claude.ai/artifact/3XaQLm6jZWha9vrNhYARqK). Each `.dc.html` file is
one artboard, a static drawing of a Claude Code terminal at the given width.
Open one in a browser to see it, or read the markup.

| File | Surface |
|---|---|
| `Main.dc.html` | 1 · The band above the prompt, expanded: one row per project, plus a "Needs you" row |
| `BandCollapsed.dc.html` | 2 · The band collapsed to one row, with its other states |
| `Pane.dc.html` | 3 · The pane beside the transcript on a wide terminal, with tabs: Needs you, Projects, Feed |
| `Narrow.dc.html` | 4 · A narrow terminal: band, toast, and a status line, with no pane |
| `Transcript.dc.html` | 5 · Borant reports as compact transcript rows, and the `/cos` digest |

The mod reads its data from the hub's `board` call (spec
`2026-10-26-sagas-and-epics-design.md`, "API, MCP, and CLI").
