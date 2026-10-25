#!/usr/bin/env bash
# An exec provisioner for the Docker e2e scenario (spec "Provisioner ops" of
# docs/superpowers/specs/2026-10-24-elastic-compute-design.md). It starts one
# container of the e2e host image per pool host, so the hub's exec path, the
# pool rules, and the parking of an idle host run with no cloud.
#
# It needs only docker and jq:
#
#   describe  a neutral size and region, no stop modes, and a billing step of
#             5 s, so the idle hosts of the scenario go away quickly
#   create    docker run -d of the image, with the labels borant.pool=<pool>
#             and borant.managed=e2e, the name of the spec, and the boot
#             values as environment. Idempotent by name.
#   list      docker ps -a of the containers that carry the pool label
#   delete    docker rm -f, after the label check of "not ours is Denied"
#
# The `config` table of the provisioner in hub.toml is a flat map of strings,
# so a list or an object in it arrives as JSON text. The plugin reads both:
#
#   {"image": "borant:e2e", "network": "borant-e2e_default",
#    "command": "borant host" | ["borant", "host"],
#    "env.BORANT_NATS_URL": "..." | "env": {"BORANT_NATS_URL": "..."}}
set -uo pipefail

request=$(cat)
op=$(jq -r '.op // empty' <<<"$request")

ok() { jq -cn --argjson v "${1:-null}" '{ok: $v}'; }

err() {
  jq -cn --arg c "$1" --arg m "$2" '{error: {code: $c, message: $m}}'
  exit 0
}

# A string of the configuration table, or `default`.
cfg() {
  jq -r --arg k "$1" --arg d "$2" '(.config // {})[$k] // $d | tostring' <<<"$request"
}

# The argv of the command of the configuration: words, JSON text, or an array.
cfg_command() {
  jq -r '(.config // {})["command"] // empty
    | . as $v
    | if ($v | type) == "array" then $v[]
      else (try ($v | fromjson | .[]) catch ($v | split(" ") | .[])) end' <<<"$request" |
    sed '/^$/d'
}

# The environment of the container: the `env` key (an object or JSON text) and
# each `env.<KEY>` key of the flat table.
cfg_env() {
  jq -r '(.config // {}) as $c
    | (($c["env"] // {})
       | if type == "object" then .
         elif type == "string" and length > 0 then (try fromjson catch {})
         else {} end) as $e
    | ($e | to_entries[] | "\(.key)=\(.value)"),
      ($c | to_entries[] | select(.key | startswith("env."))
       | "\(.key | ltrimstr("env."))=\(.value)")' <<<"$request"
}

# The host record of a container that `docker inspect` describes. The states of
# docker map onto the states of the protocol. No `$` names here: the program
# goes into double quotes.
RECORD='def state: ({running: "running", created: "creating",
      restarting: "creating", paused: "stopped", exited: "stopped",
      removing: "deleting", dead: "failed"}[.State.Status]) // "failed";
{id: .Id[0:12], pool: (.Config.Labels["borant.pool"] // ""),
 name: (.Name | ltrimstr("/")), state: state, created: .Created,
 labels: (.Config.Labels // {})}'

describe() {
  ok '{"v": 1, "kind": "provisioner", "backend": "docker",
       "regions": ["local"],
       "sizes": [{"id": "small", "vcpus": 2, "mem_mb": 2048,
                  "price_hour_usd": 0, "stopped_price_hour_usd": null}],
       "typical_start_s": 2, "typical_resume_s": null, "billing_step_s": 5,
       "one_host_per_agent": false, "stop_modes": []}'
}

create() {
  local spec name pool size region image network record
  spec=$(jq -c '.spec // {}' <<<"$request")
  name=$(jq -r '.name // empty' <<<"$spec")
  pool=$(jq -r '.pool // empty' <<<"$spec")
  [ -n "$name" ] && [ -n "$pool" ] || err invalid "the spec needs a pool and a name"
  size=$(jq -r '.size // "small"' <<<"$spec")
  [ "$size" = "small" ] || err invalid "unknown size $size"
  region=$(jq -r '.region // "local"' <<<"$spec")
  [ "$region" = "local" ] || err invalid "unknown region $region"
  # A container of that name is the host of an earlier create: return it.
  if record=$(docker inspect "$name" 2>/dev/null); then
    ok "$(jq -c ".[0] | $RECORD" <<<"$record")"
    return
  fi
  image=$(jq -r '.image // empty' <<<"$spec")
  [ -n "$image" ] || image=$(cfg image "borant:e2e")
  network=$(cfg network "")
  local -a args=(run -d --name "$name" --hostname "$name"
    --label "borant.pool=$pool" --label "borant.managed=e2e")
  [ -n "$network" ] && args+=(--network "$network")
  local kv
  # The env table of the configuration first, then the boot values of the
  # spec, so a boot value wins over the same key.
  while IFS= read -r kv; do
    [ -n "$kv" ] && args+=(-e "$kv")
  done < <(cfg_env)
  while IFS= read -r kv; do
    [ -n "$kv" ] && args+=(-e "$kv")
  done < <(jq -r '(.boot // {}) | to_entries[] | "\(.key)=\(.value)"' <<<"$spec")
  local -a command
  mapfile -t command < <(cfg_command)
  [ "${#command[@]}" -gt 0 ] || command=(borant host)
  docker "${args[@]}" "$image" "${command[@]}" >/dev/null \
    || err internal "docker run $name failed"
  record=$(docker inspect "$name" 2>/dev/null) \
    || err internal "docker inspect $name after the run failed"
  ok "$(jq -c ".[0] | $RECORD" <<<"$record")"
}

list() {
  local pool=$1
  local -a ids
  mapfile -t ids < <(docker ps -a --quiet --filter "label=borant.pool=$pool")
  if [ "${#ids[@]}" -eq 0 ]; then
    ok '[]'
    return
  fi
  local out
  out=$(docker inspect "${ids[@]}" 2>/dev/null) || err internal "docker inspect failed"
  ok "$(jq -c "[ .[] | $RECORD ]" <<<"$out")"
}

delete() {
  local id=$1 record managed pool
  if ! record=$(docker inspect "$id" 2>/dev/null); then
    # A container that is already gone is fine.
    ok 'null'
    return
  fi
  managed=$(jq -r '.[0].Config.Labels["borant.managed"] // empty' <<<"$record")
  pool=$(jq -r '.[0].Config.Labels["borant.pool"] // empty' <<<"$record")
  if [ "$managed" != "e2e" ] || [ -z "$pool" ]; then
    err denied "container $id has no borant.managed=e2e and borant.pool labels"
  fi
  docker rm -f "$id" >/dev/null || err internal "docker rm -f $id failed"
  ok 'null'
}

case "$op" in
  describe) describe ;;
  create) create ;;
  list) list "$(jq -r '.pool // empty' <<<"$request")" ;;
  delete) delete "$(jq -r '.id // empty' <<<"$request")" ;;
  *) err invalid "unknown op ${op:-none}" ;;
esac
