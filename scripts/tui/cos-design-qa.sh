#!/usr/bin/env bash
# The chief mod redesign's QA run: one real Claude Code session in tmux with a
# live board from scripts/tui/borant-stub, driven with the keys every design
# shares (digits, j, k, Enter, d, a, esc), then a refresh, a Done that the hub
# closes, a resize, and the empty board.
#
# Usage: scripts/tui/cos-design-qa.sh <plugin-dir> <out> <width> <height> [fixture|big]
#
# Writes <out>/<width>x<height>-<variant>/NN-<state>.png (and .txt) and log.txt.
# The session runs `claude --model haiku` with TUI_CLAUDE_FLAGS (lib.sh).
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
plugin="$(realpath "${1:-$root/integrations/claude-code}")"
out="${2:-/tmp/cos-design-qa}"
width="${3:-200}"
height="${4:-50}"
variant="${5:-big}"

source "$root/scripts/tui/lib.sh"
export TUI_OUT="$out/${width}x${height}-$variant"
mkdir -p "$TUI_OUT"
stub="$(mktemp -d)"
export BORANT_STUB_DIR="$stub"
log="$TUI_OUT/log.txt"
: >"$log"
note() { echo "$*" | tee -a "$log"; }
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
# A shot, and the visible rows with no ANSI, for the log.
shot() { sleep "${2:-0.8}"; tui_shot "$1"; }
# Back to an empty prompt that takes typing. With vim mode on, an Esc that
# reaches the prompt leaves insert mode, and a digit is then a count, not an
# edit; `i` goes back to insert mode, and End C-u clears the line.
prompt_reset() { sleep 0.5; tui_type i; sleep 0.3; tui_keys End C-u; sleep 0.3; }
# A Done submits a prompt, and the model's turn holds the keyboard until it
# ends (its spinner line reads "… (12s"); the pane asks for it back then.
wait_idle() {
  sleep 1
  for _ in $(seq 1 90); do tui_text | grep -q '… ([0-9]*s' || return 0; sleep 0.5; done
}
# The chief mod must never put a permission prompt in front of the person.
# One that shows is a failure: it gets a PNG and a FAIL line, then Esc so the
# rest of the run can go on.
dialogs=0
no_dialog() {
  if tui_text | grep -q 'Do you want to proceed\|requires approval'; then
    dialogs=$((dialogs + 1))
    note "FAIL: a permission dialog came up ($1); see dialog-$dialogs.png"
    tui_shot "dialog-$dialogs"
    tui_keys Escape; sleep 0.8
  fi
}

node "$root/scripts/tui/board-variants.mjs" "$variant" "$stub" >>"$log"
: >"$stub/events.jsonl"

trap 'cp "$stub/calls.log" "$TUI_OUT/calls.log" 2>/dev/null; tui_stop; rm -rf "$stub"' EXIT
tui_start "cos-qa-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli BORANT_COS_DONE_WAIT_S=30 \
  claude --model haiku "${TUI_CLAUDE_FLAGS[@]}" --plugin-dir "$plugin"
# The trust dialog defaults to "No, exit": Down, then Enter.
for _ in $(seq 1 40); do
  if tui_text | grep -q 'Yes, I trust this folder'; then tui_keys Down; sleep 0.4; tui_keys Enter; fi
  tui_text | grep -q 'chief of staff\|cos ' && break
  sleep 0.5
done
sleep 3

# A background shell puts a "1 shell" chip in the footer, which takes keys.
tui_type '! sleep 900'; tui_keys Enter; sleep 2
for _ in 1 2 3 4 5 6; do
  tui_text | tail -2 | grep -q 'shell' && break
  tui_keys C-b; sleep 0.3; tui_keys C-b; sleep 1.5
done
shot 01-rest 1

# A digit at the empty prompt: the first needs-you item.
tui_type 1; shot 02-digit1 1.5
note "prompt after 1: [$(tui_text | grep '❯' | tail -1)]"
tui_type j; shot 03-j
tui_type j; shot 04-jj
tui_type k; shot 05-jjk
tui_keys Enter; shot 06-enter 1.5
tui_keys Escape; shot 07-esc
tui_keys Escape; prompt_reset

# A project row by its digit, then Enter on it: 8, or the last digit the band
# names when it names fewer (a small board, or `+N more` below 144 columns).
d8="$(tui_text | grep -E '^(cos|chief of staff) ' | grep -oE '[1-9]: ' | cut -c1 | sort -n | tail -1)"
[ -z "$d8" ] || [ "$d8" -gt 8 ] && d8=8
note "digit for the project step: $d8"
tui_type "$d8"; shot 08-digit8 1.5
tui_keys Enter; shot 09-digit8-enter 1.5
tui_keys Escape; sleep 0.5; tui_keys Escape; prompt_reset

# Done on the first item with its key: pressed, landed, then the hub closes it.
tui_type 1; sleep 1.2
tui_type d; shot 10-done-pressed 0.3
shot 11-done-landed 3
first="$(node -e "const b=require('$stub/board.json');const q=b.collections[0].needs_you.slice().sort((a,b)=>(b.gate-a.gate)||(a.since-b.since))[0];console.log(q.title)")"
start="$(now_ms)"
board_edit "for (const c of b.collections) c.needs_you = c.needs_you.filter(q => q.title !== $(node -e "console.log(JSON.stringify(process.argv[1]))" "$first"))"
event
for _ in $(seq 1 80); do tui_text | grep -q '✓ closed' && break; sleep 0.25; done
note "Done closed mark after $(since "$start") ms (with an event)"
shot 12-done-closed 0.2
no_dialog "after Done"
wait_idle
no_dialog "after Done's turn"
# Ask on the next item.
tui_type a; shot 13-ask 1.5
note "stub sends: $(grep -c ' send ' "$stub/calls.log" 2>/dev/null || true)"
# Done on it too, which the hub never closes: the card says still open.
tui_type d; sleep 1
start="$(now_ms)"
for _ in $(seq 1 120); do tui_text | grep -q 'still open after' && break; no_dialog "in the failed Done"; sleep 0.5; done
note "Done still open mark after $(since "$start") ms (wait 30 s)"
shot 13b-done-failed 0.2
no_dialog "after the failed Done"
wait_idle
no_dialog "after the failed Done's turn"
tui_keys Escape; prompt_reset

# Refresh: a change with no event waits for the poll; one with an event does not.
# The changed titles are epics, which the inbox pane shows under an unfolded
# project, scrolled into view with it: 4 is roger-docs-api (Prod cutover), 2
# is borant (Hub backup).
no_dialog "before the refresh"
tui_type 4; sleep 1.5
start="$(now_ms)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Prod cutover', 'Prod cutover (moved)'))"
for _ in $(seq 1 80); do tui_text | grep -q 'cutover (moved)' && break; sleep 0.25; done
note "a change with no event showed after $(since "$start") ms"
shot 14a-refreshed-poll 0.2
tui_keys Escape; prompt_reset
tui_type 2; sleep 1.5
start="$(now_ms)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Hub backup to object storage', 'Hub backup to R2'))"
event
for _ in $(seq 1 80); do tui_text | grep -q 'Hub backup to R2' && break; sleep 0.25; done
note "a change with an event showed after $(since "$start") ms"
shot 14-refreshed 0.2

no_dialog "before the resize"
# Resize: the other size, then back.
if [ "$width" -ge 150 ]; then other_w=100 other_h=40; else other_w=200 other_h=50; fi
tmux -L "$TUI_NAME" resize-window -t tui -x "$other_w" -y "$other_h"
TUI_PNG_H=0
shot 15-resized 2
tmux -L "$TUI_NAME" resize-window -t tui -x "$width" -y "$height"
TUI_PNG_H=0
shot 16-resized-back 2

# Empty: nothing needs the person and no project has work.
node "$root/scripts/tui/board-variants.mjs" empty "$stub" >>"$log"
event
shot 17-empty 2.5

no_dialog "before help"
tui_keys Escape; prompt_reset
tui_type '/cos help'; sleep 0.5; tui_keys Enter; shot 18-help 2
note "permission dialogs: $dialogs"
