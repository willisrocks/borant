#!/usr/bin/env bash
# The checks of scripts/tui/columns.mjs on known captures: a needs-you row's
# status that comes is no move; a project row that loses its last column, a
# row that moves down, and a row that goes are moves. Prints PASS or FAIL.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT
need='1: ▲ Approve the cutover window         borant · 1h'
needAsked='1: ▲ Approve the cutover window         borant · 1h      ✓ asked'
proj='7: ● notes          3 open tasks          ●1  ■1      orch-notes · online'
projCut='7: ● notes          3 open tasks          ●1  ■1'
printf '%s\n%s\n' "$need" "$proj" >"$t/a"
printf '%s\n%s\n' "$needAsked" "$proj" >"$t/status"
printf '%s\n%s\n' "$need" "$projCut" >"$t/cut"
printf '\n%s\n%s\n' "$need" "$proj" >"$t/down"
printf '%s\n' "$proj" >"$t/gone"
ok=0
expect() {
  local want="$1" case="$2" got
  shift 2
  if node "$here/columns.mjs" "$@" "$t/a" "$t/$case" >/dev/null; then got=pass; else got=fail; fi
  if [ "$got" = "$want" ]; then echo "PASS columns.mjs: '$case' $*: $([ "$want" = pass ] && echo 'no move' || echo 'a move')"
  else echo "FAIL columns.mjs: '$case' $* gave $got, expected $want"; ok=1; fi
}
expect pass status
expect fail cut
expect fail down
expect fail gone
expect pass gone --cells
exit $ok
