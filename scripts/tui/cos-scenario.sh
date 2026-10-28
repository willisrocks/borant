#!/usr/bin/env bash
# The chief mod's interaction scenario: real keys, chords with a pause, mouse
# clicks and drags, a background shell chip in the footer, and a live board
# served by scripts/tui/borant-stub that the run changes, with refresh times.
#
# Usage: scripts/tui/cos-scenario.sh [plugin-dir] [out] [width] [height]
#
# Writes <out>/<width>/NN-<state>.png (and .txt) for each step, and log.txt
# with the prompt after each key test, the pane edge after each resize, and
# how long each board change took to show. The session runs `claude --model
# haiku --permission-mode default`: the Done step submits a prompt, and the
# model must not act on it without a person.
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
plugin="$(realpath "${1:-$root/integrations/claude-code}")"
out="${2:-/tmp/cos-scenario}"
width="${3:-200}"
height="${4:-50}"

source "$root/scripts/tui/lib.sh"
export TUI_OUT="$out/$width"
mkdir -p "$TUI_OUT"
stub="$(mktemp -d)"
export BORANT_STUB_DIR="$stub"
log="$TUI_OUT/log.txt"
: >"$log"
note() { echo "$*" | tee -a "$log"; }
prompt_line() { tui_text | grep '❯' | tail -1; }
since() { echo $(($(date +%s%3N) - $1)); }

node --input-type=module -e "
import { board } from '$root/integrations/claude-code/mod/fixtures/board.js'
import { tasks } from '$root/integrations/claude-code/mod/fixtures/tasks.js'
import fs from 'fs'
fs.writeFileSync('$stub/board.json', JSON.stringify(board))
for (const [c, l] of Object.entries(tasks)) fs.writeFileSync('$stub/tasks-' + c + '.json', JSON.stringify(l))
"
: >"$stub/events.jsonl"
board_edit() {
  node --input-type=module -e "
import fs from 'fs'
let b = JSON.parse(fs.readFileSync('$stub/board.json'))
$1
fs.writeFileSync('$stub/board.json', JSON.stringify(b))
"
}
event() { echo '{"kind":"task.status","channel":"personal-borant"}' >>"$stub/events.jsonl"; }

trap 'tui_stop; rm -rf "$stub"' EXIT
tui_start "cos-scenario-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli claude --model haiku --permission-mode default --plugin-dir "$plugin"
tui_wait 'chief of staff' 80 || exit 1
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
tui_shot 01-band

# The band: ctrl+x, a 1 s pause, tab, then j j k.
tui_keys C-x; sleep 1; tui_keys Tab; sleep 0.8; tui_shot 02-band-focused
tui_type j; sleep 0.8; tui_shot 03-band-j
tui_type j; sleep 0.8; tui_shot 04-band-jj
tui_type k; sleep 0.8; tui_shot 05-band-jjk
note "prompt after band j j k: [$(prompt_line)]"
tui_keys Escape; sleep 0.5; tui_keys C-u; sleep 0.3

# A digit from an empty prompt opens a project; j must move in the pane.
tui_type 3; sleep 2.5; tui_shot 06-digit3
tui_type j; sleep 0.8; tui_shot 07-digit3-j
note "prompt after 3 j: [$(prompt_line)]"
tui_keys Escape; sleep 0.6; tui_keys C-u; sleep 0.3
# 0 from an empty prompt gives the open pane the keyboard.
tui_type 0; sleep 2.5; tui_type j; sleep 0.8; tui_shot 08-zero-j
note "prompt after 0 j: [$(prompt_line)]"

# The pane's width: drag its edge 15 columns right (narrower; it opens at
# its widest), then ctrl+x left to widen it again.
edge="$(tui_column 2 '│')"
note "pane edge: $edge"
if [ "${edge:--1}" -ge 0 ]; then tui_drag $((edge + 1)) $((edge + 16)) 10; sleep 0.8; fi
note "pane edge after a drag 15 right: $(tui_column 2 '│')"
tui_shot 09-dragged
tui_keys C-x; sleep 0.3; tui_keys Left; sleep 0.8
note "pane edge after ctrl+x left: $(tui_column 2 '│')"
tui_shot 10-ctrlx-left

# Done: open Needs you with 1 and click [ Done ].
tui_keys Escape; sleep 0.5; tui_keys C-u; sleep 0.3
tui_type 1; sleep 2.5; tui_shot 11-needs
read -r col row < <(tui_find '[ Done ]')
note "[ Done ] at column ${col:-?} row ${row:-?}"
[ -n "${row:-}" ] && tui_click $((col + 2)) "$row"
sleep 0.6; tui_shot 12-done-clicked

# The hub closes the task: the item leaves the board and its event arrives.
start="$(date +%s%3N)"
board_edit "for (const c of b.collections) c.needs_you = (c.needs_you || []).filter(q => !String(q.title).startsWith('Create Hetzner'))"
event
for _ in $(seq 1 120); do tui_text | grep -q 'Needs you (1)' && break; sleep 0.25; done
note "closed item left the board after $(since "$start") ms (with an event)"
tui_shot 13-done-closed

# Refresh: a change with no event waits for the poll; one with an event does not.
# The needs-you title shows whole in the band at every size.
start="$(date +%s%3N)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Revoke old', 'Revoke stale'))"
for _ in $(seq 1 120); do tui_text | grep -q 'Revoke stale' && break; sleep 0.25; done
note "a change with no event showed after $(since "$start") ms"
start="$(date +%s%3N)"
board_edit "b = JSON.parse(JSON.stringify(b).replaceAll('Revoke stale', 'Revoke expired'))"
event
for _ in $(seq 1 120); do tui_text | grep -q 'Revoke expired' && break; sleep 0.25; done
note "a change with an event showed after $(since "$start") ms"
tui_shot 14-refreshed
