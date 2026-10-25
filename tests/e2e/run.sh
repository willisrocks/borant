#!/usr/bin/env bash
# Runs the Docker multi-host scenarios in order. Each scenario builds on the
# state of the one before it.
#
#   tests/e2e/run.sh                 build, start, run all, stop
#   BUILD=0 tests/e2e/run.sh ...     use the image that is already there
#   KEEP=1 tests/e2e/run.sh          leave the containers running afterward
#   tests/e2e/run.sh --all           print the name of every scenario
#   tests/e2e/run.sh --affected BASE the scenarios that BASE...HEAD touches
#   tests/e2e/run.sh elastic         run the elastic scenario alone
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

SCENARIOS=${*:-$ALL}
# The elastic scenario swaps the swarm for a hub with no worker hosts, so it
# runs alone and never in the default list.
for s in $SCENARIOS; do
  if [ "$s" = "elastic" ]; then SCENARIOS=elastic; fi
done

cleanup() {
  if [ "${KEEP:-0}" != "1" ]; then
    down >/dev/null 2>&1
  fi
}
trap cleanup EXIT

echo "Building the image..."
down >/dev/null 2>&1
# CI builds the image with the Docker layer cache and passes BUILD=0, so the
# swarm starts at once.
if [ "${BUILD:-1}" = "1" ]; then
  dc build hub >/dev/null || { echo "FAIL build"; exit 1; }
fi

if [ "$SCENARIOS" = "elastic" ]; then
  echo "Starting the elastic swarm..."
  dc --profile elastic up -d nats git hub-elastic host-control >/dev/null \
    || { echo "FAIL start"; dc logs --no-color 2>/dev/null | tail -80; exit 1; }
  # The `borant` CLI talks to a host daemon, so every query goes through the
  # control host, never through the hub container.
  hub_answers() { borant_on host-control hosts >/dev/null 2>&1; }
  if ! wait_for "the hub" hub_answers; then
    echo "  the control host does not reach the hub:"
    borant_on host-control hosts 2>&1 | tail -5
    dc logs --no-color 2>/dev/null | tail -80
    exit 1
  fi
else
  echo "Starting the swarm..."
  dc up -d nats git hub host-a host-b host-c >/dev/null \
    || { echo "FAIL start"; dc logs --no-color 2>/dev/null | tail -80; exit 1; }
  hosts_online() { borant_on host-a hosts | jq -e 'length == 3 and all(.online)'; }
  wait_for "host daemons" hosts_online || exit 1
fi

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
  wait_for "w1 closed build" log_has host-b w1 "CLOSE $build ok" || return 1
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

# The elastic scenario of the elastic compute design ("Tests"): a hub with no
# pool hosts and one pool of two hosts of two agents, which the hub starts
# with the provision-docker plugin. It runs alone, after its own swarm.
scenario_elastic() {
  # The static host-control holds the boss and is full, so the spawns of the
  # boss must go to the pool.
  start_agent host-control boss
  wait_for "boss on the control host" boss_on_control || return 1
  borant_on host-control send boss "CHANNEL e2e" >/dev/null
  wait_for "channel e2e" log_has host-control boss "CHANNEL e2e ok" || return 1

  local i
  for i in 1 2 3; do
    borant_on host-control send boss "SPAWN e2e wait-for-work" >/dev/null
  done
  wait_for "three spawns" three_spawns || return 1
  wait_for "two pool hosts" two_pool_hosts || return 1
  wait_for "three agents in the pool" three_in_pool || return 1
  echo "  two containers hold three agents"

  # The agents have no work, so they park and their hosts go away.
  WAIT=300 wait_for "the idle hosts go away" no_pool_hosts || return 1
  echo "  the agents parked and their containers are gone"

  # A killed container is lost: the hub deletes it and starts another host for
  # the agent that it held.
  borant_on host-control send boss "SPAWN e2e killed-during-a-turn" >/dev/null
  wait_for "a pool host for the new agent" one_pool_host || return 1
  local agent host id
  agent=$(spawned_agents | tail -1)
  host=$(where_on host-control "$agent")
  id=$(docker ps --quiet --filter "name=^${host}$")
  [ -n "$id" ] || { echo "  no container is named $host" >&2; return 1; }
  borant_on host-control send "$agent" "WRITE killed.txt during-a-turn" >/dev/null
  docker kill "$id" >/dev/null || return 1
  echo "  killed $host"
  WAIT=300 wait_for "a takeover on another host" agent_moved "$agent" "$host" || return 1
  wait_for "the killed host is deleted" container_gone "$id" || return 1

  # The cleanup removes every container of the fake provisioner.
  cleanup_e2e_containers
  [ "$(e2e_containers_count)" = "0" ] || { echo "  the cleanup left containers" >&2; return 1; }
}

boss_on_control() { [ "$(where_on host-control boss)" = "host-control" ]; }

spawned_agents() { log_of host-control boss | awk '$1=="SPAWNED" {print $2}'; }

three_spawns() { [ "$(spawned_agents | wc -l)" -ge 3 ]; }

two_pool_hosts() { [ "$(e2e_containers_count)" = "2" ]; }

one_pool_host() { [ "$(e2e_containers_count)" = "1" ]; }

no_pool_hosts() { [ "$(e2e_containers_count)" = "0" ]; }

# The agents that hold a lease on a pool host.
in_pool() {
  borant_on host-control agents 2>/dev/null |
    jq -e --argjson n "$1" '[.[] | select(.lease.state=="held" and (.lease.host | startswith("e2e-")))] | length >= $n' >/dev/null
}

three_in_pool() { in_pool 3; }

agent_moved() {
  local now
  now=$(where_on host-control "$1")
  [ -n "$now" ] && [ "$now" != "$2" ]
}

container_gone() { ! docker inspect "$1" >/dev/null 2>&1; }

for s in $SCENARIOS; do
  if "scenario_$s"; then pass "$s"; else fail "$s"; fi
done

if [ "$FAILED" != "0" ]; then
  echo "Some scenarios failed. Logs: E2E_ROOT=$ROOT docker compose -f $COMPOSE logs"
  if [ "${KEEP:-0}" != "1" ]; then
    dc logs --no-color 2>/dev/null | tail -150
  fi
  exit 1
fi
echo "All scenarios passed."
