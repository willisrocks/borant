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
# The haiku session may ask to run a Bash command after a Done prompt (its
# default permission mode, not the chief guard); the dialog would take the
# band's keys and hide the band. Refuse it: Esc never approves.
nodialog() { if tui_text | grep -q 'Do you want to proceed'; then tui_keys Escape; sleep 1; fi; }
shot() { nodialog; sleep "${2:-0.8}"; nodialog; tui_shot "$1"; }
# A test session may stop on a dialog (a permission prompt after the Done
# prompt) or in vim NORMAL mode: clear both so the next keys reach the band.
settle() {
  local _
  # Wait for a running turn to end: Esc would interrupt it and put its prompt
  # back in the composer.
  for _ in $(seq 1 60); do tui_text | grep -q 'esc to interrupt' || break; sleep 0.5; done
  for _ in 1 2 3; do nodialog; done
  tui_text | tail -3 | grep -q -- '-- NORMAL --' && tui_type i
  tui_keys C-u; sleep 0.3
}
# A row digit inside the board: the fixture has four rows.
# The still-open step's item: the fixture has one left by then.
if [ "$variant" = fixture ]; then pdigit=4 sdigit=1; else pdigit=8 sdigit=3; fi

node "$root/scripts/tui/board-variants.mjs" "$variant" "$stub" >>"$log"
: >"$stub/events.jsonl"

trap 'cp "$stub/calls.log" "$TUI_OUT/calls.log" 2>/dev/null; tui_stop; rm -rf "$stub"' EXIT
tui_start "cos-qa-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli BORANT_COS_DONE_WAIT_S=20 \
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
# Ledger band: the keys stay in the prompt after a digit (only the person
# gives a site the keyboard), so space opens there, and typing leaves. Esc
# is the editor's: in vim mode it goes to NORMAL, where digits are counts.
tui_type ' '; shot 06-space-open 1.5
tui_type ' '; shot 07-space-close
# d only arms Done; a key that is not a band key cancels it and types, d too.
tui_type d; shot 07a-d-armed
tui_type e; shot 07a-typed-leaves
note "prompt after d e: [$(tui_text | grep '❯' | tail -1)]"
tui_keys C-u; sleep 0.3

# The person's own focus chord: ctrl+x, then tab within 3 s; then j, Enter,
# and esc in the band itself.
tui_keys C-x; sleep 1; tui_keys Tab; shot 07b-ctrlx-tab 1
tui_type j; shot 07c-ctrlx-j
tui_keys Enter; shot 07d-ctrlx-enter 1.5
tui_keys Escape; shot 07e-ctrlx-esc
tui_text | tail -3 | grep -q -- '-- NORMAL --' && tui_type i
tui_keys C-u; sleep 0.3

# A project row by its digit, then space on it.
tui_type "$pdigit"; shot 08-digit8 1.5
tui_type ' '; shot 09-digit8-open 1.5
# j past the last visible row: the band scrolls to the selection.
for _ in 1 2 3 4; do tui_type j; sleep 0.4; done
tui_type j; shot 09b-j-past-end 1
tui_type x; sleep 0.3; tui_keys C-u; sleep 0.3

# Done on the first item with its key: pressed, landed, then the hub closes it.
tui_type 1; sleep 1.2
tui_type d; shot 10a-d-armed 0.6
tui_type d; shot 10-done-pressed 0.3
shot 11-done-landed 3
first="$(node -e "const b=require('$stub/board.json');const q=b.collections[0].needs_you.slice().sort((a,b)=>(b.gate-a.gate)||(a.since-b.since))[0];console.log(q.title)")"
start="$(now_ms)"
board_edit "for (const c of b.collections) c.needs_you = c.needs_you.filter(q => q.title !== $(node -e "console.log(JSON.stringify(process.argv[1]))" "$first"))"
event
for _ in $(seq 1 80); do nodialog; tui_text | grep -q '✓ closed' && break; sleep 0.25; done
note "Done closed mark after $(since "$start") ms (with an event)"
shot 12-done-closed 0.2
# Ask on the next item: in the ledger band d and a act on the selected row.
# The selection goes 10 s after the last band key, so select it by its digit
# (the closed row still holds 1).
tui_type 2; sleep 1.2
tui_type a; sleep 0.6
tui_type a; shot 13-ask 1.5
note "stub sends: $(grep -c ' send ' "$stub/calls.log" 2>/dev/null || true)"
tui_type x; sleep 0.3; tui_keys C-u; sleep 0.3

# A Done the hub never closes: still open after BORANT_COS_DONE_WAIT_S (20 s).
settle
# The closed row holds its place for 10 s; wait for it to go so the digit
# names an open item.
for _ in $(seq 1 60); do tui_text | grep -q '✓ closed' || break; sleep 0.25; done
tui_type "$sdigit"; sleep 1.2; tui_type d; sleep 0.5; tui_type d; sleep 1
start="$(now_ms)"
for _ in $(seq 1 160); do nodialog; tui_text | grep -q '! still open after' && break; sleep 0.25; done
note "still open shown $(since "$start") ms after the Done press"
shot 13b-still-open 0.2
tui_type x; sleep 0.3; settle

# Refresh: a change with no event waits for the poll; one with an event does not.
start="$(now_ms)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Revoke old willisrocks', 'Revoke stale willisrocks'))"
for _ in $(seq 1 80); do tui_text | grep -q 'Revoke stale' && break; sleep 0.25; done
note "a change with no event showed after $(since "$start") ms"
start="$(now_ms)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Hub backup to object storage', 'Hub backup to R2'))"
event
for _ in $(seq 1 80); do tui_text | grep -q 'Hub backup to R2' && break; sleep 0.25; done
note "a change with an event showed after $(since "$start") ms"
shot 14-refreshed 0.2
# The title row while a read runs (the 5 s poll).
for _ in $(seq 1 120); do tui_text | grep -q '↻ reading' && break; sleep 0.05; done
tui_shot 14a-reading
note "events calls: $(grep -c ' events ' "$stub/calls.log" 2>/dev/null || true)"

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
# A Done's closed row stays 10 s; the empty board shows once it goes.
shot 17-empty 11

tui_type '/cos help'; sleep 0.5; tui_keys Enter; shot 18-help 2
