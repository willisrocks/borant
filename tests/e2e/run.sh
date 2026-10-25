#!/usr/bin/env bash
# Runs the Docker multi-host scenarios in order. Each scenario builds on the
# state of the one before it.
#
#   tests/e2e/run.sh                 build, start, run all, stop
#   BUILD=0 tests/e2e/run.sh ...     use the image that is already there
#   KEEP=1 tests/e2e/run.sh          leave the containers running afterward
#   tests/e2e/run.sh --all           print the name of every scenario
#   tests/e2e/run.sh --affected BASE the scenarios that BASE...HEAD touches
set -uo pipefail
source "$(dirname "$0")/lib.sh"
FAILED=0

ALL="messaging tasks takeover fencing planned_move drain hub_restart hub_move"

# The scenarios that a diff touches. Every scenario drives the same swarm, so
# a change to one crate can break all of them: only the paths that no
# scenario loads skip the run, and every other path runs the whole set.
#
#   docs/, *.md, integrations/, crates/borant-memory/, LICENSE -> none
#   everything else, and any change under crates/borant-core,
#   crates/borant-hub, crates/borant-nats, docker/, or tests/e2e/lib.sh -> all
affected() {
  local base=$1 files
  files=$(git diff --name-only "$base"...HEAD) || return 1
  if [ -z "$files" ]; then
    echo "$ALL"
    return 0
  fi
  if printf '%s\n' "$files" |
    grep -qvE '^(docs/|integrations/|crates/borant-memory/|LICENSE|[^/]*\.md$)'; then
    echo "$ALL"
  fi
}

case "${1:-}" in
  --all)
    echo "$ALL"
    exit 0
    ;;
  --affected)
    affected "${2:?a base ref is needed, for example origin/main}"
    exit $?
    ;;
esac

cleanup() {
  if [ "${KEEP:-0}" != "1" ]; then
    down >/dev/null 2>&1
  fi
}
trap cleanup EXIT

echo "Building and starting the swarm..."
down >/dev/null 2>&1
# CI builds the image with the Docker layer cache and passes BUILD=0, so the
# swarm starts at once.
if [ "${BUILD:-1}" = "1" ]; then
  dc build hub >/dev/null || { echo "FAIL build"; exit 1; }
fi
dc up -d nats git hub host-a host-b host-c >/dev/null || { echo "FAIL start"; exit 1; }
hosts_online() { borant_on host-a hosts | jq -e 'length == 3 and all(.online)'; }
wait_for "host daemons" hosts_online || exit 1

scenario_messaging() {
  start_agent host-a boss
  start_agent host-b w1
  wait_for "boss registered" is_at boss host-a || return 1
  wait_for "w1 registered" is_at w1 host-b || return 1
  borant_on host-c send boss "CHANNEL demo" >/dev/null
  wait_for "channel created" log_has host-a boss "CHANNEL demo ok" || return 1
  borant_on host-c send w1 "JOIN demo" >/dev/null
  wait_for "w1 joined" log_has host-b w1 "JOIN demo ok" || return 1
  borant_on host-a send w1 "SEND boss hi-from-w1" >/dev/null
  wait_for "direct message across hosts" log_has host-a boss "hi-from-w1" || return 1
  borant_on host-c send "#demo" "team-broadcast" --urgent >/dev/null
  wait_for "urgent broadcast reaches w1" log_has host-b w1 "team-broadcast" || return 1
}

scenario_tasks() {
  borant_on host-c send boss "CREATE demo build" >/dev/null
  wait_for "task build" log_has host-a boss "CREATED .* build" || return 1
  local build
  build=$(task_id host-a boss build)
  borant_on host-c send boss "AFTER demo $build test" >/dev/null
  wait_for "task test" log_has host-a boss "CREATED .* test" || return 1
  local test
  test=$(task_id host-a boss test)
  [ "$(borant_on host-a tasks demo --ready | jq -r '.[].title')" = "build" ] || { echo "  only build is ready" >&2; return 1; }
  borant_on host-c send w1 "CLAIM demo $build
CLOSE demo $build" >/dev/null
  wait_for "w1 cannot close as done" log_has host-b w1 "CLOSE $build failed" || return 1
  borant_on host-c send boss "CLOSE demo $build" >/dev/null
  wait_for "the owner closed build" log_has host-a boss "CLOSE $build ok" || return 1
  test_ready() { borant_on host-a tasks demo --ready | jq -e '.[0].title == "test"'; }
  wait_for "test is ready" test_ready || return 1
  wait_for "the owner woke for the close" log_has host-a boss "kind=Event.*closed" || return 1
  TEST_TASK=$test
}

scenario_takeover() {
  borant_on host-c send w1 "CLAIM demo $TEST_TASK
WRITE notes.txt important-work" >/dev/null
  wait_for "w1 wrote notes" log_has host-b w1 "WROTE notes.txt" || return 1
  wait_for "checkpoint pushed" ref_exists refs/borant/w1/wip || return 1
  sleep 1
  docker network disconnect "$NETWORK" "$(container host-b)" || return 1
  borant_on host-a send w1 "WRITE after.txt sent-while-away" >/dev/null
  WAIT=90 wait_for "w1 moved off host-b" not_at w1 host-b || return 1
  local h
  h=$(where w1)
  echo "  w1 now runs on $h"
  wait_for "new session read its history" log_has "$h" w1 "HISTORY total=" || return 1
  [ "$(on "$h" cat /root/.borant/work/w1/demo/notes.txt)" = "important-work" ] || { echo "  notes.txt not restored" >&2; return 1; }
  wait_for "the message sent while away arrived" log_has "$h" w1 "WROTE after.txt" || return 1
  W1_HOST=$h
}

scenario_fencing() {
  docker network connect "$NETWORK" "$(container host-b)" || return 1
  wait_for "the old session on host-b stopped" log_has host-b w1 SIGTERM || return 1
  wait_for "stale ref saved" ref_exists refs/borant/w1/stale/ || return 1
  is_at w1 "$W1_HOST" || { echo "  w1 must stay on $W1_HOST" >&2; return 1; }
}

scenario_planned_move() {
  start_agent host-c w2 demo
  wait_for "w2 registered" is_at w2 host-c || return 1
  borant_on host-a move w2 --host host-b >/dev/null || return 1
  wait_for "w2 on host-b" is_at w2 host-b || return 1
  wait_for "w2 continued on host-b" log_has host-b w2 "You continue Borant agent w2" || return 1
  wait_for "the old w2 stopped on host-c" log_has host-c w2 SIGTERM || return 1
}

scenario_drain() {
  borant_on host-a drain host-b >/dev/null || return 1
  wait_for "w2 left host-b" not_at w2 host-b || return 1
}

scenario_hub_restart() {
  local before after
  before=$(borant_on host-a agents | jq -c '[.[] | {agent, host: .lease.host}] | sort_by(.agent)')
  dc restart hub >/dev/null
  sleep 25
  after=$(borant_on host-a agents | jq -c '[.[] | {agent, host: .lease.host}] | sort_by(.agent)')
  [ "$before" = "$after" ] || { echo "  agents moved: $before -> $after" >&2; return 1; }
}

scenario_hub_move() {
  borant_on host-c send boss "CREATE demo survives-move" >/dev/null
  wait_for "task survives-move" log_has host-a boss "CREATED .* survives-move" || return 1
  start_agent host-c ghost demo
  wait_for "ghost registered" is_at ghost host-c || return 1
  borant_on host-a send ghost "EXIT" >/dev/null
  wait_for "ghost exited" log_has host-c ghost "EXIT" || return 1
  borant_on host-a send ghost "hello-ghost" >/dev/null
  sleep 1
  borant_on hub hub export --out /tmp/export.jsonl || return 1
  local tmp
  tmp=$(mktemp -d)
  docker cp "$(container hub):/tmp/export.jsonl" "$tmp/export.jsonl" >/dev/null || return 1
  dc stop hub nats >/dev/null
  dc --profile move up -d nats2 hub2 >/dev/null || return 1
  docker cp "$tmp/export.jsonl" "$(container hub2):/tmp/export.jsonl" >/dev/null || return 1
  WAIT=30 wait_for "import into the new broker" on hub2 borant hub import /tmp/export.jsonl || return 1
  dc exec -d -T hub2 borant hub
  task_survived() { borant_on host-a tasks demo | jq -e 'any(.title == "survives-move")'; }
  WAIT=90 wait_for "hosts see the moved hub" task_survived || return 1
  start_agent host-c ghost
  WAIT=90 wait_for "the pending message survived the move" log_has host-c ghost "hello-ghost" || return 1
}

ALL="messaging tasks takeover fencing planned_move drain hub_restart hub_move"
for s in ${*:-$ALL}; do
  if "scenario_$s"; then pass "$s"; else fail "$s"; fi
done

if [ "$FAILED" != "0" ]; then
  echo "Some scenarios failed. Logs: docker compose -f docker/compose.yaml logs"
  if [ "${KEEP:-0}" != "1" ]; then
    dc logs --no-color hub host-a host-b host-c 2>/dev/null | tail -150
  fi
  exit 1
fi
echo "All scenarios passed."
