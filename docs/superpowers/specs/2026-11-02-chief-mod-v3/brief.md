# Brief for judges (no designer preference included)

## Who uses it
One person, Chris, runs about 10 projects in 2 collections with dozens of
coding agents (orchestrators and workers). He jumps between projects all day
and must make sure that nothing gets dropped. The mod is drawn inside his
"chief" Claude Code session (a chat with a chief-of-staff agent). His screen
is about 2560 px wide (about 320 terminal columns), but he also uses 200 and
120 columns.

His questions, moment to moment:
1. What needs me now?
2. What changed since I last looked?
3. What is stuck or silent?
4. What is each project doing right now, task by task, and who is on it?
5. What will ship next?
6. How do I act (answer, approve, ask the orchestrator, open the PR, jump to
   the agent) in one or two keys?

## His verdict on the current version (v0.19.2), in his words
- "this borant mod design still sucks. i can't see what tasks are open, the
  side panel hardly tells me anything useful, and the keybinds are still
  horseshit bad"
- "what the fuck is 'board mode'?"
- "why the flying fuck is the right part cut-off with my huge window?"
- "why don't we show the hotkey combo anywhere?"
- "it's no where near good enough to be remotely useful to me. i'm better
  off just asking the agent every once in a while to tell me what's going on."
- Later: "the 1-9 keys are unusable in the claude prompt right now". Hard
  rule: at the prompt the mod captures no printable key at all. Only an
  explicit hotkey gives the mod the keys.

What he saw in v0.19.2: band rows like "1: ▲ E2E: capture … ✓ asked",
"2: ▲ boopmark 3 open tasks ●1 ■1 ▲1 orch-boopmark-2 · busy · borant…". The
side panel for a project showed only "orch-boopmark-2 · busy · borant-1" and
"tasks 2 running · 1 open · 1 blocked · 23 done" over a tall empty pane.

## The surfaces
- The band: rows directly above the prompt, always visible.
- The panel: a pane docked at the right of the transcript (fullscreen, 110+
  columns), or inline above the prompt when narrower.
- The prompt footer hint line.
What each can and cannot do is in api-limits.md in this folder.
