#!/bin/sh
# A change-request exec plugin for tests: it serves the statuses of one JSON
# file, `{"<url>": <ChangeStatus>}`, named by `config.file` in every request,
# and needs no network. The wire fields are those of
# `borant_core::ports::ChangeStatus`.
set -eu

req=$(cat)

bad() { # bad CODE MESSAGE
  jq -n -c --arg code "$1" --arg message "$2" \
    '{error: {code: $code, message: $message}}'
  exit 0
}

op=$(printf '%s' "$req" | jq -r '.op // empty')
[ -n "$op" ] || bad invalid "the request has no op"

case "$op" in
describe)
  jq -n -c '{ok: {v: 1, kind: "change_requests"}}'
  ;;
status)
  file=$(printf '%s' "$req" | jq -r '.config.file // empty')
  [ -n "$file" ] || bad invalid "the config has no file"
  url=$(printf '%s' "$req" | jq -r '.url // empty')
  [ -n "$url" ] || bad invalid "the request has no url"
  [ -f "$file" ] || bad not_found "no change with this url"
  value=$(jq -c --arg u "$url" '.[$u] // empty' "$file")
  [ -n "$value" ] || bad not_found "no change with this url"
  jq -n -c --argjson v "$value" '{ok: $v}'
  ;;
*)
  bad invalid "unknown op $op"
  ;;
esac
