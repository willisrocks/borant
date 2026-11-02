#!/usr/bin/env bash
# The chief mod's interaction scenario: board mode driven with real keys, a
# background shell chip in the footer, and a live board served by
# scripts/tui/borant-stub that the run changes in the middle of navigation.
#
# Usage: scripts/tui/cos-scenario.sh [plugin-dir] [out] [width] [height] [board]
#
# board is `big` (12 projects, 6 need you; the default) or `fixture`, from
# scripts/tui/board-variants.mjs. The plugin's MCP server is
# scripts/tui/stub-mcp.mjs: a Done's turn closes the task with task_close, or
# asks its owner with send, and the scenario plays that owner. Writes <out>/<width>/NN-<state>.png (and
# .txt, .plain.txt) after every key, and log.txt with the checks: a line that
# starts with FAIL is a defect. The session runs `claude --model sonnet` with
# TUI_CLAUDE_FLAGS (lib.sh): the Done step submits a prompt, and the model
# must neither act on it nor reach the real sessions here.
#
# TUI_CWD names the folder the session starts in. Use one that Claude Code
# already trusts, or the run moves the trust dialog to "Yes" first.
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
plugin="$(realpath "${1:-$root/integrations/claude-code}")"
out="${2:-/tmp/cos-scenario}"
width="${3:-200}"
height="${4:-50}"
variant="${5:-big}"

source "$root/scripts/tui/lib.sh"
export TUI_OUT="$out/$width"
mkdir -p "$TUI_OUT"
stub="$(mktemp -d)"
export BORANT_STUB_DIR="$stub"
log="$TUI_OUT/log.txt"
: >"$log"
note() { echo "$*" | tee -a "$log"; }
check() { if eval "$2"; then note "PASS $1"; else note "FAIL $1"; fi; }
now_ms() { node -e 'console.log(Date.now())'; }
since() { echo $(($(now_ms) - $1)); }
event() { echo '{"kind":"task.status","channel":"personal-borant"}' >>"$stub/events.jsonl"; }
board_edit() {
  node --input-type=module -e "
import fs from 'fs'
let b = JSON.parse(fs.readFileSync('$stub/board.json'))
$1
fs.writeFileSync('$stub/board.json', JSON.stringify(b))
"
}
# close_task <title>: the hub closes a needs-you task: it leaves the board,
# and \`borant tasks\` lists it as closed.
close_task() {
  node --input-type=module -e "
import fs from 'fs'
const b = JSON.parse(fs.readFileSync('$stub/board.json'))
for (const c of b.collections) {
  const q = c.needs_you.find((q) => q.title === process.argv[1])
  if (!q) continue
  c.needs_you = c.needs_you.filter((x) => x !== q)
  const file = '$stub/tasks-' + q.channel + '.json'
  const list = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file)) : []
  for (const task of list) if (task.id === q.task) task.status = 'closed'
  fs.writeFileSync(file, JSON.stringify(list))
}
fs.writeFileSync('$stub/board.json', JSON.stringify(b))
" "$1"
}
json() { node -e "console.log(JSON.stringify(process.argv[1]))" "$1"; }
# The needs-you titles in the band's order: gates first, then the oldest.
needs_titles() { node -e "const b=require('$stub/board.json');console.log(b.collections.flatMap(c=>c.needs_you).sort((a,b)=>(b.gate-a.gate)||(a.since-b.since)).map(q=>q.title).join('\n'))"; }
n=0
# step <name> <key...>: press the keys, wait, and shoot NN-<name>.
step() {
  local name="$1"
  shift
  local k
  for k in "$@"; do
    case "$k" in lit:*) tui_type "${k#lit:}" ;; *) tui_keys "$k" ;; esac
    sleep 0.3
  done
  sleep "${STEP_WAIT:-0.9}"
  n=$((n + 1))
  last="$(printf '%02d' "$n")-$name"
  tui_shot "$last"
}
shown() { cat "$TUI_OUT/$last.plain.txt"; }
# The selected item's title as the preview shows it: the first title line
# under the preview's header, right of the pane's border.
pane_title() { perl -CSD -ne 'my @p = split /\x{2502}/; my $r = $p[-1] // ""; if ($r =~ /(needs you|project) \x{b7} \d+ of/) { $h = 1; next } if ($h && $r =~ /^\s*[\x{25b2}\x{2713}\x{2013}]\s+(\S.{0,40})/) { print "$1\n"; exit }' "$TUI_OUT/$1.plain.txt"; }
# The prompt's line: what the composer holds. Claude Code draws a hint or a
# dim suggestion in an empty one, so a check looks for the keys it typed.
prompt_text() { shown | grep '^❯' | tail -1 | perl -CS -pe 's/^\x{276f}[\s\x{a0}]*//; s/^Try ".*"$//; s/[\s\x{a0}]+$//'; }
# The screen line of the preview's first field (`project`), right of the
# pane's border: it must not move when the selected title changes.
pane_field_line() { perl -CSD -ne 'my @p = split /\x{2502}/; if (@p > 1 && $p[-1] =~ /^\s+project\s/) { print "$.\n"; exit }' "$TUI_OUT/$1.plain.txt"; }
in_board() { shown | grep -q 'board mode · esc exit'; }
# The preview header of the selected row: "needs you · 3 of 6", "project · 2 of 9".
selected_is() { shown | grep -q "$1 of "; }
# The header a row's digit selects: the needs-you items come first.
header_for() { local n; n="$(needs_titles | grep -c .)"; if [ "$1" -le "$n" ]; then echo "needs you · $1"; else echo "project · $(($1 - n))"; fi; }
at_rest() { shown | grep -q '0: board mode'; }
# The mod refuses any tool call that would ask the person in a Done's turn,
# so a permission dialog is a defect: log it, and refuse it to go on.
nodialog() { if tui_text | grep -q 'Do you want to proceed'; then note "FAIL a permission dialog appeared"; tui_shot "dialog-$(now_ms)"; tui_keys Escape; sleep 1.5; fi; }
# A running turn: Claude Code's spinner line, `Ruminating… (12s · …)`.
turn_runs() { tui_text | grep -q '… ([0-9]*s'; }
vim_mode() { tui_text | tail -3 | grep -q -- '-- INSERT --\|-- NORMAL --'; }

# The column checker itself, on known captures.
"$root/scripts/tui/columns-selftest.sh" | tee -a "$log"
node "$root/scripts/tui/board-variants.mjs" "$variant" "$stub" >>"$log"
: >"$stub/events.jsonl"

trap 'tui_stop; rm -rf "$stub"' EXIT
tui_start "cos-scenario-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli BORANT_COS_DONE_WAIT_S=20 \
  claude --model sonnet "${TUI_CLAUDE_FLAGS[@]}" --allowedTools mcp__plugin_borant_borant__task_close mcp__plugin_borant_borant__send --plugin-dir "$plugin"
# The trust dialog defaults to "No, exit": Down, and Enter only once "Yes" is chosen.
for _ in $(seq 1 80); do
  if tui_text | grep -q 'Yes, I trust this folder'; then
    tui_text | grep -q '❯ Yes, I trust this folder' || tui_keys Down
    sleep 0.4
    tui_text | grep -q '❯ Yes, I trust this folder' && tui_keys Enter
  fi
  tui_text | grep -q 'chief of staff\|── cos' && break
  sleep 0.5
done
tui_wait 'chief of staff\|── cos' 20 || exit 1
sleep 3

# A background shell puts a "1 shell" chip in the footer, which takes keys.
tui_type '! sleep 900'
tui_keys Enter
sleep 2
for _ in 1 2 3 4 5 6; do
  tui_text | tail -2 | grep -q 'shell' && break
  tui_keys C-b; sleep 0.3; tui_keys C-b; sleep 1.5
done
note "footer: $(tui_text | tail -1)"
check "the event stream started (calls.log has events --follow)" "grep -q 'events --follow' '$stub/calls.log'"

# At rest the band names the key that enters board mode.
step rest
check "at rest the band shows '0: board mode'" "at_rest"

# 0 enters board mode: the band says how to leave, the pane holds the keys.
step zero lit:0
check "0 entered board mode (band shows 'board mode · esc exit')" "in_board"
# From about 186 columns the band keeps 100 beside the docked preview and is
# laid out that wide at rest; narrower, board mode lays it out again.
if [ "$width" -ge 186 ]; then
  check "entering board mode moved no band row or column" "node '$root/scripts/tui/columns.mjs' '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
else
  note "LIMIT below 186 columns the docked preview leaves the band under 100 columns, so board mode lays the band out again"
fi
check "0 did not land in the prompt" "! prompt_text | grep -q 0"

# Every digit selects its row and stays out of the prompt.
for d in 1 2 3 4 5 6 7 8 9; do
  step "digit$d" "lit:$d"
  check "digit $d selected its row ($(header_for "$d"))" "selected_is \"$(header_for "$d")\""
done
check "1-9 in board mode never reached the prompt" "! prompt_text | grep -q '[1-9]' && in_board"
step digit1-again lit:1

# j and k move; the preview follows.
step j lit:j
check "j moved to row 2" "selected_is 'needs you · 2'"
step jj lit:j
step jjk lit:k
check "j then k moved back to row 2" "selected_is 'needs you · 2'"
# Three keys in one burst each count, faster than the key field is drawn
# again: j j j from row 2 ends on row 5, then k k k ends back on row 2.
step burst lit:jjj
check "a burst of j j j moves three rows (row 5), and none reaches the prompt" "selected_is 'needs you · 5' && ! prompt_text | grep -q j"
step burst-back lit:kkk
check "a burst of k k k moves back three rows (row 2)" "selected_is 'needs you · 2' && ! prompt_text | grep -q k"

# A live update in the middle of navigation: titles grow, a count changes, a
# host moves, a new needs-you item arrives. No column may move.
cp "$TUI_OUT/$last.plain.txt" "$TUI_OUT/before-live.plain.txt"
board_edit "
const c = b.collections[0]
for (const q of c.needs_you) q.title = q.title + ' (and now a much longer title that the band must cut)'
for (const p of c.projects) { if (p.tasks && p.tasks.running) p.tasks.running += 7; for (const o of p.orchestrators) o.host = 'a-much-longer-host-name-' + (o.host || 'x') }
c.needs_you.push({ project: 'kata', channel: 'personal-kata', task: '01M5ZG00000000000000099999', title: 'A new question arrived during navigation', status: 'needs_human', attention_msg: 'decision', since: b.at - 60000, gate: false, owner: 'orch-kata' })
"
event
STEP_WAIT=1.5 step live-update
check "no band row or column moved across a live update" "node '$root/scripts/tui/columns.mjs' '$TUI_OUT/before-live.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
check "board mode held across the live update" "in_board"
check "the new item waits for esc, and the band says so" "! shown | grep -q '▲ A new question arrived' && shown | grep -q '+1 after esc\|+1 new'"
step live-j lit:j
step live-k lit:k

# Enter shows details; j and k move with the details open; Enter goes back.
step enter Enter
check "Enter opened the details" "shown | grep -q '⏎ back\|the task\|reading'"
# A live update with the details open: the selected title grows. The fields
# under it stay where they are.
cp "$TUI_OUT/$last.plain.txt" "$TUI_OUT/before-details-live.plain.txt"
board_edit "for (const c of b.collections) for (const q of c.needs_you) if (q.title === $(json "$(needs_titles | sed -n 2p)")) q.title = q.title + ' and then a great deal more words that would need two more lines of the card to show'"
event
STEP_WAIT=1.5 step details-live
check "a live update with the details open moved no field of the preview" "[ \"\$(pane_field_line before-details-live)\" = \"\$(pane_field_line $last)\" ] && [ -n \"\$(pane_field_line $last)\" ]"
step details-j lit:j
check "j in details moves on and keeps the details open" "selected_is 'needs you · 3' && shown | grep -q 'the task'"
step details-k lit:k
check "k in details moves back with the details open" "selected_is 'needs you · 2' && shown | grep -q 'the task'"
step details-back Enter
check "Enter again closed the details" "! shown | grep -q 'the task'"
step help 'lit:?'
check "? showed the help" "shown | grep -q 'enter board mode, from'"
step help-back 'lit:?'
step stray-x lit:x
check "a letter the board does not use stays out of the prompt" "! prompt_text | grep -q x && in_board"

# Done on the first item: pressed, landed, then the hub closes it.
step done-select lit:1
STEP_WAIT=0.2 step done-pressed lit:d
check "d showed feedback at once" "shown | grep -q '◷ sending\|◷ closing'"
STEP_WAIT=3 step done-landed
nodialog
first="$(needs_titles | head -1)"
# The chief's turn must act: task_close (the stub MCP closes it) or a send to
# the owner (the scenario, as the owner, closes it then).
for _ in $(seq 1 120); do nodialog; grep -q ' mcp task_close \| mcp send ' "$stub/calls.log" && break; sleep 0.5; done
start="$(now_ms)"
if grep -q ' mcp task_close ' "$stub/calls.log"; then
  note "PASS the chief closed the task with task_close"
elif grep -q ' mcp send ' "$stub/calls.log"; then
  note "PASS the chief asked the owner to close it (send); the owner closes it"
  close_task "$first"
  event
else
  note "FAIL the chief neither closed the task nor asked its owner within 60 s"
fi
for _ in $(seq 1 80); do tui_text | grep -q '✓ closed' && break; sleep 0.25; done
note "Done shows closed $(since "$start") ms after the task closed"
STEP_WAIT=0.2 step done-closed
check "the closed item shows ✓ closed" "shown | grep -q '✓ closed'"

# Ask on the next item.
nodialog
step ask-j lit:j
step ask lit:a
STEP_WAIT=1.5 step ask-sent
check "Ask sent one message (borant send as the person)" "[ \"\$(grep -c '^[0-9.N]* send ' '$stub/calls.log')\" = 1 ]"
check "the asked item shows ✓ asked" "shown | grep -q '✓ asked'"

# A slow Ask never holds up the keys after it.
touch "$stub/send.slow"
step slow-select lit:3
STEP_WAIT=0.3 step slow-ask lit:a
before_j="$last"
STEP_WAIT=0.3 step slow-ask-j lit:j
check "j moved while a slow Ask was still sending" "[ -n \"\$(pane_title $last)\" ] && [ \"\$(pane_title $before_j)\" != \"\$(pane_title $last)\" ] && shown | grep -q '◷ asking'"
rm -f "$stub/send.slow"
sleep 3

# Esc leaves board mode: the pane closes and the keys reach the prompt again.
nodialog
step esc Escape
check "esc left board mode (band shows '0: board mode')" "at_rest"
if [ "$width" -ge 186 ]; then
  check "after esc the band keeps the columns it had at rest" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
fi
check "after esc the held-back item shows" "tui_text | grep -q '▲ A new question arrived'"
step type-hello lit:hello
check "after esc, typed keys reach the prompt" "prompt_text | grep -q hello"
# A 0 or a digit after text is text.
step type-zero lit:0
step type-digit lit:3
check "0 and a digit in a non-empty prompt stay text" "prompt_text | grep -q 'hello03' && at_rest"
tui_keys C-u
sleep 0.5

# Tab and Shift+Tab would move the ring off the key field, onto the pane's
# close mark, where a letter goes to the prompt: the mod keeps the ring.
step tab-enter lit:0
step tab Tab
step tab-type lit:z
check "after Tab board mode holds, and a letter stays out of the prompt" "in_board && ! prompt_text | grep -q z"
step shift-tab BTab
step shift-tab-type lit:z
check "after Shift+Tab board mode holds, and a letter stays out of the prompt" "in_board && ! prompt_text | grep -q z"
step tab-exit Escape
check "esc after Tab left board mode" "at_rest"

# A click on a row enters board mode on that row.
tui_keys C-u
sleep 0.3
step click-rest
# A click on the middle of row 3's title, not its digit.
read -r cx cy <<<"$(tui_find "$(needs_titles | sed -n 3p | cut -c1-24)")"
tui_click $((cx + 10)) "$cy"
STEP_WAIT=1.2 step click-row
check "a click on row 3's title entered board mode on it" "in_board && selected_is 'needs you · 3'"
step click-esc Escape
# A click on a row past nine, which has no digit.
read -r cx cy <<<"$(tui_find 'wellsky-know')"
tui_click $((cx + 4)) "$cy"
STEP_WAIT=1.2 step click-row-past-nine
check "a click on a row past nine entered board mode on it" "in_board && shown | grep -q 'project · [0-9]* of' && shown | grep -q '│wellsky-knowledge-base'"
step click-past-nine-esc Escape

# ctrl+x tab gives the band the keys, and the pane cannot take them from
# there: 0 then says how to enter board mode instead of failing quietly.
tui_keys C-x
sleep 1
step ctrlx-tab Tab
STEP_WAIT=0.6 step ctrlx-tab-zero lit:0
check "0 after ctrl+x tab says how to enter board mode" "tui_text | grep -q 'esc, then 0'"
step ctrlx-esc Escape
step ctrlx-reenter lit:0
check "esc, then 0 entered board mode" "in_board"
if [ "$width" -ge 186 ]; then
  check "board mode after ctrl+x tab has the columns of the band at rest" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
fi
step ctrlx-exit Escape
if [ "$width" -ge 186 ]; then
  check "after ctrl+x tab and board mode the band has the columns it had at rest" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
fi

# Vim mode. In normal mode Claude Code gives a bare digit to vim (a count or
# a motion) and raises nothing a mod sees, so 0 stays with vim: that is a
# limit of the mod API, logged as LIMIT. i, then 0, enters board mode.
if vim_mode; then
  tui_keys Escape
  sleep 0.6
  step vim-normal
  check "Esc at the empty prompt left vim insert mode" "! shown | tail -3 | grep -q -- '-- INSERT --'"
  step vim-normal-zero lit:0
  if in_board; then note "PASS 0 in vim normal mode entered board mode"; step vim-normal-esc Escape; else note "LIMIT 0 in vim normal mode stays with vim (no event reaches the mod)"; fi
  step vim-insert i
  check "i put the prompt back in vim insert mode" "shown | tail -3 | grep -q -- '-- INSERT --'"
  step vim-insert-zero lit:0
  check "0 in vim insert mode entered board mode" "in_board"
  step vim-insert-digit lit:4
  check "4 in board mode selected row 4" "shown | grep -q ' 4 of '"
  step vim-insert-esc Escape
  check "esc after vim insert mode left board mode" "at_rest"
else
  note "SKIP vim mode: this Claude Code is not in vim mode"
fi

# Board mode during a running turn: the model works, the keys stay on the board.
tui_type 'write a 3000 word essay on the history of tea, in one answer, no tools'
tui_keys Enter
for _ in $(seq 1 20); do turn_runs && break; sleep 0.5; done
step turn-zero lit:0
check "0 entered board mode while a turn ran" "in_board && shown | grep -q '… ([0-9]*s'"
before_j="$last"
step turn-j lit:j
check "j moved while a turn ran, and stayed out of the prompt" "in_board && [ \"\$(pane_title $before_j)\" != \"\$(pane_title $last)\" ] && ! prompt_text | grep -q j"
step turn-exit Escape
# Stop the long turn, so a Done's prompt later does not wait behind it.
tui_keys C-c
for _ in $(seq 1 60); do turn_runs || break; sleep 0.5; done
tui_text | tail -3 | grep -q -- '-- NORMAL --' && tui_keys i
sleep 0.5

# Refresh: a change with an event shows within a second, at rest.
title="$(needs_titles | tail -1)"
start="$(now_ms)"
board_edit "for (const c of b.collections) for (const q of c.needs_you) if (q.title === $(json "$title")) q.title = 'Evented: ' + q.title"
event
for _ in $(seq 1 200); do tui_text | grep -q 'Evented: ' && break; sleep 0.05; done
evented="$(since "$start")"
note "a change with an event showed after $evented ms"
check "an event-driven refresh landed in under 1 s ($evented ms)" "[ $evented -lt 1000 ]"
step refreshed

# A Done the hub never closes shows "still open" after BORANT_COS_DONE_WAIT_S:
# the stub MCP refuses task_close, as a hub refuses a chief that may not.
touch "$stub/close.refuse"
nodialog
step still-select lit:1
step still-d lit:d
for _ in $(seq 1 60); do nodialog; tui_text | grep -q '! still open' && break; sleep 0.5; done
STEP_WAIT=0.3 step still-open
check "an unclosed Done shows ! still open" "shown | grep -q '! still open'"
step still-esc Escape

# The hub stops answering: the board says so, with the reason.
touch "$stub/board.fail"
for _ in $(seq 1 40); do tui_text | grep -q 'unavailable' && break; sleep 0.25; done
step unavailable
rm -f "$stub/board.fail"
for _ in $(seq 1 40); do tui_text | grep -q '0: board mode' && break; sleep 0.25; done

# Resize: the other size, then back.
if [ "$width" -ge 150 ]; then other_w=120 other_h=40; else other_w=240 other_h=60; fi
tmux -L "$TUI_NAME" resize-window -t tui -x "$other_w" -y "$other_h"
TUI_W="$other_w" TUI_H="$other_h"
STEP_WAIT=2 step resized
tmux -L "$TUI_NAME" resize-window -t tui -x "$width" -y "$height"
TUI_W="$width" TUI_H="$height"
STEP_WAIT=2 step resized-back
if [ "$width" -ge 186 ]; then
  check "after a resize and back the band has the columns it had at rest" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
  # The same with board mode on through the resize.
  step resize-open lit:0
  tmux -L "$TUI_NAME" resize-window -t tui -x "$other_w" -y "$other_h"
  TUI_W="$other_w" TUI_H="$other_h"
  STEP_WAIT=2 step resized-open
  step resized-open-esc Escape
  tmux -L "$TUI_NAME" resize-window -t tui -x "$width" -y "$height"
  TUI_W="$width" TUI_H="$height"
  STEP_WAIT=2 step resized-open-back
  check "after a resize with board mode on, and back, the band has the columns it had at rest" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
  step resized-open-again lit:0
  check "and board mode then moves no column" "node '$root/scripts/tui/columns.mjs' --cells '$TUI_OUT/01-rest.plain.txt' '$TUI_OUT/$last.plain.txt' >>'$log'"
  step resized-open-again-esc Escape
fi

# Empty: nothing needs the person and no project has work.
node "$root/scripts/tui/board-variants.mjs" empty "$stub" >>"$log"
event
STEP_WAIT=6 step empty

nodialog
step help-command lit:/cos\ help Enter
note "event streams started: $(grep -c ' events --follow' "$stub/calls.log" 2>/dev/null || echo 0)"
grep -q '^FAIL' "$log" && exit 1
exit 0
