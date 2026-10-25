#!/bin/sh
# A provisioner exec plugin for tests: it keeps hosts in one JSON file and
# needs no network. The file comes from `config.file` in every request:
#
#   {"next": 2, "hosts": [<record>], "foreign": ["<id>"]}
#
# A record has the wire fields of "Provisioner ops" in
# docs/superpowers/specs/2026-10-24-elastic-compute-design.md. An id in
# `foreign` is a host of the backend that this plugin did not create, as a
# server without the ownership label is, so a delete of it is `denied`. Every
# write goes to a temporary file and a rename, so a reader never sees half a
# file.
set -eu

req=$(cat)

bad() { # bad CODE MESSAGE
  jq -n -c --arg code "$1" --arg message "$2" \
    '{error: {code: $code, message: $message}}'
  exit 0
}

op=$(printf '%s' "$req" | jq -r '.op // empty')
file=$(printf '%s' "$req" | jq -r '.config.file // empty')
[ -n "$op" ] || bad invalid "the request has no op"
# Every op needs the config, so a caller that forgets it fails everywhere.
[ -n "$file" ] || bad invalid "the config has no file"

tmp="$file.$$"
trap 'rm -f "$tmp" "$tmp.req"' EXIT

state() { # state makes the file when the backend has no host yet
  [ -f "$file" ] || printf '{"next": 1, "hosts": [], "foreign": []}\n' > "$file"
}

write_state() { # write_state REPLY_AND_STATE
  printf '%s' "$1" | jq '.state' > "$tmp"
  mv "$tmp" "$file"
}

case "$op" in
describe)
  # The file backend offers one size and one region.
  printf '%s\n' '{"ok": {"v": 1, "kind": "provisioner", "backend": "file", "regions": ["r1"], "sizes": [{"id": "small", "vcpus": 2, "mem_mb": 4096, "price_hour_usd": 0.1, "stopped_price_hour_usd": null}], "typical_start_s": 5, "typical_resume_s": null, "billing_step_s": 3600, "one_host_per_agent": false, "stop_modes": []}}'
  ;;
list)
  state
  pool=$(printf '%s' "$req" | jq -r '.pool // empty')
  [ -n "$pool" ] || bad invalid "the request has no pool"
  jq -c --arg pool "$pool" \
    '{ok: [(.hosts // [])[] | select(.pool == $pool)]}' "$file"
  ;;
create)
  state
  printf '%s' "$req" > "$tmp.req"
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  out=$(jq -n --slurpfile req "$tmp.req" --slurpfile st "$file" --arg now "$now" '
    ($st[0] // {}) as $s
    | {next: ($s.next // 1), hosts: ($s.hosts // []), foreign: ($s.foreign // [])} as $state
    | $req[0].spec as $spec
    | ([$state.hosts[] | select(.pool == $spec.pool and .name == $spec.name)] | first) as $same
    | if $spec == null then
        {state: $state,
         reply: {error: {code: "invalid", message: "the request has no spec"}}}
      elif $same != null then
        # A create is idempotent by pool and name, so a retried create
        # returns the host that the first one made.
        {state: $state, reply: {ok: $same}}
      elif ["small"] | index($spec.size) | not then
        {state: $state,
         reply: {error: {code: "invalid", message: "the file backend offers no size"}}}
      else
        ($state
          | .next as $n
          | .hosts += [{id: ("file-" + ($n | tostring)),
                        pool: $spec.pool,
                        name: $spec.name,
                        state: "running",
                        created: $now,
                        labels: ($spec.labels // {})}]
          | .next = ($n + 1)) as $next
        | {state: $next, reply: {ok: $next.hosts[-1]}}
      end')
  write_state "$out"
  printf '%s' "$out" | jq -c '.reply'
  ;;
delete)
  state
  id=$(printf '%s' "$req" | jq -r '.id // empty')
  [ -n "$id" ] || bad invalid "the request has no id"
  out=$(jq -n --slurpfile st "$file" --arg id "$id" '
    ($st[0] // {}) as $s
    | {next: ($s.next // 1), hosts: ($s.hosts // []), foreign: ($s.foreign // [])} as $state
    | if any($state.foreign[]; . == $id) then
        {state: $state,
         reply: {error: {code: "denied",
                         message: "the host was not created by this provisioner"}}}
      else
        # A host that is missing is Ok, so a retried delete succeeds.
        {state: ($state | .hosts = [.hosts[] | select(.id != $id)]),
         reply: {ok: null}}
      end')
  write_state "$out"
  printf '%s' "$out" | jq -c '.reply'
  ;;
*)
  # `stop` and `start` are not implemented: `stop_modes` is empty.
  bad invalid "the file backend does not implement this op"
  ;;
esac
