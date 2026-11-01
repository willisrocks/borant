#!/usr/bin/env bash
# The chief mod's interaction scenario: real keys, the ctrl+x tab chord with a
# pause, a background shell chip in the footer, and a live board served by
# scripts/tui/borant-stub that the run changes, with refresh times.
#
# Usage: scripts/tui/cos-scenario.sh [plugin-dir] [out] [width] [height] [board]
#
# board is `big` (12 projects, 6 need you; the default) or `fixture`, from
# scripts/tui/board-variants.mjs. Writes <out>/<width>/NN-<state>.png (and
# .txt) for each step, and log.txt with the refresh times and the checks: a
# line that starts with FAIL is a defect. The session runs `claude --model
# haiku` with TUI_CLAUDE_FLAGS (lib.sh): the Done step submits a prompt, and
# the model must neither act on it nor reach the real sessions here.
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
json() { node -e "console.log(JSON.stringify(process.argv[1]))" "$1"; }
# The needs-you titles in the band's order: gates first, then the oldest.
needs_titles() { node -e "const b=require('$stub/board.json');console.log(b.collections.flatMap(c=>c.needs_you).sort((a,b)=>(b.gate-a.gate)||(a.since-b.since)).map(q=>q.title).join('\n'))"; }
shot() { sleep "${2:-0.8}"; tui_shot "$1"; }
# The mod refuses any tool call that would ask the person in a Done's turn,
# so a permission dialog is a defect: log it, and refuse it to go on.
nodialog() { if tui_text | grep -q 'Do you want to proceed'; then note "FAIL a permission dialog appeared"; tui_shot "dialog-$(now_ms)"; tui_keys Escape; sleep 1.5; fi; }
# The keys back at an empty prompt. One Esc leaves the preview; an Esc at the
# empty prompt would open Claude Code's rewind dialog, so none is pressed there.
to_prompt() { nodialog; tui_keys Escape; sleep 0.5; tui_keys C-u; sleep 0.3; }

node "$root/scripts/tui/board-variants.mjs" "$variant" "$stub" >>"$log"
: >"$stub/events.jsonl"

trap 'tui_stop; rm -rf "$stub"' EXIT
tui_start "cos-scenario-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli BORANT_COS_DONE_WAIT_S=20 \
  claude --model haiku "${TUI_CLAUDE_FLAGS[@]}" --plugin-dir "$plugin"
# The trust dialog defaults to "No, exit": Down, then Enter.
for _ in $(seq 1 80); do
  if tui_text | grep -q 'Yes, I trust this folder'; then tui_keys Down; sleep 0.4; tui_keys Enter; fi
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
shot 01-band 1

# The event stream runs: the stub logged `borant events --follow`.
check "the event stream started (calls.log has events --follow)" "grep -q 'events --follow' '$stub/calls.log'"

# The band takes the keys with ctrl+x, a 1 s pause, tab; j and k move its
# selection, and a docked preview follows it.
tui_keys C-x; sleep 1; tui_keys Tab; shot 02-band-focused
tui_type j; shot 03-band-j
tui_type j; shot 04-band-jj
tui_type k; shot 05-band-jjk
# Esc gives the keys back: the band drops its fill and names the digits again.
tui_keys Escape; shot 05b-band-esc 1.5
check "the band let the keys go after esc" "grep -q 'select' '$TUI_OUT/05b-band-esc.plain.txt'"
tui_keys C-u; sleep 0.3

# A digit at the empty prompt selects its row and gives the preview the keys.
tui_type 1; shot 06-digit1 1.5
check "the digit did not land in the prompt" "! tui_text | grep '❯' | tail -1 | grep -q '1'"
tui_type j; shot 07-digit1-j
tui_keys Enter; shot 08-more 1.5
to_prompt

# Done on the first item: pressed, landed, then the hub closes it.
tui_type 1; sleep 1.2
tui_type d; shot 09-done-pressed 0.15
shot 10-done-landed 3
first="$(needs_titles | head -1)"
start="$(now_ms)"
board_edit "for (const c of b.collections) c.needs_you = c.needs_you.filter(q => q.title !== $(json "$first"))"
event
for _ in $(seq 1 80); do tui_text | grep -q '✓ closed' && break; sleep 0.25; done
note "Done shows closed $(since "$start") ms after the hub closed it (with an event)"
shot 11-done-closed 0.2

# Ask on the next item. The closed one keeps its row and the selection 10 s,
# so j moves on first.
nodialog
if tui_text | grep -q 'esc prompt'; then tui_type j; sleep 0.5; else tui_keys C-u; tui_type 2; sleep 1.2; fi
tui_type a; shot 12-ask 1.5
check "Ask sent one message (stub send)" "[ \"\$(grep -c ' send ' '$stub/calls.log')\" = 1 ]"
to_prompt

# Refresh: a change with no event waits for the 5 s poll; one with an event
# shows at once. "↻ reading" stays at least 600 ms from the start of a read.
last="$(needs_titles | tail -1)"
start="$(now_ms)"
board_edit "for (const c of b.collections) for (const q of c.needs_you) if (q.title === $(json "$last")) q.title = 'Moved: ' + q.title"
for _ in $(seq 1 80); do tui_text | grep -q 'Moved: ' && break; sleep 0.25; done
note "a change with no event showed after $(since "$start") ms"
sleep 1
title="$(needs_titles | tail -1)"
start="$(now_ms)"
board_edit "for (const c of b.collections) for (const q of c.needs_you) if (q.title === $(json "$title")) q.title = 'Evented: ' + q.title.replace('Moved: ', '')"
event
for _ in $(seq 1 200); do tui_text | grep -q 'Evented: ' && break; sleep 0.05; done
evented="$(since "$start")"
tui_shot 13-reading
note "a change with an event showed after $evented ms"
check "an event-driven refresh landed in under 1 s ($evented ms)" "[ $evented -lt 1000 ]"
shot 14-refreshed 0.8

# A Done the hub never closes shows "still open" after BORANT_COS_DONE_WAIT_S.
nodialog
tui_keys C-u; tui_type 1; sleep 1.2
tui_type d; sleep 1
nodialog
for _ in $(seq 1 60); do tui_text | grep -q '! still open' && break; sleep 0.5; done
nodialog
shot 15-still-open 0.3
to_prompt

# The event stream stops: the title says polling, with the data's age.
touch "$stub/events.off"
pkill -f "tail -n0 -F $stub/events.jsonl" || true
for _ in $(seq 1 40); do tui_text | grep -q 'polling' && break; sleep 0.25; done
shot 16-polling 2.5
# The hub stops answering: the board says so, with the reason.
touch "$stub/board.fail"
for _ in $(seq 1 40); do tui_text | grep -q 'unavailable' && break; sleep 0.25; done
shot 17-unavailable 0.5
rm -f "$stub/board.fail" "$stub/events.off"
for _ in $(seq 1 60); do tui_text | grep -q ' live ' && break; sleep 0.5; done
check "the event stream started again after it ended" "tui_text | grep -q ' live '"
nodialog

# Resize: the other size, then back.
if [ "$width" -ge 150 ]; then other_w=100 other_h=40; else other_w=200 other_h=50; fi
tmux -L "$TUI_NAME" resize-window -t tui -x "$other_w" -y "$other_h"
TUI_PNG_H=0
shot 18-resized 2
tmux -L "$TUI_NAME" resize-window -t tui -x "$width" -y "$height"
TUI_PNG_H=0
shot 19-resized-back 2

# Empty: nothing needs the person and no project has work.
node "$root/scripts/tui/board-variants.mjs" empty "$stub" >>"$log"
event
shot 20-empty 11

nodialog
tui_type '/cos help'; sleep 0.5; tui_keys Enter; shot 21-help 2
# The event stream must reach borant: an event-driven refresh needs it.
note "event streams started: $(grep -c ' events --follow' "$stub/calls.log" 2>/dev/null || echo 0)"
grep -c '^FAIL' "$log" >/dev/null && exit 1
exit 0
