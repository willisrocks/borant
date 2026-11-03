#!/usr/bin/env bash
# The chief mod v3's flows in a real Claude Code session, at one size, on the
# design's sample swarm (scripts/tui/swarm.mjs) served by scripts/tui/borant-stub.
#
# Usage: scripts/tui/cos-scenario.sh [plugin-dir] [out] [width] [height]
#
# Each step presses real keys at a person's pace, writes <out>/<width>/NN-<step>.png
# (and .txt, .plain.txt), and logs PASS or FAIL lines to log.txt: the hotkey at
# rest, no key taken at the prompt, ctrl+x b, j, the section jumps, a project's
# tasks and back, an answer and its result, an approval and its result, the
# help, x, esc, ctrl+x b with text in the prompt, the hub down, and the band
# within its width and its row limit. With RECORD=1 the pane is captured every
# 200 ms the whole time and joined into <out>/<width>/rec/flows-<width>.mp4.
#
# TUI_CWD names the folder the session starts in: one that Claude Code
# already trusts. The run never answers a trust dialog.
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
plugin="$(realpath "${1:-$root/integrations/claude-code}")"
out="${2:-/tmp/cos-scenario}"
width="${3:-200}"
height="${4:-50}"

source "$root/scripts/tui/lib.sh"
export TUI_OUT="$out/$width"
rm -rf "$TUI_OUT"
mkdir -p "$TUI_OUT"
stub="$(mktemp -d)"
log="$TUI_OUT/log.txt"
: >"$log"
note() { echo "$*" | tee -a "$log"; }
check() { if eval "$2"; then note "PASS $1"; else note "FAIL $1"; fi; }
n=0
last=""
# step <name> <key...>: press each key (lit:<text> types it one key at a
# time, as a person does; chord:<k> is ctrl+x then k), wait, and shoot NN-<name>.
step() {
  local name="$1" k ch
  shift
  for k in "$@"; do
    case "$k" in
      lit:*)
        local text="${k#lit:}" i
        for ((i = 0; i < ${#text}; i++)); do
          ch="${text:$i:1}"
          tui_type "$ch"
          sleep 0.08
        done
        ;;
      chord:*) tui_keys C-x; sleep 0.4; tui_keys "${k#chord:}" ;;
      *) tui_keys "$k" ;;
    esac
    sleep "${KEY_WAIT:-0.6}"
  done
  sleep "${STEP_WAIT:-1.2}"
  n=$((n + 1))
  last="$(printf '%02d' "$n")-$name"
  tui_shot "$last"
}
shown() { cat "$TUI_OUT/$last.plain.txt"; }
called() { grep -q "$1" "$stub/calls.log"; }
prompt_text() { shown | grep '^❯' | tail -1 | perl -CS -pe 's/^\x{276f}[\s\x{a0}]*//; s/^Try ".*"$//; s/[\s\x{a0}]+$//'; }
# The band: the rows from its header (the row that counts what needs you) to the prompt's rule.
band_rows() { shown | awk '/need you/ && !b {b=1} b && /^─────/ {exit} b {n++} END {print n+0}'; }
widest() { shown | perl -CS -ne 'chomp; $m = length if length > $m; END { print $m + 0 }'; }

node "$root/scripts/tui/swarm.mjs" "$stub" >>"$log"
: >"$stub/events.jsonl"
rec=""
cleanup() {
  if [ -n "$rec" ]; then touch "$out/$width/rec/stop"; wait "$rec" 2>/dev/null; fi
  tui_stop
  rm -rf "$stub"
}
trap cleanup EXIT
tui_start "cos-v3-$$" "$width" "$height" env PATH="$root/scripts/tui/stub-bin:$PATH" \
  BORANT_STUB_DIR="$stub" BORANT_ROLE=chief BORANT_COS_BOARD=cli \
  claude --model haiku "${TUI_CLAUDE_FLAGS[@]}" --plugin-dir "$plugin"
for _ in $(seq 1 60); do
  tui_text | grep -q 'trust this folder' && { note "FAIL a trust dialog: start in a trusted folder (TUI_CWD)"; exit 1; }
  tui_text | grep -q 'need you' && break
  sleep 0.5
done
if [ "${RECORD:-0}" = 1 ]; then
  mkdir -p "$out/$width/rec"
  node "$root/scripts/tui/record.mjs" capture "cos-v3-$$" "$out/$width/rec" &
  rec=$!
fi
limit=$(((height - 6) / 2))
sleep 2

step rest
check "at rest the band names the hotkey" "shown | grep -q 'ctrl+x b'"
check "a worker with no tool call for 30 minutes is silent" "shown | grep -q '1 silent'"
# A change lands while the person looks: it shows, by name, on the next read.
echo '{"seq":'"$(date +%s)"',"at":'"$(node -e 'console.log(Date.now())')"',"project":"roger-docs-api","channel":"work-roger-docs-api","task":null,"verb":"pr_merged","text":"PR #612 Retry-After on 429","by":{"type":"agent","id":"wkr-docs-1"}}' >>"$stub/changes.jsonl"
echo '{"kind":"change.updated"}' >>"$stub/events.jsonl"
STEP_WAIT=6 step live-change
check "a change from the hub feed shows by name" "shown | grep -q 'Retry-After on 429'"
check "the footer names the hotkey" "shown | tail -2 | grep -q 'ctrl+x b'"
check "the band names projects by their tasks" "shown | grep -q 'Billing export'"
check "the band stays within its row limit ($(band_rows) <= $limit)" "[ $(band_rows) -le $limit ]"
check "no line is wider than the terminal" "[ $(widest) -le $width ]"

step typed lit:'1. fix the bug'
check "a digit and letters at the prompt are typing: '$(prompt_text)'" "[ \"\$(prompt_text)\" = '1. fix the bug' ]"
check "typing opens nothing" "! shown | grep -q 'KEYS ON BORANT'"
step cleared C-u

step open chord:b
check "ctrl+x b gives Borant the keys" "shown | grep -q 'KEYS ON BORANT'"
check "the panel shows the first item's detail" "shown | grep -q 'BLOCKED SPAWN'"

step move j j
check "j j selects the third item: its detail shows" "shown | grep -q 'NEEDS YOUR ANSWER'"

step ships 3
check "3 jumps to the open PRs" "shown | grep -q '⇡ OPEN PR'"
check "the open PRs say how close each is to merge" "shown | grep -q 'approved, checks pass'"
step projects 5
# The selected project, as its detail names it: "<name>  <collection> · ".
picked="$(shown | perl -CS -ne 'if (/(?:\x{2502} ?|^)([a-z][a-z0-9-]+)  (personal|work) \x{b7} /) { print $1; exit }')"
step tasks Enter
check "enter opens the selected project's tasks ($picked)" "[ -n \"$picked\" ] && shown | grep -q \"$picked: every task\""
step task j
step task-back k
step task-answer r
check "r in a project's task list opens the answer field there" "shown | grep -q ' ANSWER ' && shown | grep -q 'answer to wkr-emr-7'"
step task-esc Escape
check "esc from the field gives the keys back" "! shown | grep -q 'KEYS ON BORANT\\| ANSWER '"
step reopen chord:b
check "ctrl+x b opens the list again" "shown | grep -q 'KEYS ON BORANT' && ! shown | grep -q ': every task'"

step needs 1 j j
step answer r
check "r opens the answer field" "shown | grep -q ' ANSWER '"
step typed-answer lit:'Record a fixture once and replay it.'
check "the answer field takes letters, j, and digits" "shown | grep -q 'Record a fixture once'"
step sent Enter
STEP_WAIT=3 step answered
check "the answer ran borant answer" "called 'answer'"
check "the item shows its result, and the count dropped" "shown | grep -q 'answered' && shown | grep -q '3 need you'"

# The receipt row leaves after 10 s; then approve is the third item.
STEP_WAIT=8 step answered-later
check "the receipt row left after 10 s" "! shown | grep -q 'answered [0-9][0-9]:[0-9][0-9] · sent'"
step approve-select 1 j j
step approve y
check "y asks once more" "shown | grep -q 'Approve it?'"
step approved y
STEP_WAIT=3 step approved-later
check "y y ran borant task close" "called 'task close'"
check "the approval shows its result" "shown | grep -q 'approved'"

step help h
check "h lists every key" "shown | grep -q 'ALL KEYS'"
step help-back h
step seen x
check "x marks the changes seen" "shown | grep -q 'CHANGED SINCE [0-9:]* · 0\\|nothing since'"
check "the answer reached the hub feed and the history" "grep -q answered \"\$stub/changes.jsonl\""
step esc Escape
check "esc gives the keys back" "! shown | grep -q 'KEYS ON BORANT'"
step typed-again lit:'0 tasks'
check "after esc a digit is typing: '$(prompt_text)'" "[ \"\$(prompt_text)\" = '0 tasks' ]"
step nokeys chord:b
check "ctrl+x b with text in the prompt says why the keys stay there" "shown | grep -q 'keys stay on the prompt'"
step nokeys-esc Escape C-u Escape

touch "$stub/board.fail"
STEP_WAIT=7 step hub-down
check "a hub that is down is named, with the last good read" "shown | grep -q 'no hub since'"
rm -f "$stub/board.fail"
STEP_WAIT=7 step hub-back
check "the band is back when the hub is" "! shown | grep -q 'no hub since'"

if [ -n "$rec" ]; then
  touch "$out/$width/rec/stop"
  wait "$rec"
  rec=""
  node "$root/scripts/tui/record.mjs" render "$out/$width/rec" "$width" "flows-$width" | tee -a "$log"
fi
note "$(grep -c '^FAIL' "$log") FAIL lines"
