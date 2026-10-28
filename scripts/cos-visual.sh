#!/usr/bin/env bash
# Render the chief-of-staff mod in a real Claude Code session at two fixed
# sizes, drive it with keys, and write an ANSI capture and a PNG for each
# state. This is the loop for every mod or TUI change: unit tests do not paint,
# and a text capture without color does not show a highlight or a focus ring.
#
# Usage:
#   scripts/cos-visual.sh [plugin-dir] [output-dir] [prefix]
#
# The board comes from BORANT_COS_BOARD (default: fixture). A live board needs
# `borant` on PATH and BORANT_COS_BOARD=cli.
#
#   nix shell nixpkgs#charm-freeze -c scripts/cos-visual.sh \
#     integrations/claude-code docs/chief-mod-board-ux/captures after
#
# States: band, band-focused, pane-opened, pane-moved, at 200x50 and 120x40.
set -euo pipefail

plugin="${1:-integrations/claude-code}"
out="${2:-/tmp/cos-visual}"
prefix="${3:-}"

command -v claude >/dev/null || { echo "claude is not on PATH" >&2; exit 1; }
command -v tmux >/dev/null || { echo "tmux is not on PATH" >&2; exit 1; }
command -v freeze >/dev/null || {
  echo "freeze is not on PATH; run: nix shell nixpkgs#charm-freeze" >&2
  exit 1
}

plugin="$(realpath "$plugin")"
mkdir -p "$out"

for size in 200x50 120x40; do
  width="${size%x*}"
  height="${size#*x}"
  socket="cos-visual-$$-$width"
  name="cos"
  run="$(mktemp -d)"
  tag="${prefix:+$prefix-}$width"

  cleanup() {
    tmux -L "$socket" kill-server 2>/dev/null || true
    rm -rf "$run"
  }
  trap cleanup EXIT

  env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor \
    tmux -L "$socket" new-session -d -s "$name" -x "$width" -y "$height" -c "$run" \
    "env -u NO_COLOR FORCE_COLOR=3 TERM=xterm-256color COLORTERM=truecolor BORANT_ROLE=chief BORANT_COS_BOARD=${BORANT_COS_BOARD:-fixture} claude --plugin-dir '$plugin'"

  tmux_send() { tmux -L "$socket" send-keys -t "$name" "$@"; }
  tmux_text() { tmux -L "$socket" capture-pane -p -t "$name"; }

  wait_for() {
    local text="$1" tries="${2:-40}"
    for _ in $(seq 1 "$tries"); do
      if tmux_text | grep -q -- "$text"; then return 0; fi
      sleep 0.5
    done
    echo "timed out waiting for: $text" >&2
    tmux_text >&2
    return 1
  }

  shot() {
    local state="$1"
    tmux -L "$socket" capture-pane -e -p -t "$name" >"$out/$tag-$state.txt"
    tmux -L "$socket" capture-pane -p -t "$name" >"$out/$tag-$state.plain.txt"
    freeze --execute "cat '$out/$tag-$state.txt'" -o "$out/$tag-$state.png" --margin=12 >/dev/null
  }

  wait_for 'Claude Code'
  # Let session.start register /cos before the prompt autocompletes it.
  sleep 4
  tmux_send -l '/cos'
  tmux_send Enter
  wait_for 'chief of staff'
  wait_for 'cos · 2 need you'

  shot band

  # The band's focus starts on its help chip; Tab and Down reach a project row.
  tmux_send C-x Tab
  sleep 0.5
  tmux_send Tab
  sleep 0.5
  tmux_send Down
  sleep 0.5
  shot band-focused

  # Enter opens the pane and asks for its keyboard.
  tmux_send Enter
  sleep 1.5
  shot pane-opened

  # j moves through the epics and tasks.
  tmux_send -l 'j'
  sleep 0.5
  shot pane-moved

  cleanup
  trap - EXIT
done

echo "wrote $out"
