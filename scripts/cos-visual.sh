#!/usr/bin/env bash
# Render the chief-of-staff mod in a real Claude Code session at two fixed
# sizes, drive it with keys, and write an ANSI capture and a PNG for each
# state. The tui-visual-qa skill (.agents/skills/tui-visual-qa) describes the
# loop; scripts/tui/cos-scenario.sh covers mouse, chords, and refresh.
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

root="$(cd "$(dirname "$0")/.." && pwd)"
plugin="$(realpath "${1:-integrations/claude-code}")"
export TUI_OUT="${2:-/tmp/cos-visual}"
prefix="${3:-}"

command -v claude >/dev/null || { echo "claude is not on PATH" >&2; exit 1; }
command -v tmux >/dev/null || { echo "tmux is not on PATH" >&2; exit 1; }
command -v freeze >/dev/null || {
  echo "freeze is not on PATH; run: nix shell nixpkgs#charm-freeze" >&2
  exit 1
}

source "$root/scripts/tui/lib.sh"
mkdir -p "$TUI_OUT"
trap tui_stop EXIT

for size in 200x50 120x40; do
  width="${size%x*}"
  height="${size#*x}"
  tag="${prefix:+$prefix-}$width"

  tui_start "cos-visual-$$-$width" "$width" "$height" env PATH="$PATH" BORANT_ROLE=chief \
    BORANT_COS_BOARD="${BORANT_COS_BOARD:-fixture}" claude --plugin-dir "$plugin"

  tui_wait 'Claude Code'
  # Let session.start register /cos before the prompt autocompletes it.
  sleep 4
  tui_type '/cos'
  tui_keys Enter
  tui_wait 'chief of staff'
  tui_wait 'cos · 2 need you'

  tui_shot "$tag-band"

  # The band's focus starts on its help chip; j j reaches the first project.
  tui_keys C-x Tab
  sleep 0.5
  tui_type 'j'
  sleep 0.3
  tui_type 'j'
  sleep 0.5
  tui_shot "$tag-band-focused"

  # Enter opens the pane and gives it the keyboard.
  tui_keys Enter
  sleep 1.5
  tui_shot "$tag-pane-opened"

  # j moves through the epics and tasks.
  tui_type 'j'
  sleep 0.5
  tui_shot "$tag-pane-moved"

  tui_stop
done

echo "wrote $TUI_OUT"
