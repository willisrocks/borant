# Sol log

Sol is `gpt-6.1-sol` at effort high, asked with the ask-a-friend skill from
the borant repo. Each round gave Sol `brief.md`, `api-limits.md`, and the
round's screens, and no designer preference.

## Round 1 (conversation codex-309)

Directions: A triage list band, B project table band, C one-line band with a
big docked panel.

| Direction | Sol liked | Sol rejected |
|---|---|---|
| A triage list | Names the request, project, consequence, and age. The focused pane with the full question. | `PROJECTS borant 2 working, 1 PR` repeats the counts of v0.19.2. A-320 adds nothing over A-200 and wastes width. A-120 drops the worker. The first item is not the most urgent, and nothing says why. `d mark done` is ambiguous. |
| B project table | The strongest base. The focused project pane: `OPEN, NOBODY ON IT` answers "I can't see what tasks are open". Dependencies (`after Hub backup`). | Tasks packed into one sentence per project. `#612 merged` under "ships next". PR numbers without names. Counts that disagree (6 vs 8). `saga 2/5`, `gate`, `review?` are shorthand. |
| C panel first | The 320 panel as an overview, with named shipping candidates. | Counts only at 120 and 200. `next:` gives no reason. View digits that look active at rest. The question cut off before its choices. `KEYS ON BORAN…` cut off. |

API problems Sol found:
- `ctrl+x b` with text in the prompt cannot give the panel the keys. The captions promised that.
- With the diff panel open, the chord goes to the diff panel. No fallback was shown.
- The hint can name only the default chord.
- The chord presses a Button; it does not focus the band. A and B showed band navigation that the API cannot give.
- B showed `↑↓ project` and `↑↓ task` at the same time with one focus owner.
- A-120 had 15 band rows; the measured limit is 14.

What changed for round 2:
- One focus owner: the panel. The band is read-only at rest and mirrors the panel's selection.
- Needs-you rows name the full ask, why it is urgent, and order by impact.
- Project rows name their tasks, with who and how long, plus open and unassigned work.
- "Ships next" holds only future work, by name. Merged work moves to "changed".
- Counts equal the rows that they count. "Since your last look" is defined: since the keys last left the panel.
- Separate verbs: answer, approve, close as done.
- New states: text in the prompt, the diff-panel fallback (`/borant`), a reply typed in the panel.

## Round 2 (codex-309, reply)

| Choice | Sol's pick | Reason |
|---|---|---|
| 320 at rest: wide band vs panel docked at rest | Wide band | The docked panel repeats the band's triage and changes. The wide band shows it in 11 rows against 18. Bring "ships next" into the wide band. |
| Panel: drill-down vs list and detail side by side | Side by side where the dock is wide enough, drill-down where it is not | Reading the history as the selection moves helps a decision without Enter. Decide by the panel's real width, because a dragged dock can be narrow. |

Sol liked: one focus owner; the project screen (all 7 unfinished tasks with state, owner, and what each waits on); the honest "keys stay on the prompt" screen; the orchestrator's recommendation in the history.

Sol rejected:
- The band's next PR for borant (`#203 CI running`) disagreed with the panel (`#204`, closer to merge).
- At 120, `roger-docs-a…` twice: two projects that cannot be told apart. 16 band rows over the 14-row limit. Two lines 121 cells wide.
- `○ 3 ready, 1 later` still hides the names of open tasks. `○ none open` beside a running task.
- "Preprod QA" drawn as ready while its spawn is blocked.
- The approval story contradicts a running epic of the same saga.
- `worker waits` then `in progress since 13:52`: the current state must come first.
- `orch-boopmark` in one place and `orch-boopmark-2` in another.
- `esc back to the list`: with `closeOnEscape`, Esc closes the pane. `? keys`: `?` is not an allowed hotkey.
- `/borant` only in a caption, never on screen.
- Command syntax (`borant answer boopmark/E2E-login`) in the reply hint.

What changed for round 3:
- Every count and every "next PR" comes from the data module. A task that cannot start is "blocked", not "ready".
- 120 columns: 14 band rows, distinct project names, silent work folded into its project row.
- Project rows name their open tasks.
- Keys: `b` goes back, `h` shows all keys, Esc gives the keys back and closes the panel. `/borant` shows next to `ctrl+x b`.
- The panel is a list plus the detail of the selected row: side by side from 120 panel cells, list above detail below that.
- "Changed since" holds until the person presses `x` (mark seen) in the panel. Leaving the panel marks nothing seen.
- New screens: approve with its result, an answer with its result, a blocked spawn with its fixes, the help, and 120 columns with the keys.

## Round 3 (codex-309, reply)

Sol's answer to "would he look at this instead of asking the chief": not yet. "Chris can use this for triage, but the single blocker is contradictory current state after an action succeeds."

Sol liked: actual decisions and urgency visible; the spawn detail with concrete fixes; `x` gives "seen" a clear meaning; the project view (5 for question 4); counts verified against data.mjs (4 requests, 1 silent and 1 offline, 5 changes, 8 projects, 20 agents, PR order #2440, #204, #203, #88); all widths and band heights in their limits.

Sol rejected:
- After an answer: `✓ answered 14:48 · wkr-boop-1 took it up 14:49` next to `NEEDS YOUR ANSWER` and `4 need you`. After an approval: the same.
- `orchestrator starts epics 3 to 5`: closing the gate does not prove that work started.
- At 320, `▲ Approve epics 3-5 orchestrator 42m +3`: the running tasks hidden behind `+3` on a huge screen.
- `no open task` beside a running task. Ready and later with the same mark.
- At 120, the orchestrator's recommendation cut off with no sign that more exists. The question repeated three times in the detail.
- `⇡ ship n…` cut four times.
- "Any other key cancels" and "any key closes": an unbound key goes to the prompt. `h all keys` shown while an Input has the keys.
- Approval "epics 3 to 5" vs "2 epics wait"; "2 of 5 done" vs "done or running". PR checks and review text not from the data. "more" marks that count headings.

What changed for round 4:
- A finished action is a receipt row, not a request: the counts drop at once, the task state changes, and the receipt moves to "changed". An approval says "gate closed"; work that starts is its own change.
- At 320, a project with more tasks than fit takes a second row. "nothing unassigned" replaces "no open task". Later work has its own mark (◷).
- The detail leads with the full question and the orchestrator's recommendation; the question is not repeated. A cut detail says "↓ n more lines".
- Exact keys: `y` or `n`; `b` or `h` closes the help; the header changes while an Input has the keys.
- New screens: send failed, after `x` with a new change, a silent worker, an offline orchestrator, a task inside a project, the ask result, 120 with an answer field.

## Round 4 (codex-309, reply)

Sol: "Keep this layout. The final round needs fixes to state and rendering, not another design direction." On replacing the chief: "The overview is now useful enough to attract Chris's attention. Conflicting current state is the one thing that still sends him back to the chief."

Sol liked: after an answer, every surface agrees (`3 need you`, `6 changed`, `✓ ANSWERED`, the task running again); at 320 every active task has a name and an owner; `◷` and `nothing unassigned`; the recommendation in the detail; the silent-worker evidence (last tool call, last text); exact `y`/`n` and help keys. No width or height over the limits.

Sol rejected:
- After the approval: `saga Elastic compute: undefined of undefined epics done`, and ready tasks that still say `after your approval`.
- After `x`: `✓ MERGED` while "ships next" still lists `#2440 ready to merge`.
- At 120, the typing screen said `✓ ANSWERED` before the answer was sent (a mock bug: a scenario leaked into the next screen). The question and the draft were cut, with no cursor.
- `TYPING AN AN…`: a cut header.
- `! not sent ... the hub did not answer in 8 s`: a timeout means "not confirmed", not "not sent".
- `↓ 0 more items` in the task list, with the selection on a blank row.
- `a` sends a question at once; its label must say what it sends.

What changed for round 5:
- The ship list and the agent count are derived on each draw; every scenario changes the task, its PR, its notes, and the saga line together, and no scenario leaks into the next.
- The 120 answer wraps the question and the draft, and the cursor follows the draft. The typing chip says `ANSWER`.
- A timeout says "not confirmed"; enter checks the hub first and sends again only if the answer did not arrive.
- Task rows are real list items with correct "more" counts.
- `a` says "ask orch-… for a recommendation".
- New screens: receipt expiry, zero changes after `x`, nothing needs you, hub down, session not registered, first run, inline under 110 columns, a narrow dragged panel, text in the prompt.

## Round 5 (codex-309, reply): final

Sol's verdict: "Yes. This is ready to show Chris at a design gate. The layout now answers enough of his questions to justify using it before asking the chief."

Sol verified every scenario: needs-you, changes, and open-PR counts agree on every surface after an answer, a receipt's expiry, an approval, `x`, and a merge after `x`. All 28 screens fit their widths, and every band fits its row limit.

Sol's remaining screen defects, all fixed in the final set:
- Hub down said "what was true at 14:30" over data from 14:35: now "the last good read, 14:46", and a new screen shows the panel with the hub down.
- The empty example showed the approval gate as running: the gate is now closed.
- The silent-worker caption claimed a verdict: it now shows evidence only.
- The PR detail stated a merge policy that Borant does not know: removed.
- The unconfirmed answer said `enter sends` at the top and `check and send` at the bottom: one wording now.
- `borant doctor` described as a fix: it is diagnosis, and it names the fix.
- Selection after a receipt expires: the caption says that a move never acts.

Sol on the specs: all four are needed, none is over-built, no fifth subsystem is needed. Contract gaps, now written into the specs: durable answer keys in the hub (today the key is not stored); the task version the person saw on every action; PR check counts and review-requested in the PR port; one consistency rule for the reads (`at`, a read after each action); event sequence numbers for "seen"; one primary change per task; a running tool is not silence; missing data is not silence; the approval inside a saga is a `needs_human` task that later epics are blocked by. The decline command was cut, and launcher focus starts with tmux only.

Sol's next gate: prove the keyboard and the actions in a real session (arrows and Tab, the wrapped Input, the dock sizes).

## Build review (codex-309, then codex-310)

Sol judged the recorded build (`docs/chief-mod-v3/`: captures after every step at 120, 200, and 320 columns, the PASS/FAIL logs, the videos).

First verdict: "No. Not ready for a review PR. It is much more useful than v0.19.3, but the recording shows selection and action defects." No break of the no-printable-key rule. Must-fixes:
1. `5` then enter opened borant's tasks while roger-emr was selected: the ring and the selection disagreed.
2. In a project's task list, the approval task had no `y` and the stuck task no `r`; at 120 the key footer was blank.
3. The note of the stuck task was cut in a mostly empty 320 panel.
4. After an approval, borant still said "1 need you" until the next read.
5. "SHIPS NEXT" promised readiness that the mod cannot know, and the project's PR column used another order than the list.

All five were fixed and recorded again, with a new check that enter opens the selected project's tasks. Second verdict (codex-310, a short new conversation, because a reply in the long one timed out): each of the five fixed, quoting the captures, and "Ready to send for review: yes."

## Follow-up review: the mod on the new hub data (codex-311)

The follow-up switches the mod to the four merged specs: the board's task lines with PR state, agent activity, the changes feed, and task history. Sol read the recorded captures at 120, 200, and 320 columns.

First verdict: no, with three defects: in a project's task list `r` and `d` were offered without their field and confirmation; "ready to merge" claimed more than the PR watch knows (no draft or conflict state); and the key list cut what `4` and `5` do at 120. All three were fixed, with a new scenario step that answers from a project's task list. Second verdict: "Ready to send for review: yes."
