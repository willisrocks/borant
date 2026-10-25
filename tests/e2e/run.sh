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

ALL="messaging tasks takeover fencing planned_move drain hub_restart hub_move sandbox"

# The paths of the sandbox: its two sources, its own swarm, and the shared
# image that carries bubblewrap.
SANDBOX_PATHS='^(crates/borant-launch/src/sandbox\.rs|crates/borant-host/src/sandbox\.rs|tests/e2e/sandbox/|docker/Dockerfile$|docker/compose\.yaml$|crates/borant-testkit/src/bin/borant-fake-harness\.rs$)'

# The scenarios that a diff touches. Every scenario drives the same swarm, so
# a change to one crate can break all of them: only the paths that no
# scenario loads skip the run, and every other path runs the whole set.
#
#   docs/, *.md, integrations/, crates/borant-memory/, LICENSE -> none
#   crates/borant-hub/src/members.rs, crates/borant-core/src/rules/members.rs
#     -> members (the only scenario that builds a bundle)
#   everything else, and any change under crates/borant-core,
#   crates/borant-hub, crates/borant-nats, docker/, or tests/e2e/lib.sh -> all
#   the sandbox and its scenario -> sandbox, or all when other paths changed
affected() {
  local base=$1 files
  files=$(git diff --name-only "$base"...HEAD) || return 1
  if [ -z "$files" ]; then
    echo "$ALL"
    return 0
  fi
  # A diff of the sandbox or its scenario needs that scenario. When nothing
  # else changed, it runs alone; otherwise the default set runs first and the
  # script then runs itself for the sandbox swarm.
  if printf '%s\n' "$files" | grep -qE "$SANDBOX_PATHS"; then
    if printf '%s\n' "$files" | grep -qvE "$SANDBOX_PATHS"; then
      echo "$ALL"
    else
      echo sandbox
    fi
    return 0
  fi
  if printf '%s\n' "$files" |
    grep -qvE '^(docs/|integrations/|crates/borant-memory/|LICENSE|[^/]*\.md$|crates/borant-hub/src/members\.rs|crates/borant-core/src/rules/members\.rs)'; then
    echo "$ALL"
    return 0
  fi
  if printf '%s\n' "$files" |
    grep -qE '^(crates/borant-hub/src/members\.rs|crates/borant-core/src/rules/members\.rs)'; then
    echo members
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
# The elastic and members scenarios bring their own swarm (a hub with no
# worker hosts, and one host with a bundle), so each runs alone. The sandbox
# scenario has its own swarm as well, and it can share a run with the default
# set: the default scenarios run first in a child, then this script continues
# with the sandbox swarm.
for s in $SCENARIOS; do
  case "$s" in
    elastic|members) SCENARIOS=$s ;;
  esac
done
case " $SCENARIOS " in
  *" sandbox "*)
    others=$(printf '%s\n' $SCENARIOS | grep -vx sandbox | tr '\n' ' ')
    if [ -n "$others" ]; then
      # The word splitting is the point: one argument per scenario.
      # shellcheck disable=SC2086
      "$0" $others || exit 1
    fi
    SCENARIOS=sandbox
    ;;
esac

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

if [ "$SCENARIOS" = "sandbox" ]; then
  # One host without [sandbox], so the first sandboxed spawn is refused; the
  # scenario starts the sandbox host itself after that.
  echo "Starting the sandbox swarm..."
  dc --profile sandbox up -d nats git hub-sandbox host-a >/dev/null \
    || { echo "FAIL start"; dc --profile sandbox logs --no-color 2>/dev/null | tail -200; exit 1; }
  hosts_online() { borant_on host-a hosts | jq -e 'length == 1 and all(.online)'; }
  wait_for "host daemons" hosts_online || exit 1
elif [ "$SCENARIOS" = "elastic" ]; then
  echo "Starting the elastic swarm..."
  dc --profile elastic up -d nats git hub-elastic host-control >/dev/null \
    || { echo "FAIL start"; dc --profile elastic logs --no-color 2>/dev/null | tail -200; exit 1; }
  # The `borant` CLI talks to a host daemon, so every query goes through the
  # control host, never through the hub container.
  hub_answers() { borant_on host-control hosts >/dev/null 2>&1; }
  if ! wait_for "the hub" hub_answers; then
    echo "  the control host does not reach the hub:"
    borant_on host-control hosts 2>&1 | tail -5
    dc --profile elastic logs --no-color hub-elastic host-control 2>/dev/null | tail -200
    exit 1
  fi
  # The provisioner must run in the hub container with docker, jq, and the
  # socket, and answer the two ops of the scenario.
  provisioner_ok() {
    on hub-elastic sh -c '
      command -v docker >/dev/null && command -v jq >/dev/null || exit 1
      docker ps -q >/dev/null || exit 1
      echo "{\"v\":1,\"op\":\"describe\",\"config\":{}}" |
        /plugins/provision-docker.sh | jq -e ".ok.kind == \"provisioner\"" >/dev/null || exit 1
      echo "{\"v\":1,\"op\":\"list\",\"config\":{},\"pool\":\"e2e\"}" |
        /plugins/provision-docker.sh | jq -e ".ok | type == \"array\"" >/dev/null
    ' >/dev/null 2>&1
  }
  if ! wait_for "the provisioner" provisioner_ok; then
    echo "  the provisioner of the hub does not answer:"
    on hub-elastic sh -c 'command -v docker; command -v jq; docker ps -q >/dev/null && echo socket ok'
    on hub-elastic sh -c 'echo "{\"v\":1,\"op\":\"describe\",\"config\":{}}" | /plugins/provision-docker.sh'
    dc --profile elastic logs --no-color hub-elastic host-control 2>/dev/null | tail -200
    exit 1
  fi
elif [ "$SCENARIOS" = "members" ]; then
  echo "Starting the members swarm..."
  dc --profile members up -d nats git hub host-members >/dev/null \
    || { echo "FAIL start"; dc --profile members logs --no-color 2>/dev/null | tail -200; exit 1; }
  hosts_online() { borant_on host-members hosts | jq -e 'length == 1 and all(.online)'; }
  wait_for "host daemons" hosts_online || exit 1
else
  echo "Starting the swarm..."
  dc up -d nats git hub host-a host-b host-c >/dev/null \
    || { echo "FAIL start"; dc --profile elastic logs --no-color 2>/dev/null | tail -200; exit 1; }
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

# The sandbox scenario of phase 5, on its own swarm (see the start block). The
# hub has a role that asks for a namespace sandbox, so the first spawn waits
# with `no sandbox` while only a host without [sandbox] is up; the sandbox host
# then takes it, and the agent inside its namespace reads its own clone and
# neither the host's marker file nor a sibling's clone.
scenario_sandbox() {
  local target=host-sandbox

  start_agent host-a boss
  wait_for "boss registered" is_at boss host-a || return 1
  borant_on host-a project add demo git://git/demo.git --branch main >/dev/null || return 1
  borant_on host-a send boss "CHANNEL demo demo" >/dev/null
  wait_for "the channel with its project" log_has host-a boss "CHANNEL demo ok" || return 1

  # A spawn needs a session, so the boss asks for the sandboxed agent. Only
  # host-a is up, and it offers no sandbox: the spawn waits, and the hub says
  # why.
  borant_on host-a send boss "SPAWNROLE demo sandboxed look around" >/dev/null
  wait_for "the spawn refused for no sandbox" refused_no_sandbox || return 1

  # The one host that offers the sandbox comes up. When the container refuses
  # a user namespace, the host drops its sandbox at start and the scenario
  # skips instead of failing (phase 5 spec, the e2e task).
  dc --profile sandbox up -d host-sandbox >/dev/null || return 1
  sandbox_host_up() {
    borant_on host-a hosts 2>/dev/null | jq -e 'length == 2 and all(.online)'
  }
  wait_for "the sandbox host" sandbox_host_up || return 1
  if ! sandbox_offered; then
    echo "  the host dropped its sandbox:"
    dc logs --no-color host-sandbox 2>/dev/null | grep "start check failed" | tail -1
    echo "  bwrap inside the host container:"
    on host-sandbox sh -c 'bwrap --ro-bind / / true; echo "exit=$?"' 2>&1 | tail -3
    on host-sandbox sh -c \
      'cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null;
       cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null'
    echo "SKIP sandbox: bwrap cannot create a user namespace in this container"
    return 0
  fi

  wait_for "the spawn on the sandbox host" boxed_on_sandbox || return 1
  local boxed
  boxed=$(boxed_id)
  echo "  $boxed runs on $target"
  wait_for "the clone of the sandboxed agent" cloned "$boxed" || return 1

  # A marker outside the clone, and a sibling agent with a clone of its own on
  # the same host.
  on "$target" sh -c 'mkdir -p /root/.ssh && echo host-secret > /root/.ssh/marker' || return 1
  start_agent "$target" sib
  wait_for "the sibling registered" is_at sib "$target" || return 1
  borant_on host-a send sib "WRITE notes.txt sibling-secret" >/dev/null
  wait_for "the sibling clone" log_has "$target" sib "WROTE notes.txt" || return 1

  borant_on host-a send "$boxed" "WRITE own.txt mine
READ own.txt
READ /root/.ssh/marker
READ /work/sib/notes.txt" >/dev/null
  wait_for "the sandboxed reads" sandbox_reads "$boxed" || return 1
  echo "  the agent read only its own clone:"
  on "$target" cat "/root/.borant/work/$boxed/demo/reads.log" || return 1
}

# The agent that the boss spawned, from the log line `SPAWNED <agent>`.
boxed_id() { log_of host-a boss | awk '$1=="SPAWNED" {id=$2} END {print id}'; }

# The pending spawn names why no host takes it: its role asks for the sandbox
# and the only host has none. Before the spawn queue lands the placement event
# holds it; with the queue it is in the record that `borant ps --queued` prints.
refused_no_sandbox() {
  local boxed events queue
  boxed=$(boxed_id)
  [ -n "$boxed" ] || return 1
  events=$(borant_on host-a events --json --kind placement.refused 2>/dev/null)
  if printf '%s\n' "$events" |
    jq -e --arg a "$boxed" 'select(.kind == "placement.refused" and .agent == $a
                  and (.rejected | any(.why == "no sandbox")))' >/dev/null 2>&1; then
    return 0
  fi
  queue=$(borant_on host-a ps --queued 2>/dev/null) || return 1
  printf '%s\n' "$queue" | grep -q "$boxed" &&
    printf '%s\n' "$queue" | grep -q "no sandbox"
}

# The spawned agent holds a lease on the sandbox host.
boxed_on_sandbox() {
  local id
  id=$(boxed_id)
  [ -n "$id" ] && [ "$(where "$id")" = "host-sandbox" ]
}

# The host advertises the namespace sandbox once its start check passed.
sandbox_offered() {
  borant_on host-a hosts 2>/dev/null |
    jq -e 'any(.[]; .host.name == "host-sandbox"
                  and (.host.sandboxes | index("namespace")) != null)' >/dev/null
}

# The clone of the agent exists on the host that runs it.
cloned() { on host-sandbox test -d "/root/.borant/work/$1/demo/.git"; }

# The results that the sandboxed agent wrote into its own clone: its file is
# there, and the host marker and the sibling's clone are not.
sandbox_reads() {
  local logged
  logged=$(on host-sandbox cat "/root/.borant/work/$1/demo/reads.log" 2>/dev/null) || return 1
  printf '%s\n' "$logged" | grep -q '^READ own.txt ok' &&
    printf '%s\n' "$logged" | grep -q '^READ /root/.ssh/marker failed' &&
    printf '%s\n' "$logged" | grep -q '^READ /work/sib/notes.txt failed'
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
  echo "  spawned: $(spawned_agents | tr '\n' ' ')"
  if spawned_agents | grep -qx '?'; then
    echo "  the boss got no agent id from a spawn" >&2
    return 1
  fi
  # The spawns must not fit on the control host, or the pool is never asked.
  if [ "$(on_control)" -gt 1 ]; then
    echo "  the control host took the spawns: its max_agents is not 1" >&2
    return 1
  fi
  wait_for "two pool hosts" two_pool_hosts || return 1
  wait_for "three agents in the pool" three_in_pool || return 1
  echo "  two containers hold three agents"

  # The agents have no work, so they park and their hosts go away.
  WAIT=300 wait_for "the idle hosts go away" no_pool_hosts || return 1
  echo "  the agents parked and their containers are gone"

  # A killed container is lost: the hub deletes it and starts another host for
  # the agent that it held.
  borant_on host-control send boss "SPAWN e2e killed-during-a-turn" >/dev/null
  wait_for "the fourth spawn" four_spawns || return 1
  wait_for "a pool host for the new agent" one_pool_host || return 1
  local agent host id
  agent=$(spawned_agents | tail -1)
  wait_for "the new agent on a pool host" agent_placed "$agent" || return 1
  host=$(where_on host-control "$agent")
  [ -n "$host" ] || { echo "  $agent holds no lease on a host" >&2; return 1; }
  id=$(docker ps --quiet --filter "name=^${host}$")
  [ -n "$id" ] || { echo "  no container is named $host" >&2; return 1; }
  # The hub takes over an expired lease only for an agent with work, so the
  # agent claims a task before its container is killed: a message that it
  # already got is no reason to start it again.
  borant_on host-control send boss "CREATE e2e killed-worker" >/dev/null
  wait_for "a task for the killed agent" log_has host-control boss "CREATED .* killed-worker" \
    || return 1
  local task
  task=$(task_id host-control boss killed-worker)
  [ -n "$task" ] || { echo "  no task id in the boss log" >&2; return 1; }
  borant_on host-control send "$agent" "CLAIM e2e $task" >/dev/null
  wait_for "the claim of the killed agent" agent_claimed "$id" "$agent" "$task" || return 1
  docker kill "$id" >/dev/null || return 1
  echo "  killed $host ($id), its claim of $task waits"
  # The pool name of a host can repeat after a delete, so the takeover is
  # the agent on a pool host again once the killed container is gone.
  WAIT=300 wait_for "the killed host is deleted" container_gone "$id" || return 1
  WAIT=300 wait_for "a takeover on a pool host" agent_on_pool "$agent" || return 1
  echo "  the agent now runs on $(where_on host-control "$agent")"

  # The cleanup removes every container of the fake provisioner.
  cleanup_e2e_containers
  [ "$(e2e_containers_count)" = "0" ] || { echo "  the cleanup left containers" >&2; return 1; }
}

boss_on_control() { [ "$(where_on host-control boss)" = "host-control" ]; }

# The agents that hold a lease on the control host, the boss included.
on_control() {
  borant_on host-control agents 2>/dev/null |
    jq '[.[] | select(.lease.host == "host-control")] | length'
}

spawned_agents() { log_of host-control boss | awk '$1=="SPAWNED" {print $2}'; }

three_spawns() { [ "$(spawned_agents | wc -l)" -ge 3 ]; }

four_spawns() { [ "$(spawned_agents | wc -l)" -ge 4 ]; }

# The agent holds a lease on a host that the hub gave it.
agent_placed() { [ -n "$(where_on host-control "$1")" ]; }

two_pool_hosts() { [ "$(e2e_containers_count)" = "2" ]; }

one_pool_host() { [ "$(e2e_containers_count)" = "1" ]; }

no_pool_hosts() { [ "$(e2e_containers_count)" = "0" ]; }

# The names of the hosts of the pool, from their registered pool.
pool_hosts() {
  borant_on host-control hosts 2>/dev/null |
    jq -c '[.[] | select(.host.pool == "e2e") | .host.name]'
}

# The agents that hold a lease on a host of the pool.
in_pool() {
  local hosts
  hosts=$(pool_hosts)
  [ -n "$hosts" ] && [ "$hosts" != "[]" ] || return 1
  borant_on host-control agents 2>/dev/null |
    jq -e --argjson h "$hosts" --argjson n "$1" \
      '[.[] | select(.lease.state == "held") | .lease.host as $x
        | select($h | index($x))] | length >= $n' >/dev/null 2>&1
}

three_in_pool() { in_pool 3; }

# The log of a fake agent on a container that the provisioner started.
pool_log() { docker exec "$1" cat "/root/.borant/fake/$2.log" 2>/dev/null; }

# The agent on that container claimed the task.
agent_claimed() { pool_log "$1" "$2" | grep -q -- "CLAIM $3 ok"; }

# The agent holds a lease on a host of the pool.
agent_on_pool() {
  local hosts
  hosts=$(pool_hosts)
  [ -n "$hosts" ] && [ "$hosts" != "[]" ] || return 1
  borant_on host-control agents 2>/dev/null |
    jq -e --argjson h "$hosts" --arg name "$1" \
      'any(.[]; .agent == $name and .lease.state == "held"
            and (.lease.host as $x | ($h | index($x)) != null))' >/dev/null 2>&1
}

container_gone() { ! docker inspect "$1" >/dev/null 2>&1; }

# The members scenario of the token pools design: one host with the fake-auth
# bundle of three members of the fake harness. Fill-first (decision 2 of the
# design) gives every launch the first free member in name order, so the three
# spawns share member a; a reported limit moves the agent to the next free
# member of the same host, and with every member limited the agent waits, the
# members_limited alert fires, and the agent wakes at the earliest reset.
# Nothing real is touched: the values are fakes.
scenario_members() {
  start_agent host-members boss
  wait_for "boss registered" members_placed boss || return 1
  borant_on host-members send boss "CHANNEL pool" >/dev/null
  wait_for "channel pool" log_has host-members boss "CHANNEL pool ok" || return 1

  local i
  for i in 1 2 3; do
    borant_on host-members send boss "SPAWN pool wait-for-work" >/dev/null
  done
  wait_for "three spawns" members_three_spawns || return 1
  local a1 a2 a3 a
  a1=$(members_agents | sed -n 1p)
  a2=$(members_agents | sed -n 2p)
  a3=$(members_agents | sed -n 3p)
  for a in "$a1" "$a2" "$a3"; do
    [ -n "$a" ] || { echo "  a spawn gave no agent id" >&2; return 1; }
    wait_for "$a on the members host" members_placed "$a" || return 1
    [ -n "$(member_of "$a")" ] || {
      echo "  $a holds no member of fake-auth" >&2
      return 1
    }
  done
  echo "  agents: $(members_agents | tr '\n' ' ')"
  echo "  members in use: $(members_in_use | tr '\n' ' ')"
  wait_for "the spawns share the first member" members_share_first || {
    echo "  fill-first did not put every agent on member a" >&2
    return 1
  }
  wait_for "borant members lists a, b, c" members_three_members || return 1

  # A limit of the member of an agent moves it to the next free member of the
  # same host, through the limit path.
  local before after
  before=$(member_of "$a1")
  limit_agent "$a1" 7200 >/dev/null || return 1
  wait_for "member $before limited" member_limited "$before" || return 1
  wait_for "$a1 on a new member" member_moved "$a1" "$before" || return 1
  after=$(member_of "$a1")
  echo "  $a1 moved from $before to $after after the limit"
  members_placed "$a1" || { echo "  $a1 left its host" >&2; return 1; }
  limit_move_logged "$a1" || {
    echo "  no move.requested with reason limit for $a1" >&2
    return 1
  }

  # Limit the member it holds now, and then the last free one: every member
  # is limited, so the agent waits for the earliest reset.
  limit_agent "$a1" 45 >/dev/null || return 1
  wait_for "$a1 on the last free member" member_moved "$a1" "$after" || return 1
  after=$(member_of "$a1")
  limit_agent "$a1" 45 >/dev/null || return 1
  wait_for "every member limited" every_member_limited || return 1
  wait_for "$a1 waits on a member" member_waiting "$a1" || return 1
  echo "  every member is limited and $a1 waits on $after"

  # The alert of the bundle fires, and the agent wakes at the earliest reset.
  wait_for "the members_limited alert" members_limited_alert || return 1
  WAIT=150 wait_for "the wake at the reset" \
    log_has host-members "$a1" "Your account limit reset. Continue your task." || return 1
  echo "  $a1 woke at the reset"

  # No fake secret reaches a container log or the event stream.
  local logs events value
  logs=$(dc --profile members logs --no-color 2>/dev/null)
  events=$(borant_on host-members events --json 2>/dev/null)
  for value in fake-a-token-not-a-secret fake-b-token-not-a-secret fake-c-token-not-a-secret; do
    if [[ "$logs" == *"$value"* ]]; then
      echo "  a fake secret reached a container log" >&2
      return 1
    fi
    if [[ "$events" == *"$value"* ]]; then
      echo "  a fake secret reached the event stream" >&2
      return 1
    fi
  done
}

# The agents that the boss spawned.
members_agents() { log_of host-members boss | awk '$1=="SPAWNED" {print $2}'; }

members_three_spawns() { [ "$(members_agents | wc -l)" -ge 3 ]; }

# The agent holds a lease on the members host.
members_placed() { [ "$(where_on host-members "$1")" = "host-members" ]; }

# A member of fake-auth waits for the agent in `borant members`.
member_waiting() {
  borant_on host-members members --json 2>/dev/null |
    jq -e --arg a "$1" \
      'any(.["fake-auth"][]?; (.waiting // []) | index($a) != null)' >/dev/null
}

# The member of the fake-auth bundle that an agent holds, or nothing.
member_of() {
  borant_on host-members agents 2>/dev/null |
    jq -r --arg a "$1" '.[] | select(.agent == $a) | .members["fake-auth"] // empty'
}

# The members of fake-auth that the agents hold now.
members_in_use() {
  borant_on host-members agents 2>/dev/null |
    jq -r '.[].members["fake-auth"] // empty' | sort -u
}

# Fill-first: every spawned agent holds the first member, a.
members_share_first() { [ "$(members_in_use | tr '\n' ' ')" = "a " ]; }

# `borant members` knows the bundle and the three members that the host has.
members_three_members() {
  [ "$(borant_on host-members members --json 2>/dev/null |
    jq -r '.["fake-auth"][]?.member' | sort | tr '\n' ' ')" = "a b c " ]
}

# The agent moved to a member other than `$2`.
member_moved() {
  local now
  now=$(member_of "$1")
  [ -n "$now" ] && [ "$now" != "$2" ]
}

# The member holds a limit in `borant members`.
member_limited() {
  borant_on host-members members --json 2>/dev/null |
    jq -e --arg m "$1" \
      'any(.["fake-auth"][]?; .member == $m and .limited_until != null)' >/dev/null
}

# The number of members of fake-auth that are limited now.
members_limited_count() {
  borant_on host-members members --json 2>/dev/null |
    jq '[.["fake-auth"][]? | select(.limited_until != null)] | length'
}

every_member_limited() { [ "$(members_limited_count)" = "3" ]; }

# The hub published the alert of the bundle.
members_limited_alert() {
  borant_on host-members events --json 2>/dev/null |
    jq -e 'select(.kind == "alert.fired" and .rule == "members_limited"
                and .subject == "fake-auth")' >/dev/null
}

# The hub logged the limit move of the agent.
limit_move_logged() {
  borant_on host-members events --json 2>/dev/null |
    jq -e --arg a "$1" \
      'select(.kind == "move.requested" and .agent == $a and .reason == "limit")' >/dev/null
}

# An RFC 3339 time `$1` seconds ahead, on the clock of the host container.
resets_in() { on host-members date -u -d "+$1 seconds" +%Y-%m-%dT%H:%M:%SZ; }

# Sends LIMIT for the member that the agent holds now, and prints the time.
limit_agent() {
  local resets
  resets=$(resets_in "$2") || return 1
  borant_on host-members send "$1" "LIMIT $resets" >/dev/null
  echo "$resets"
}

for s in $SCENARIOS; do
  if "scenario_$s"; then pass "$s"; else fail "$s"; fi
done

if [ "$FAILED" != "0" ]; then
  echo "Some scenarios failed. Logs: E2E_ROOT=$ROOT docker compose -f $COMPOSE logs"
  if [ "${KEEP:-0}" != "1" ]; then
    if [ "$SCENARIOS" = "elastic" ]; then
      echo "  agents:"
      borant_on host-control agents 2>/dev/null |
        jq -c '.[] | {agent, harness, host, lease: .lease.state}' | head -20
      echo "  hosts:"
      borant_on host-control hosts 2>/dev/null |
        jq -c '.[] | {name: .host.name, pool: .host.pool, max_agents: .host.max_agents,
                      harnesses: .host.harnesses, online}' | head -10
      echo "  the boss log:"
      log_of host-control boss 2>/dev/null | tail -20
      echo "  hub-elastic and host-control:"
      dc --profile elastic logs --no-color hub-elastic host-control 2>/dev/null | tail -200
    fi
    if [ "$SCENARIOS" = "sandbox" ]; then
      echo "  hosts:"
      borant_on host-a hosts 2>/dev/null |
        jq -c '.[] | {name: .host.name, sandboxes: .host.sandboxes,
                      harnesses: .host.harnesses, online}' | head -10
      echo "  the boss log:"
      log_of host-a boss 2>/dev/null | tail -20
      echo "  hub-sandbox and host-sandbox:"
      dc --profile sandbox logs --no-color hub-sandbox host-sandbox 2>/dev/null | tail -200
    fi
    if [ "$SCENARIOS" = "members" ]; then
      echo "  members:"
      borant_on host-members members --json 2>/dev/null | head -60
      echo "  agents:"
      borant_on host-members agents 2>/dev/null |
        jq -c '.[] | {agent, host, members, lease: .lease.state}' | head -10
      echo "  the boss log:"
      log_of host-members boss 2>/dev/null | tail -20
      echo "  the last agent log:"
      LAST_AGENT=$(members_agents | tail -1)
      [ -n "$LAST_AGENT" ] && log_of host-members "$LAST_AGENT" | tail -30
    fi
    dc --profile elastic --profile sandbox --profile members --profile move logs --no-color 2>/dev/null | tail -200
  fi
  exit 1
fi
echo "All scenarios passed."
