# shellcheck shell=bash
# Helpers for driving a TUI in tmux and rendering what it shows. Source it:
#
#   source scripts/tui/lib.sh
#   tui_start cos 200 50 claude --plugin-dir integrations/claude-code
#   tui_wait 'chief of staff'; tui_keys C-x; sleep 1; tui_keys Tab
#   tui_shot band-focused; tui_stop
#
# TUI_OUT is the output folder (default /tmp/tui). PNGs need `freeze`
# (`nix shell nixpkgs#charm-freeze`). The program runs without the caller's
# BORANT_* and CLAUDE* variables, so a session under test never registers as
# the agent that runs it and never sees its credentials.

TUI_OUT="${TUI_OUT:-/tmp/tui}"

# Flags for a Claude Code session under test. It must not act on a prompt the
# mod submits (default permission mode asks first), and it must not reach the
# real sessions on this machine: Claude Code lets a session list and message
# the others, and removing variables does not hide them.
# shellcheck disable=SC2034 # used by the scripts that source this file
TUI_CLAUDE_FLAGS=(--permission-mode default --disallowedTools SendMessage ListAgents)

# tui_start <name> <width> <height> <command...>: start the program in a new
# tmux server named <name>, at a fixed size, with the mouse on.
tui_start() {
  TUI_NAME="$1" TUI_W="$2" TUI_H="$3"
  TUI_PNG_H=0
  shift 3
  mkdir -p "$TUI_OUT"
  local run unset=()
  run="$(mktemp -d)"
  while IFS='=' read -r var _; do
    case "$var" in BORANT_* | CLAUDE*) unset+=(-u "$var") ;; esac
  done < <(env)
  env "${unset[@]}" -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor FORCE_COLOR=3 \
    tmux -L "$TUI_NAME" new-session -d -s tui -x "$TUI_W" -y "$TUI_H" -c "${TUI_CWD:-$run}" "$(printf '%q ' "$@")"
  tmux -L "$TUI_NAME" set -g mouse on
  # Without focus events Claude Code draws a notice over the band.
  tmux -L "$TUI_NAME" set -g focus-events on
}

# tui_keys <keys...>: tmux key names (C-x, Tab, Enter, Escape, Down).
tui_keys() { tmux -L "$TUI_NAME" send-keys -t tui "$@"; }
# tui_type <text>: literal text, also raw escape sequences.
tui_type() { tmux -L "$TUI_NAME" send-keys -t tui -l "$1"; }
# tui_text: the screen as plain text.
tui_text() { tmux -L "$TUI_NAME" capture-pane -p -t tui; }

# tui_wait <text> [tries]: wait up to tries x 0.5 s for text on the screen.
tui_wait() {
  local _
  for _ in $(seq 1 "${2:-60}"); do
    tui_text | grep -q -- "$1" && return 0
    sleep 0.5
  done
  echo "tui_wait: timed out waiting for: $1" >&2
  return 1
}

# tui_png_height <file>: a PNG's height in pixels, from its header.
tui_png_height() {
  od -An -tu1 -j20 -N4 "$1" 2>/dev/null | awk '{ print $1 * 16777216 + $2 * 65536 + $3 * 256 + $4 }'
}

# tui_shot <name>: write <name>.txt (ANSI), <name>.plain.txt, and <name>.png.
# freeze sometimes cuts the last rows off under load. Every full shot of one
# session has the same height, so a shorter one is taken again.
tui_shot() {
  tmux -L "$TUI_NAME" capture-pane -e -p -t tui >"$TUI_OUT/$1.txt"
  tmux -L "$TUI_NAME" capture-pane -p -t tui >"$TUI_OUT/$1.plain.txt"
  local _ height
  for _ in 1 2 3 4; do
    if freeze --execute "cat '$TUI_OUT/$1.txt'" -o "$TUI_OUT/$1.png" --margin=12 >/dev/null 2>&1; then
      height="$(tui_png_height "$TUI_OUT/$1.png")"
      if [ "${height:-0}" -ge "${TUI_PNG_H:-0}" ]; then
        TUI_PNG_H="$height"
        return 0
      fi
    fi
    sleep 1
  done
  echo "tui_shot: no full PNG for $1 (height ${height:-none}, expected ${TUI_PNG_H:-?})" >&2
}

# tui_find <text>: "<column> <row>" (1-based) of the first cell of text.
tui_find() {
  tui_text | TEXT="$1" perl -CS -ne 'BEGIN { $t = $ENV{TEXT}; utf8::decode($t) } my $i = index($_, $t); if ($i >= 0) { print $i + 1, " ", $., "\n"; exit }'
}

# tui_click <column> <row>: a left click, as an SGR mouse press and release.
tui_click() {
  tui_type $'\e[<0;'"$1;$2"'M'
  tui_type $'\e[<0;'"$1;$2"'m'
}

# tui_drag <from-column> <to-column> <row>: press, move in steps, release.
tui_drag() {
  local x="$1" to="$2" row="$3" step=3
  [ "$x" -gt "$to" ] && step=-3
  tui_type $'\e[<0;'"$x;$row"'M'
  while [ $(((to - x) * step)) -gt 0 ]; do
    x=$((x + step))
    tui_type $'\e[<32;'"$x;$row"'M'
    sleep 0.05
  done
  tui_type $'\e[<0;'"$to;$row"'m'
}

# tui_column <row> <char>: the 0-based column of char in a row, or -1.
tui_column() {
  tui_text | sed -n "$1p" | CHAR="$2" perl -CS -ne 'BEGIN { $c = $ENV{CHAR}; utf8::decode($c) } print index($_, $c)'
}

tui_stop() { tmux -L "$TUI_NAME" kill-server 2>/dev/null || true; }
