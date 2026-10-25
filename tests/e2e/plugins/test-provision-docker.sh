#!/usr/bin/env bash
# Tests provision-docker.sh with a fake `docker` on PATH, which keeps
# containers in files and records its arguments. No Docker daemon, no network.
#
#   tests/e2e/plugins/test-provision-docker.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN="$HERE/provision-docker.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
BIN="$WORK/bin"
STATE="$WORK/state"
mkdir -p "$BIN" "$STATE/c"
FAILED=0

fail() {
  echo "FAIL $1"
  FAILED=1
}

pass() { echo "PASS $1"; }

check() {
  local what=$1 got=$2 want=$3
  if [ "$got" = "$want" ]; then pass "$what"; else fail "$what: got '$got', want '$want'"; fi
}

cat >"$BIN/docker" <<'STUB'
#!/usr/bin/env bash
# A fake docker: containers are JSON files, one per container.
set -uo pipefail
state=${FAKE_DOCKER:?}
id_of() { printf '%s' "$1" | sha256sum | cut -d' ' -f1; }
find_container() {
  local want=$1 file
  for file in "$state"/c/*.json; do
    [ -e "$file" ] || continue
    if [ "$(jq -r --arg w "$want" '(.Name == ("/" + $w)) or (.Id == $w) or (.Id | startswith($w))' "$file")" = "true" ]; then
      printf '%s\n' "$file"
      return 0
    fi
  done
  return 1
}
printf '%s\n' "$*" >>"$state/calls.log"
cmd=$1
shift
case "$cmd" in
run)
  [ "$1" = "-d" ] && shift
  name="" labels="" envs="" network=""
  while [ $# -gt 0 ]; do
    case "$1" in
    --name) name=$2; shift 2 ;;
    --hostname) shift 2 ;;
    --label) labels="$labels $2"; shift 2 ;;
    --network) network=$2; shift 2 ;;
    -e) envs="$envs $2"; shift 2 ;;
    *) break ;;
    esac
  done
  image=$1
  shift
  labels_json=$(printf '%s\n' $labels | jq -R 'split("=") | {(.[0]): (.[1:] | join("="))}' | jq -s 'add // {}')
  jq -n --arg name "$name" --arg id "$(id_of "$name")" --argjson labels "$labels_json" \
    --arg image "$image" --arg command "$*" --arg envs "$envs" --arg network "$network" \
    '{Id: $id, Name: ("/" + $name), Created: "2026-10-25T00:00:00Z",
      State: {Status: "running"},
      Config: {Image: $image, Labels: $labels, Env: ($envs | split(" ") | map(select(. != "")))}}
     | . + {Config: (.Config + {Network: $network, Command: $command})}' \
    >"$state/c/$name.json"
  jq -r '.Id' "$state/c/$name.json"
  ;;
inspect)
  files=()
  for want in "$@"; do
    file=$(find_container "$want") || { echo "no such container: $want" >&2; exit 1; }
    files+=("$file")
  done
  jq -s '.' "${files[@]}"
  ;;
ps)
  # `ps -a --quiet --filter label=borant.pool=<pool>`
  pool=""
  while [ $# -gt 0 ]; do
    case "$1" in
    --filter) pool=${2#label=borant.pool=}; shift 2 ;;
    *) shift ;;
    esac
  done
  for file in "$state"/c/*.json; do
    [ -e "$file" ] || continue
    if [ "$(jq -r --arg p "$pool" '(.Config.Labels["borant.pool"] // "") == $p' "$file")" = "true" ]; then
      jq -r '.Id' "$file"
    fi
  done
  ;;
rm)
  [ "$1" = "-f" ] && shift
  file=$(find_container "$1") || exit 1
  rm -f "$file"
  ;;
*)
  echo "fake docker: unknown command $cmd" >&2
  exit 1
  ;;
esac
STUB
chmod +x "$BIN/docker"

# Runs the plugin with one request and returns its reply.
plugin() {
  printf '%s' "$1" | FAKE_DOCKER="$STATE" PATH="$BIN:$PATH" "$PLUGIN"
}

calls() { cat "$STATE/calls.log" 2>/dev/null; }

if ! command -v jq >/dev/null; then
  echo "SKIP jq is not installed"
  exit 0
fi

# describe
reply=$(plugin '{"v": 1, "op": "describe", "config": {}}')
check "describe kind" "$(jq -r '.ok.kind' <<<"$reply")" "provisioner"
check "describe version" "$(jq -r '.ok.v' <<<"$reply")" "1"
check "describe size" "$(jq -r '.ok.sizes[0].id' <<<"$reply")" "small"
check "describe region" "$(jq -r '.ok.regions[0]' <<<"$reply")" "local"
check "describe stop modes" "$(jq -r '.ok.stop_modes | length' <<<"$reply")" "0"

# create
reply=$(plugin '{"v": 1, "op": "create", "config": {"network": "borant-e2e_default"},
  "spec": {"pool": "p1", "name": "n1", "size": "small", "region": "local",
           "labels": {"borant.pool": "p1"},
           "boot": {"BORANT_POOL": "p1", "BORANT_TAGS": "tag-a"}}}')
check "create pool" "$(jq -r '.ok.pool' <<<"$reply")" "p1"
check "create name" "$(jq -r '.ok.name' <<<"$reply")" "n1"
check "create state" "$(jq -r '.ok.state' <<<"$reply")" "running"
check "create id" "$(jq -r '.ok.id | length' <<<"$reply")" "12"
check "create labels" "$(jq -r '.ok.labels["borant.managed"]' <<<"$reply")" "e2e"
create_call=$(calls | grep '^run ' | tail -1)
for want in "--name n1" "--label borant.pool=p1" "--label borant.managed=e2e" \
  "-e BORANT_POOL=p1" "-e BORANT_TAGS=tag-a" "--network borant-e2e_default" \
  "borant:e2e borant host"; do
  case "$create_call" in
  *"$want"*) pass "create runs with $want" ;;
  *) fail "create runs with $want: $create_call" ;;
  esac
done

# An idempotent create: the same name returns the same container, no second run.
runs_before=$(calls | grep -c '^run ')
reply=$(plugin '{"v": 1, "op": "create", "config": {},
  "spec": {"pool": "p1", "name": "n1", "size": "small"}}')
check "create twice is idle" "$(calls | grep -c '^run ')" "$runs_before"
check "create twice returns the name" "$(jq -r '.ok.name' <<<"$reply")" "n1"
check "create twice keeps the id" "$(jq -r '.ok.id | length' <<<"$reply")" "12"

# The image of the spec wins over the image of the configuration.
plugin '{"v": 1, "op": "create", "config": {},
  "spec": {"pool": "p1", "name": "n3", "size": "small", "image": "borant:other"}}' >/dev/null
check "the spec image runs" "$(calls | grep '^run ' | tail -1 | grep -c 'borant:other')" "1"
plugin '{"v": 1, "op": "delete", "config": {}, "id": "n3"}' >/dev/null

# list
reply=$(plugin '{"v": 1, "op": "list", "config": {}, "pool": "p1"}')
check "list the pool" "$(jq -r '.ok | map(.name) | join(",")' <<<"$reply")" "n1"
reply=$(plugin '{"v": 1, "op": "list", "config": {}, "pool": "other"}')
check "list another pool" "$(jq -c '.ok' <<<"$reply")" "[]"

# The size and the region of the spec
reply=$(plugin '{"v": 1, "op": "create", "config": {},
  "spec": {"pool": "p1", "name": "n2", "size": "big"}}')
check "an unlisted size is invalid" "$(jq -r '.error.code' <<<"$reply")" "invalid"

# delete: not ours is denied
jq -n '{Id: "b1", Name: "/stranger", Created: "now", State: {Status: "running"},
        Config: {Labels: {"borant.pool": "p1"}}}' >"$STATE/c/stranger.json"
rms_before=$(calls | grep -c '^rm ')
reply=$(plugin '{"v": 1, "op": "delete", "config": {}, "id": "stranger"}')
check "delete without the label is denied" "$(jq -r '.error.code' <<<"$reply")" "denied"
check "delete without the label runs no rm" "$(calls | grep -c '^rm ')" "$rms_before"

# delete: missing is ok
reply=$(plugin '{"v": 1, "op": "delete", "config": {}, "id": "absent"}')
check "delete a missing container" "$(jq -c '.' <<<"$reply")" '{"ok":null}'

# delete: ours goes away
reply=$(plugin '{"v": 1, "op": "delete", "config": {}, "id": "n1"}')
check "delete ours" "$(jq -c '.' <<<"$reply")" '{"ok":null}'
check "delete ours removed the container" \
  "$(ls "$STATE/c" | tr '\n' ' ')" "stranger.json "

# An unknown op
reply=$(plugin '{"v": 1, "op": "stop", "config": {}, "id": "n1"}')
check "an unknown op is invalid" "$(jq -r '.error.code' <<<"$reply")" "invalid"

if [ "$FAILED" != "0" ]; then
  echo "Some checks failed."
  exit 1
fi
echo "All checks passed."
