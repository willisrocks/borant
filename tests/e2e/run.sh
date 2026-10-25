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

ALL="messaging tasks takeover fencing planned_move drain hub_restart nats_restart hub_move queue host_restart cancel continue sandbox pause"

# The paths of the sandbox: its two sources, its own swarm, and the shared
# image that carries bubblewrap. `docker/compose.yaml` is not here: the queue
# scenario's one-slot host and the host_restart scenario's host-r live in that
# file too, so a change to it runs every scenario.
#
# The host_restart scenario needs no path of its own: the restore that it
# drives is crates/borant-host, which every scenario starts, and its host-r
# service is the compose file above. Its own steps are in this file, which
# already runs every scenario.
#
# The nats_restart scenario needs no path of its own either: the outage that it
# drives is the inbox pull of crates/borant-nats and the outbox of
# crates/borant-host, which every host of every scenario loads. Its own steps
# are in this file, which already runs every scenario.
SANDBOX_PATHS='^(crates/borant-launch/src/sandbox\.rs|crates/borant-host/src/sandbox\.rs|tests/e2e/sandbox/|docker/Dockerfile$|crates/borant-testkit/src/bin/borant-fake-harness\.rs$)'

# The paths of the pause scenario: its own swarm, which runs alone.
PAUSE_PATHS='^tests/e2e/pause/'

# The paths of the continue scenario: the hub's continue path and the pure
# rule that it calls. The scenario has no swarm of its own (it starts host-r
# of the default swarm), so every other path runs the default set as before;
# the fake harness that it scripts is shared with every scenario, and the
# sandbox branch below already names it.
CONTINUE_PATHS='^(crates/borant-hub/src/continuation\.rs|crates/borant-core/src/rules/continuation\.rs)$'

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
#   the pause scenario and its swarm -> pause, or all when other paths changed
#   the continue path and its rule -> continue, or all when other paths
#     changed
#   the cancel path (crates/borant-hub/src/retire.rs, the cancel call in
#   crates/borant-hub/src/spawns.rs, the delete-work order in
#   crates/borant-host/src/commands.rs) -> all: the queue scenario cancels too,
#   so a change there may not run the cancel scenario alone
affected() {
  local base=$1 files
  files=$(git diff --name-only "$base"...HEAD) || return 1
  if [ -z "$files" ]; then
    echo "$ALL"
    return 0
  fi
  # A diff of the sandbox or its scenario needs that scenario. When nothing
  # else changed, it runs alone; otherwise the default set runs first and the
  # script then runs itself for the sandbox swarm. The pause scenario runs
  # alone the same way.
  if printf '%s\n' "$files" | grep -qE "$SANDBOX_PATHS"; then
    if printf '%s\n' "$files" | grep -qvE "$SANDBOX_PATHS"; then
      echo "$ALL"
    else
      echo sandbox
    fi
    return 0
  fi
  if printf '%s\n' "$files" | grep -qE "$PAUSE_PATHS"; then
    if printf '%s\n' "$files" | grep -qvE "$PAUSE_PATHS"; then
      echo "$ALL"
    else
      echo pause
    fi
    return 0
  fi
  if printf '%s\n' "$files" | grep -qE "$CONTINUE_PATHS"; then
    if printf '%s\n' "$files" | grep -qvE "$CONTINUE_PATHS"; then
      echo "$ALL"
    else
      echo continue
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
# and pause scenarios bring their own swarms as well, and they can share a run
# with the default set: the other scenarios run first in a child, then this
# script continues with the swarm of the scenario that runs alone.
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
case " $SCENARIOS " in
  *" pause "*)
    others=$(printf '%s\n' $SCENARIOS | grep -vx pause | tr '\n' ' ')
    if [ -n "$others" ]; then
      # The word splitting is the point: one argument per scenario.
      # shellcheck disable=SC2086
      "$0" $others || exit 1
    fi
    SCENARIOS=pause
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
elif [ "$SCENARIOS" = "pause" ]; then
  # One host with a slow heartbeat: the hub places agents on it only after its
  # first heartbeat, so the wait is longer than the default swarm's.
  echo "Starting the pause swarm..."
  dc --profile pause up -d nats git hub-pause host-pause >/dev/null \
    || { echo "FAIL start"; dc --profile pause logs --no-color 2>/dev/null | tail -200; exit 1; }
  hosts_online() { borant_on host-pause hosts | jq -e 'length == 1 and all(.online)'; }
  WAIT=90 wait_for "host daemons" hosts_online || exit 1
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

# The nats_restart scenario of phase 10: the broker goes down under two idle
# sessions of two hosts. There is no boss to queue the messages, so the
# scenario sends three direct messages to the first session through host-a,
# which runs neither of them: the outbox of host-a holds each one until the
# broker answers again, and the session logs every one of them exactly once
# inside 30 s after that. A new direct message then wakes the second session
# inside 30 s.
#
# A run that fails names what happened: the reply of every send, nr-a in the
# view of its host, the inbox counts of nr-a, and its pause events. A host
# pauses its sessions while the hub is unreachable, and a message that a pull
# takes from a paused session is not acked, so the inbox delivers it again only
# after its ack wait (60 s in crates/borant-nats/src/bus.rs). The scenario
# follows a missing message for 90 s more, which tells a late one from a lost
# one.
#
# The broker stops first, and `docker compose restart nats` brings it back. A
# restart of a running broker returns in well under a second, so the three
# sends would publish at once, the outbox would hold nothing, and the
# scenario would pass without proving the outage. The restart of a stopped
# container is the same restart of the same container, and JetStream keeps its
# data in the `/data` of that container either way.
scenario_nats_restart() {
  start_agent host-b nr-a
  start_agent host-c nr-b
  wait_for "nr-a registered" is_at nr-a host-b || return 1
  wait_for "nr-b registered" is_at nr-b host-c || return 1
  if ! nr_idle host-b nr-a || ! nr_idle host-c nr-b; then
    echo "  a session was not idle before the outage:" >&2
    log_of host-b nr-a | tail -5 >&2
    log_of host-c nr-b | tail -5 >&2
    return 1
  fi

  # The outage: no host reaches the broker, so the three messages wait in the
  # outbox of the host that sends them. Every reply is in the run log, because
  # a run that fails here has to name what the sender was told.
  dc stop nats >/dev/null || return 1
  local i out t0
  for i in 1 2 3; do
    if ! out=$(borant_on host-a send nr-a "nats-outage-$i" 2>&1); then
      echo "  sending nats-outage-$i failed: $out" >&2
      dc logs --no-color nats hub host-a 2>/dev/null | tail -20 >&2
      return 1
    fi
    echo "  nats-outage-$i: $(printf '%s' "$out" | tr -s '[:space:]' ' ')"
    if ! printf '%s\n' "$out" | grep -q '"queued": true'; then
      echo "  nats-outage-$i did not wait in the outbox: $out" >&2
      return 1
    fi
  done

  dc restart nats >/dev/null || return 1
  if ! WAIT=90 wait_for "the swarm after the broker restart" nr_bus_back; then
    echo "  the swarm does not answer after the restart:" >&2
    borant_on host-a hosts 2>&1 | tail -3 >&2
    echo "  the nats service:" >&2
    dc ps nats 2>&1 | tail -3 >&2
    dc logs --no-color nats hub host-a 2>/dev/null | tail -40 >&2
    return 1
  fi

  t0=$(date +%s)

  # Every message of the outage arrives once, inside 30 s, and no copy comes
  # back after the broker had the time to deliver one again.
  if ! WAIT=30 wait_for "the three messages of the outage, once each" nr_backlog_once; then
    echo "  the log of nr-a:" >&2
    log_of host-b nr-a | tail -20 >&2
    echo "  the outbox of host-a:" >&2
    dc logs --no-color host-a 2>/dev/null | grep -i outbox | tail -5 >&2
    echo "  nr-a in the view of its host:" >&2
    borant_on host-b status --json 2>/dev/null |
      jq -c '.agents[] | select(.agent == "nr-a") | {agent, busy, lease: .lease.state}' >&2
    echo "  the inbox counts of nr-a (pending/ack_pending/redelivered):" >&2
    borant_on host-b status --json 2>/dev/null |
      jq -c '.inboxes[] | select(.agent == "nr-a") | .stats' >&2
    echo "  the pause and resume events of nr-a:" >&2
    borant_on host-b events --json 2>/dev/null |
      grep -E 'agent\.(paused|resumed)' | tail -5 >&2
    nr_late "$t0"
    return 1
  fi
  sleep 5
  if ! nr_backlog_once; then
    echo "  a message of the outage arrived a second time:" >&2
    nr_outage_counts >&2
    return 1
  fi

  # The session that waited through the outage wakes on a new direct message.
  borant_on host-a send nr-b nats-after-restart >/dev/null || return 1
  if ! WAIT=30 wait_for "nr-b wakes after the restart" log_has host-c nr-b nats-after-restart; then
    echo "  the log of nr-b:" >&2
    log_of host-c nr-b | tail -20 >&2
    return 1
  fi
  if [ "$(nr_count host-c nr-b nats-after-restart)" != "1" ]; then
    echo "  nr-b woke on nats-after-restart more than once:" >&2
    log_of host-c nr-b | tail -20 >&2
    return 1
  fi
  if nr_has host-c nr-b nats-outage; then
    echo "  nr-b received a message that was for nr-a:" >&2
    log_of host-c nr-b | grep -F -- nats-outage >&2
    return 1
  fi

  # Both sessions end here, so the scenarios after this one start from the
  # state they had before it.
  borant_on host-a send nr-a EXIT >/dev/null
  wait_for "nr-a ended" log_has host-b nr-a EXIT || return 1
  borant_on host-a send nr-b EXIT >/dev/null
  wait_for "nr-b ended" log_has host-c nr-b EXIT || return 1
}

# A session that waits for work: its log holds the line of its start, and no
# message.
nr_idle() { ! log_of "$1" "$2" | grep -qE '^(MSG|SIGTERM)'; }

# How often the fake log of an agent holds a text.
nr_count() { log_of "$1" "$2" | grep -c -F -- "$3"; }

nr_has() { log_of "$1" "$2" | grep -q -F -- "$3"; }

# Every message of the outage is in the log of the first session, once each.
nr_backlog_once() {
  local log i n
  log=$(log_of host-b nr-a)
  for i in 1 2 3; do
    n=$(printf '%s\n' "$log" | grep -c -F -- "nats-outage-$i")
    [ "$n" = "1" ] || return 1
  done
}

# How often each message of the outage is in that log, for a failure report.
nr_outage_counts() {
  local log i
  log=$(log_of host-b nr-a)
  for i in 1 2 3; do
    echo "  nats-outage-$i: $(printf '%s\n' "$log" | grep -c -F -- "nats-outage-$i")"
  done
}

# Every message of the outage is in the log at least once.
nr_backlog_seen() {
  local log i
  log=$(log_of host-b nr-a)
  for i in 1 2 3; do
    printf '%s\n' "$log" | grep -q -F -- "nats-outage-$i" || return 1
  done
}

# The messages that missed the 30 s window: waits for them up to 90 s more, then
# prints how often each one is in the log, against the seconds since the
# restart. A message that a paused session pulled is not acked, and NATS
# delivers it again only after the ack wait of the inbox, so this tells a late
# message from a lost one.
nr_late() {
  local start=$1 n=0
  while [ "$n" -lt 90 ] && ! nr_backlog_seen; do
    sleep 1
    n=$((n + 1))
  done
  echo "  $(( $(date +%s) - start ))s after the restart:" >&2
  nr_outage_counts >&2
}

# The broker and the hub answer a control request again.
nr_bus_back() { borant_on host-a hosts >/dev/null 2>&1; }

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

# The spawn queue: a host with one slot fills, the next spawn waits in the
# state store of the hub across a hub restart, and the freed slot starts it
# with its prompt. The scenario starts host-q on demand, because no host of
# the swarm has one slot, and it makes its own channel so that it also runs
# alone.
scenario_queue() {
  # `--no-deps` keeps compose from starting `nats` again: the move scenario
  # leaves the swarm on nats2, and a second broker named `natshub` would take
  # host-q away from the hub.
  dc up -d --no-deps host-q >/dev/null || return 1
  # Exactly one broker must answer `natshub`, or host-q joins the wrong one
  # and never registers.
  brokers_up() {
    { docker inspect --format '{{.State.Running}}' "$(container nats)" 2>/dev/null
      docker inspect --format '{{.State.Running}}' "$(container nats2)" 2>/dev/null
    } | grep -cx true
  }
  [ "$(brokers_up)" = "1" ] \
    || { echo "  $(brokers_up) brokers are up: host-q needs exactly one" >&2; return 1; }
  host_online() {
    borant_on host-a hosts | jq -e --arg h "$1" 'any(.[]; .host.name == $h and .online)'
  }
  if ! wait_for "host-q online" host_online host-q; then
    echo "  the host-q log:" >&2
    dc logs --no-color host-q 2>/dev/null | tail -10 >&2
    return 1
  fi

  start_agent host-a qboss
  wait_for "qboss registered" is_at qboss host-a || return 1
  borant_on host-c send qboss "CHANNEL queue" >/dev/null
  wait_for "channel queue" log_has host-a qboss "CHANNEL queue ok" || return 1

  # The prompt is the text of the file, and the fake harness logs it.
  on host-a sh -c 'printf "%s\n" queue-fill-prompt-ok > /tmp/q-fill.md' || return 1
  on host-a sh -c 'printf "%s\n" queue-wait-prompt-ok > /tmp/q-wait.md' || return 1

  # The first spawn fills the one slot of host-q.
  local out
  out=$(borant_on host-a spawn --channel queue --prompt-file /tmp/q-fill.md \
    --harness fake --host host-q --name qfill --key q-fill) || return 1
  case "$out" in
    "agent qfill placed"*) ;;
    *) echo "  qfill: $out" >&2; return 1 ;;
  esac
  wait_for "qfill on host-q" is_at qfill host-q || return 1

  # The second spawn finds host-q full, so the hub queues it.
  out=$(borant_on host-a spawn --channel queue --prompt-file /tmp/q-wait.md \
    --harness fake --host host-q --name qwait --key q-wait) || return 1
  [ "$out" = "agent qwait queued" ] \
    || { echo "  qwait: $out (host-q's max_agents is not 1)" >&2; return 1; }
  queue_holds() {
    borant_on host-a ps --queued --json |
      jq -e 'any(.[]; .agent == "qwait" and .state == "queued")'
  }
  wait_for "qwait in the queue" queue_holds || return 1

  # The queue is a record of the state store, so a restarted hub holds it.
  # The move scenario leaves the swarm on hub2, whose hub process runs inside
  # a `sleep infinity` container, so that is the hub to restart; a run of this
  # scenario alone still serves from hub.
  if [ "$(docker inspect --format '{{.State.Running}}' "$(container hub2)" 2>/dev/null)" = "true" ]; then
    dc --profile move restart hub2 >/dev/null
    dc --profile move exec -d -T hub2 borant hub
  else
    dc restart hub >/dev/null
  fi
  WAIT=90 wait_for "the queue survived the hub restart" queue_holds || return 1

  # The freed slot starts the queued agent with its prompt.
  borant_on host-a cancel qfill >/dev/null || return 1
  wait_for "qwait starts" log_has host-q qwait "PROMPT queue-wait-prompt-ok" || return 1
  wait_for "qwait holds host-q" is_at qwait host-q || return 1

  # The same key returns the agent of the first spawn, not a second one.
  out=$(borant_on host-a spawn --channel queue --prompt-file /tmp/q-wait.md \
    --harness fake --host host-q --name qwait --key q-wait) || return 1
  case "$out" in
    "agent qwait placed"*|"agent qwait queued") ;;
    *) echo "  the retry of q-wait answered: $out" >&2; return 1 ;;
  esac

  # Leave no agent of this scenario for a later one.
  borant_on host-a cancel qwait >/dev/null 2>&1
  dc stop host-q >/dev/null 2>&1
  return 0
}

# The host_restart scenario of phase 10: three live sessions on one host, and
# a restart of that host daemon alone, with its container and the harness
# processes still up. The host must load all three from its session table in
# under 10 s, keep their leases, and still take a direct message for each
# agent. The host names the count and the milliseconds of the restore.
scenario_host_restart() {
  # The daemon is not the main process of the container (see compose), so this
  # scenario starts it and can stop that one PID.
  dc up -d --no-deps host-r >/dev/null || return 1
  start_host_r 1 || return 1
  if ! wait_for "host-r online" host_r_online; then
    echo "  the host-r log:" >&2
    host_r_log 1 | tail -20 >&2
    return 1
  fi

  local a
  for a in hr1 hr2 hr3; do
    start_agent host-r "$a"
  done
  for a in hr1 hr2 hr3; do
    wait_for "$a on host-r" on_host_r "$a" || return 1
  done

  # The leases, the harness PIDs, and the container start time before the
  # restart: none of them may change across it, or the restart took the
  # sessions with it.
  local leases_before harnesses_before started_before pid
  leases_before=$(hr_leases) || return 1
  harnesses_before=$(harness_pids) || return 1
  [ -n "$harnesses_before" ] || { echo "  no harness process on host-r" >&2; return 1; }
  started_before=$(container_started) || return 1
  pid=$(host_r_pid) || return 1

  # SIGKILL, not SIGTERM: the sessions must survive the daemon as a crash
  # leaves them, and a graceful stop ends them by design.
  on host-r kill -9 "$pid" || return 1
  wait_for "the old host daemon to exit" daemon_stopped "$pid" || return 1
  start_host_r 2 || return 1

  if ! wait_for "the restore line of the host" host_r_restored; then
    echo "  the host-r log:" >&2
    host_r_log 2 | tail -20 >&2
    return 1
  fi
  local line restored ms
  line=$(host_r_restored)
  restored=${line#restored=}
  restored=${restored%% *}
  ms=${line##*ms=}
  ms=${ms%% *}
  echo "  restored $restored sessions in $ms ms"
  [ "$restored" = "3" ] \
    || { echo "  the host restored $restored sessions, not 3" >&2; return 1; }
  [ "$ms" -lt 10000 ] \
    || { echo "  the restore took $ms ms, the limit is 10000" >&2; return 1; }

  # Only the daemon restarted: the same three harness processes, in the same
  # container, run still.
  [ "$(host_r_pid)" != "$pid" ] \
    || { echo "  the daemon PID $pid did not change" >&2; return 1; }
  [ "$(harness_pids)" = "$harnesses_before" ] || {
    echo "  the harness processes changed: $harnesses_before -> $(harness_pids)" >&2
    return 1
  }
  [ "$(container_started)" = "$started_before" ] \
    || { echo "  the container restarted with the daemon" >&2; return 1; }

  # Every lease stays held, with the same token: it did not expire and get
  # taken again. The swarm's lease is 6 s and the host renews a held lease on
  # each 2 s heartbeat, so a restart slower than that loses it as well: both
  # numbers above and this check tell a slow restore from a lost one.
  [ "$(hr_leases)" = "$leases_before" ] || {
    echo "  the leases changed: $leases_before -> $(hr_leases)" >&2
    return 1
  }

  # Each restored session still takes a direct message.
  for a in hr1 hr2 hr3; do
    borant_on host-r send "$a" "after-host-restart-$a" >/dev/null || return 1
  done
  for a in hr1 hr2 hr3; do
    wait_for "$a took a message after the restart" \
      log_has host-r "$a" "after-host-restart-$a" || return 1
  done

  # Leave no agent or host of this scenario for a later one.
  for a in hr1 hr2 hr3; do
    borant_on host-r cancel "$a" >/dev/null 2>&1
  done
  dc stop host-r >/dev/null 2>&1
  return 0
}

# Starts the host daemon of host-r in the background under a pid file and a
# log of its own, so the scenario stops the PID that it started and reads the
# restore of the start it means ($1 names that start).
start_host_r() {
  # A pid file of an earlier start would satisfy the wait below.
  on host-r rm -f /tmp/host-r.pid || return 1
  dc exec -d -T host-r sh -c \
    "echo \$\$ >/tmp/host-r.pid; exec borant host >/tmp/host-r-$1.log 2>&1" || return 1
  wait_for "the pid file of host-r" host_r_pid_file
}

host_r_pid_file() { on host-r test -s /tmp/host-r.pid; }

host_r_pid() { on host-r cat /tmp/host-r.pid 2>/dev/null | tr -d '[:space:]'; }

host_r_log() { on host-r cat "/tmp/host-r-$1.log" 2>/dev/null | strip_colours; }

# The daemon writes its log to a file, and the logger still colours the field
# names and values: every match and every line that the scenario prints drops
# the SGR sequences first. printf builds the ESC, because BSD sed has no \x.
strip_colours() {
  local esc
  esc=$(printf '\033')
  sed "s/${esc}\[[0-9;]*m//g"
}

# The host daemon answers and the hub lists it online.
host_r_online() {
  borant_on host-r hosts 2>/dev/null |
    jq -e 'any(.[]; .host.name == "host-r" and .online)' >/dev/null
}

# The restore line of the second start as `restored=<n> ms=<m>`: the sessions
# that the host restored and the milliseconds of the restore. The order of the
# two fields is the logger's, so each is taken where it is.
host_r_restored() {
  local line restored ms
  line=$(host_r_log 2 | grep 'restored local sessions' | head -1)
  [ -n "$line" ] || return 1
  restored=$(printf '%s\n' "$line" | grep -o 'restored=[0-9]*' | head -1)
  ms=$(printf '%s\n' "$line" | grep -o 'ms=[0-9]*' | head -1)
  [ -n "$restored" ] && [ -n "$ms" ] || return 1
  printf '%s %s\n' "$restored" "$ms"
}

# A PID that is gone, or one that the container never reaped, is not the
# daemon any more.
daemon_stopped() {
  case "$(on host-r ps -o stat= -p "$1" 2>/dev/null | tr -d '[:space:]')" in
    '' | Z*) return 0 ;;
    *) return 1 ;;
  esac
}

# The fake harness processes of host-r: the same PIDs before and after the
# restart say that only the daemon restarted.
harness_pids() {
  on host-r ps -eo pid=,args= 2>/dev/null |
    awk '$2 == "borant-fake-harness" {print $1}' | sort | tr '\n' ' '
}

# The moment the container of host-r started, so a container restart shows.
container_started() {
  docker inspect --format '{{.State.StartedAt}}' "$(container host-r)" 2>/dev/null
}

# The leases of the three agents of this scenario, with their tokens.
hr_leases() {
  borant_on host-r agents 2>/dev/null |
    jq -c '[.[] | select(.agent == "hr1" or .agent == "hr2" or .agent == "hr3")
            | {agent, host: .lease.host, state: .lease.state, token: .lease.token}]
           | sort_by(.agent)'
}

# The agent holds its lease on host-r.
on_host_r() { [ "$(where_on host-r "$1")" = "host-r" ]; }

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

# The pause scenario of phase 10, on its own swarm (see the start block). The
# hub of tests/e2e/pause holds a role with `max_turns_per_day = 2`, and its one
# host reports a heartbeat every 20 s. The hub counts the turns of a session on
# the heartbeat, so the three turns of the capped agent land in one window of
# the count and the third is the turn that reaches the cap: the hub stops the
# agent with one `agent.paused` event whose reason starts with `cap:`, its
# claim goes to a person, and no placement brings it back. `borant resume`
# places it again. A person also stops a second agent and resumes it.
scenario_pause() {
  local task out seen

  # The boss creates the channel with its project, and the task that the
  # capped agent claims.
  start_agent host-pause boss
  wait_for "boss registered" pause_holds boss || return 1
  borant_on host-pause project add demo git://git/demo.git --branch main >/dev/null || return 1
  borant_on host-pause send boss "CHANNEL demo demo" >/dev/null
  wait_for "the channel with its project" log_has host-pause boss "CHANNEL demo ok" || return 1
  borant_on host-pause send boss "CREATE demo capped" >/dev/null
  wait_for "task capped" log_has host-pause boss "CREATED .* capped" || return 1
  task=$(task_id host-pause boss capped)
  [ -n "$task" ] || { echo "  the id of the task capped" >&2; return 1; }

  # The capped agent claims the task in the turn of its launch prompt.
  on host-pause sh -c "printf 'CLAIM demo $task\n' > /tmp/pause-capped.md" || return 1
  out=$(borant_on host-pause spawn --channel demo --role capped --name capped \
    --prompt-file /tmp/pause-capped.md) || return 1
  case "$out" in
    "agent capped placed"*) ;;
    *) echo "  capped: $out" >&2; return 1 ;;
  esac
  wait_for "the capped agent claimed the task" log_has host-pause capped "CLAIM $task ok" || return 1

  # The next heartbeat reports the first turn and the idle session, so the hub
  # stops the session at once when the cap is reached instead of after the
  # grace. The two turns that follow are in the window of the count.
  sleep 2
  seen=$(pause_heartbeat) || return 1
  wait_for "a heartbeat of the pause host" pause_beat "$seen" || return 1
  borant_on host-pause send capped "WRITE step2.txt two" >/dev/null
  wait_for "the second turn of the capped agent" log_has host-pause capped "WROTE step2.txt" || return 1
  borant_on host-pause send capped "WRITE step3.txt three" >/dev/null
  wait_for "the third turn of the capped agent" log_has host-pause capped "WROTE step3.txt" || return 1

  # The third turn reaches the cap of two.
  wait_for "the agent.paused event of the cap" pause_capped_event || return 1
  wait_for "the claim of the capped agent in needs_human" pause_capped_claim "$task" || return 1
  wait_for "the stopped session of the capped agent" log_has host-pause capped SIGTERM || return 1
  echo "  the cap stopped capped: $(pause_capped_why)"

  # No placement brings the stopped agent back.
  wait_for "the capped agent holds no lease" pause_free capped || return 1
  sleep 30
  pause_free capped || { echo "  the hub placed the stopped agent again" >&2; return 1; }

  # A person resumes it, and it holds a lease again. The resume of a
  # cap-stopped agent starts the cap over, so the agent stays placed.
  out=$(borant_on host-pause resume capped) || return 1
  pause_placed "$out" capped || return 1
  wait_for "the resumed agent holds its lease" pause_holds capped || return 1

  # A person stops a second agent, which is idle, and resumes it. An idle
  # session ends at once with its lease released; a fenced turn does not
  # release its lease, so the scenario waits for a heartbeat that carries the
  # idle report of the agent before it stops it.
  start_agent host-pause helper demo
  wait_for "the helper registered" pause_holds helper || return 1
  borant_on host-pause send helper "WRITE helper.txt ready" >/dev/null
  wait_for "the first turn of the helper" log_has host-pause helper "WROTE helper.txt" || return 1
  sleep 2
  seen=$(pause_heartbeat) || return 1
  wait_for "a heartbeat of the pause host" pause_beat "$seen" || return 1
  borant_on host-pause stop helper --grace 5 >/dev/null || return 1
  wait_for "the stopped helper ended" log_has host-pause helper SIGTERM || return 1
  wait_for "the stopped helper holds no lease" pause_free helper || return 1
  wait_for "the inbox names the stop of the helper" pause_helper_stopped || return 1
  out=$(borant_on host-pause resume helper) || return 1
  pause_placed "$out" helper || return 1
  wait_for "the resumed helper holds its lease" pause_holds helper || return 1
}

# The agent holds a held lease on the one host of the pause swarm.
pause_holds() { [ "$(where_on host-pause "$1")" = "host-pause" ]; }

# The agent holds no held lease: no placement brought it back.
pause_free() { [ -z "$(where_on host-pause "$1")" ]; }

# The reply of `borant resume` clears the pause and places the agent again.
pause_placed() {
  printf '%s' "$1" | jq -e --arg a "$2" 'select(.agent == $a and .paused == false and .placed)' >/dev/null ||
    { echo "  resume $2 answered: $1" >&2; return 1; }
}

# The `last_seen` of the pause host, which the hub moves on each heartbeat.
pause_heartbeat() {
  borant_on host-pause hosts 2>/dev/null |
    jq -r '.[] | select(.host.name == "host-pause") | .last_seen'
}

# The pause host sent a heartbeat after the one of `$1`.
pause_beat() {
  local now
  now=$(pause_heartbeat) || return 1
  [ -n "$1" ] && [ -n "$now" ] && [ "$now" != "$1" ]
}

# The hub published `agent.paused` for the capped agent, with the cap as the
# reason. A stop by a person has another reason, so this one is the cap.
pause_capped_event() {
  borant_on host-pause events --json --kind agent.paused 2>/dev/null |
    jq -e 'select(.agent == "capped" and .kind == "agent.paused"
                and (.why | startswith("cap:")))' >/dev/null
}

# The reason of the cap pause, for the log of the scenario.
pause_capped_why() {
  borant_on host-pause events --json --kind agent.paused 2>/dev/null |
    jq -r 'select(.agent == "capped" and (.why | startswith("cap:"))) | .why' | tail -1
}

# The claim of the capped agent is in `needs_human`, with the reason of the cap.
pause_capped_claim() {
  borant_on host-pause inbox --json 2>/dev/null |
    jq -e --arg t "$1" 'any(.tasks[]; .id == $t and .status == "needs_human"
                and (.attention_msg | startswith("cap:")))' >/dev/null
}

# The inbox lists the helper, which holds no claim, as stopped by a person.
pause_helper_stopped() {
  borant_on host-pause inbox --json 2>/dev/null |
    jq -e 'any(.agents[]; .agent == "helper" and .why == "stopped by human")' >/dev/null
}

# The cancel scenario of phase 10: `borant cancel` of an idle worker ends its
# session within one heartbeat, and the host then frees its clone; a worker in
# a turn is not stopped until the turn ends. The swarm's heartbeat is 2 s
# (`BORANT_HEARTBEAT_SECS`), so one heartbeat plus 10 s is 12 s here.

# The agent holds a lease, so the hub placed it.
cancel_held() { [ -n "$(where "$1")" ]; }

# The host ended the session of the retired agent and freed its work dir: the
# lease is released, the dir is gone, and the host log names the delete. The
# host writes its fields with ANSI, so `agent=idle1` is only contiguous after
# the escapes go: tracing styles the name, the `=`, and the value apart.
cancel_retired() {
  local host=$1 agent=$2
  [ -z "$(where "$agent")" ] || return 1
  if on "$host" test -e "/root/.borant/work/$agent"; then
    return 1
  fi
  dc logs --no-color "$host" 2>/dev/null |
    sed -e 's/\x1b\[[0-9;]*m//g' |
    grep "deleted the work dir of a retired agent" |
    grep -Eq "agent=$agent([^[:alnum:]_.-]|$)"
}

scenario_cancel() {
  local out host start

  # A channel with a project, so that a launch restores the demo repo into the
  # work dir of its agent: that clone is what the host must free.
  borant_on host-a project add canceltest git://git/demo.git --branch main >/dev/null || return 1
  start_agent host-a cboss
  wait_for "cboss registered" is_at cboss host-a || return 1
  borant_on host-a send cboss "CHANNEL canceltest canceltest" >/dev/null
  wait_for "the channel with its project" log_has host-a cboss "CHANNEL canceltest ok" || return 1
  # The channel stays; the session of cboss only held a slot of host-a.
  borant_on host-a cancel cboss >/dev/null 2>&1

  # A line that the fake harness logs, so that the scenario knows the launch
  # went through.
  on host-a sh -c 'printf "%s\n" cancel-idle-ok > /tmp/cancel-idle.md' || return 1
  on host-a sh -c 'printf "%s\n" cancel-busy-ok > /tmp/cancel-busy.md' || return 1

  # An idle worker with a clone.
  out=$(borant_on host-a spawn --channel canceltest --prompt-file /tmp/cancel-idle.md \
    --harness fake --name idle1 --key cancel-idle) || return 1
  case "$out" in
    "agent idle1 placed"*) ;;
    *)
      echo "  idle1: $out" >&2
      borant_on host-a ps --queued --json >&2 2>/dev/null
      return 1
      ;;
  esac
  wait_for "idle1 holds a lease" cancel_held idle1 || return 1
  host=$(where idle1)
  wait_for "idle1 read its prompt" log_has "$host" idle1 "PROMPT cancel-idle-ok" || return 1
  on "$host" test -d /root/.borant/work/idle1/demo/.git \
    || { echo "  idle1 has no clone on $host" >&2; return 1; }
  # Two heartbeats carry the end of the first turn to the hub, so it knows
  # that the session is idle and stops it at once.
  sleep 4

  start=$SECONDS
  out=$(borant_on host-a cancel idle1) || return 1
  printf '%s' "$out" | jq -e '.stopped == true' >/dev/null \
    || { echo "  an idle worker was not stopped at once: $out" >&2; return 1; }
  WAIT=12 wait_for "idle1 ends within one heartbeat and 10 s" cancel_retired "$host" idle1 || return 1
  echo "  idle1 stopped and lost its clone in $((SECONDS - start)) s"

  # The worker in a turn: a FIFO outside its clone keeps the write of its turn
  # waiting, so the turn runs for as long as the scenario needs.
  out=$(borant_on host-a spawn --channel canceltest --prompt-file /tmp/cancel-busy.md \
    --harness fake --name busy1 --key cancel-busy) || return 1
  case "$out" in
    "agent busy1 placed"*) ;;
    *)
      echo "  busy1: $out" >&2
      borant_on host-a ps --queued --json >&2 2>/dev/null
      return 1
      ;;
  esac
  wait_for "busy1 holds a lease" cancel_held busy1 || return 1
  host=$(where busy1)
  wait_for "busy1 read its prompt" log_has "$host" busy1 "PROMPT cancel-busy-ok" || return 1
  on "$host" sh -c 'rm -f /tmp/cancel-busy.fifo && mkfifo /tmp/cancel-busy.fifo' || return 1
  borant_on host-a send busy1 "WRITE /tmp/cancel-busy.fifo in-a-long-turn" >/dev/null
  wait_for "busy1 started the write" log_has "$host" busy1 "WRITE /tmp/cancel-busy.fifo" || return 1
  if log_of "$host" busy1 | grep -q "WROTE /tmp/cancel-busy.fifo"; then
    echo "  busy1 did not wait in its turn" >&2
    return 1
  fi
  # Three heartbeats carry the turn to the hub, so that the cancel sees one.
  sleep 6

  out=$(borant_on host-a cancel busy1) || return 1
  printf '%s' "$out" | jq -e '.stopped == false and .stopping == "at the turn end"' >/dev/null \
    || { echo "  a busy worker was not kept for its turn end: $out" >&2; return 1; }

  # The session, its lease, and its clone stay while the turn runs.
  sleep 6
  [ "$(where busy1)" = "$host" ] \
    || { echo "  busy1 lost its lease in its turn" >&2; return 1; }
  on "$host" test -d /root/.borant/work/busy1 \
    || { echo "  busy1 lost its clone in its turn" >&2; return 1; }
  if log_of "$host" busy1 | grep -q SIGTERM; then
    echo "  busy1 was stopped in its turn" >&2
    return 1
  fi

  # The turn ends: the write returns, and the session stops at the next
  # heartbeat, which frees the clone.
  on "$host" sh -c 'timeout 10 cat /tmp/cancel-busy.fifo >/dev/null' || return 1
  wait_for "busy1 finished the write" log_has "$host" busy1 "WROTE /tmp/cancel-busy.fifo" || return 1
  WAIT=20 wait_for "busy1 ends after its turn" cancel_retired "$host" busy1 || return 1
  wait_for "busy1 stopped" log_has "$host" busy1 SIGTERM || return 1
}

# The continue scenario of phase 11 (bounded task continuation): an idle worker
# bound to a task that it claims gets a continue, the wait that it then stores
# stops the continues, and the limit sends the task to a person. A host daemon
# restart and a planned move keep the count. The scenario runs on the default
# swarm, with host-r as the host whose daemon alone can be stopped.
#
# In the full run this comes after the hub move, so the live hub is hub2 and
# the live broker is nats2. It starts its own host alone, with --no-deps, and
# waits for the live hub to answer: nothing here may start a second broker or
# a second hub. The boss agent has a name of its own as well: the cancel
# scenario retires its boss for good, and a retired agent is never woken
# again, so reusing that name would leave the channel call in a dead inbox.
scenario_continue() {
  local out task_a task_b pid

  # The hub of the live swarm answers before this scenario touches it.
  wait_for "the live hub" continue_hub_up || return 1

  # host-r is not part of the default swarm: the host_restart scenario starts
  # it on demand, and this one does the same. --no-deps keeps compose from
  # starting the old nats: after the hub move the swarm runs on nats2.
  dc up -d --no-deps host-r >/dev/null || return 1
  start_host_r 1 || return 1
  if ! wait_for "host-r online" host_r_online; then
    echo "  the host-r log:" >&2
    host_r_log 1 | tail -20 >&2
    return 1
  fi

  # The channel and the two tasks that the workers bind.
  start_agent host-a cont-boss
  wait_for "cont-boss registered" is_at cont-boss host-a || return 1
  borant_on host-a send cont-boss "CHANNEL continue" >/dev/null || return 1
  wait_for "channel continue" log_has host-a cont-boss "CHANNEL continue ok" || return 1
  borant_on host-a send cont-boss "CREATE continue waits
CREATE continue limit" >/dev/null || return 1
  wait_for "the task waits" log_has host-a cont-boss "CREATED .* waits" || return 1
  wait_for "the task limit" log_has host-a cont-boss "CREATED .* limit" || return 1
  task_a=$(task_id host-a cont-boss waits)
  task_b=$(task_id host-a cont-boss limit)
  [ -n "$task_a" ] && [ -n "$task_b" ] \
    || { echo "  the task ids of the scenario" >&2; return 1; }

  # Each worker claims its bound task in the turn of the launch prompt, so the
  # task is in progress and claimed before the session goes idle.
  on host-a sh -c "printf 'CLAIM continue $task_a\n' > /tmp/continue-waits.md" || return 1
  on host-a sh -c "printf 'CLAIM continue $task_b\n' > /tmp/continue-limit.md" || return 1

  # A worker with two continues: the first idle gets one, then it is done and
  # waits on its task, and the wait stops the next idle.
  out=$(borant_on host-a spawn --channel continue --prompt-file /tmp/continue-waits.md \
    --harness fake --host host-r --name cont-a --key continue-waits \
    --task "continue/$task_a" --continue 2) || return 1
  case "$out" in
    "agent cont-a placed"*) ;;
    *) echo "  cont-a: $out" >&2; return 1 ;;
  esac
  wait_for "cont-a claimed waits" log_has host-r cont-a "CLAIM $task_a ok" || return 1
  wait_log_fast host-r cont-a "Continue with task continue/$task_a" || return 1
  borant_on host-r send cont-a "WAITTASK continue $task_a" >/dev/null
  wait_for "cont-a stored its wait" log_has host-r cont-a "WAITTASK $task_a ok" || return 1
  # The count is written before the send, and the stored wait stops the next
  # idle period.
  [ "$(continue_state cont-a)" = "1 false" ] \
    || { echo "  cont-a after its wait: $(continue_state cont-a)" >&2; return 1; }
  sleep 8
  [ "$(continue_count host-r cont-a)" = "1" ] \
    || { echo "  cont-a got another continue after its wait" >&2; return 1; }

  # A worker with one continue: the next idle reaches the limit, so the hub
  # sends no continue and sets its task needs_human.
  out=$(borant_on host-a spawn --channel continue --prompt-file /tmp/continue-limit.md \
    --harness fake --host host-r --name cont-b --key continue-limit \
    --task "continue/$task_b" --continue 1) || return 1
  case "$out" in
    "agent cont-b placed"*) ;;
    *) echo "  cont-b: $out" >&2; return 1 ;;
  esac
  wait_for "cont-b claimed limit" log_has host-r cont-b "CLAIM $task_b ok" || return 1
  wait_for "cont-b got its one continue" \
    log_has host-r cont-b "Continue with task continue/$task_b" || return 1
  wait_for "the task of cont-b needs a person" \
    continue_needs_human "$task_b" "continue limit reached (1)" || return 1
  [ "$(continue_state cont-b)" = "1 true" ] \
    || { echo "  cont-b at the limit: $(continue_state cont-b)" >&2; return 1; }

  # A host daemon restart keeps both counts, and the wait still stops cont-a
  # after a new turn.
  pid=$(host_r_pid) || return 1
  on host-r kill -9 "$pid" || return 1
  wait_for "the old host daemon to exit" daemon_stopped "$pid" || return 1
  start_host_r 2 || return 1
  if ! wait_for "the restart of host-r restored both sessions" continue_restored; then
    echo "  the host-r log:" >&2
    host_r_log 2 | tail -20 >&2
    return 1
  fi
  borant_on host-r send cont-a "WRITE after-restart.txt yes" >/dev/null
  wait_for "cont-a took a turn after the restart" \
    log_has host-r cont-a "WROTE after-restart.txt" || return 1
  sleep 8
  [ "$(continue_count host-r cont-a)" = "1" ] \
    || { echo "  the restart sent cont-a another continue" >&2; return 1; }
  [ "$(continue_state cont-a)" = "1 false" ] \
    || { echo "  cont-a after the restart: $(continue_state cont-a)" >&2; return 1; }
  [ "$(continue_state cont-b)" = "1 true" ] \
    || { echo "  cont-b after the restart: $(continue_state cont-b)" >&2; return 1; }

  # A planned move keeps the count too, and its new idle period gets no
  # continue: the wait of the worker still covers the task. host-b holds one
  # session after the earlier scenarios (nr-a of nats_restart), so it has the
  # room that host-a may not have.
  borant_on host-a move cont-a --host host-b >/dev/null || return 1
  wait_for "cont-a on host-b" is_at cont-a host-b || return 1
  wait_for "cont-a continued on host-b" \
    log_has host-b cont-a "You continue Borant agent cont-a" || return 1
  sleep 8
  [ "$(continue_count host-b cont-a)" = "0" ] \
    || { echo "  the move sent cont-a another continue" >&2; return 1; }
  [ "$(continue_state cont-a)" = "1 false" ] \
    || { echo "  cont-a after the move: $(continue_state cont-a)" >&2; return 1; }

  # Leave no agent or host of this scenario for a later one.
  borant_on host-a cancel cont-a >/dev/null 2>&1
  borant_on host-r cancel cont-b >/dev/null 2>&1
  borant_on host-a cancel cont-boss >/dev/null 2>&1
  dc stop host-r >/dev/null 2>&1
  return 0
}

# The continues that an agent got: the hub's direct message is one line per
# continue in the fake log of the agent.
continue_count() { log_of "$1" "$2" | grep -c "Continue with task continue/"; }

# The live hub answers a request from the default swarm.
continue_hub_up() { borant_on host-a hosts >/dev/null 2>&1; }

# Waits up to WAIT seconds for a log line, polling every 100 ms: the wait that
# stops the next continue must land before the next heartbeat of the host.
wait_log_fast() {
  local n=${WAIT:-60}
  for _ in $(seq 1 $((n * 10))); do
    log_has "$1" "$2" "$3" && return 0
    sleep 0.1
  done
  echo "  timed out after ${n}s: $3" >&2
  return 1
}

# The continue state of an agent in the hub record, as `<used> <exhausted>`.
# The snapshot lands on the box of the live hub: hub2 after the hub move
# scenario, hub on a run of this scenario alone. The name is fresh, so only
# the container that wrote it holds the file and a stopped hub cannot be read.
continue_state() {
  CONTINUE_STATE_N=$((CONTINUE_STATE_N + 1))
  local tmp="/tmp/e2e-continue-state-$$-$CONTINUE_STATE_N.jsonl" svc rc
  borant_on host-a hub export --snapshot --out "$tmp" >/dev/null || return 1
  for svc in hub2 hub; do
    on "$svc" test -s "$tmp" 2>/dev/null || continue
    on "$svc" jq -r --arg k "agent.$1" \
      'select(.t == "state" and .key == $k)
       | "\(.value.continues.used) \(.value.continues.exhausted)"' "$tmp"
    rc=$?
    on "$svc" rm -f "$tmp"
    return "$rc"
  done
  return 1
}
CONTINUE_STATE_N=0

# The task is in `needs_human` with the message of the exhausted continues.
continue_needs_human() {
  borant_on host-a inbox --json 2>/dev/null |
    jq -e --arg t "$1" --arg w "$2" \
      'any(.tasks[]; .id == $t and .status == "needs_human" and .attention_msg == $w)' >/dev/null
}

# The restart of host-r restored both sessions of the scenario.
continue_restored() {
  local out
  out=$(host_r_restored) || return 1
  case "$out" in
    "restored=2 "*) return 0 ;;
    *) return 1 ;;
  esac
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
    case " $SCENARIOS " in
      *" continue "*)
        echo "  the continue agents:"
        borant_on host-a agents 2>/dev/null |
          jq -c '.[] | select(.agent | startswith("cont-"))
                | {agent, host, lease: .lease.state}' | head -10
        echo "  inbox:"
        borant_on host-a inbox --json 2>/dev/null | head -40
        echo "  the continue logs:"
        log_of host-r cont-a 2>/dev/null | tail -20
        log_of host-b cont-a 2>/dev/null | tail -20
        log_of host-r cont-b 2>/dev/null | tail -20
        log_of host-a cont-boss 2>/dev/null | tail -20
        echo "  the daemon logs of host-r:"
        host_r_log 1 | tail -20
        host_r_log 2 | tail -40
        ;;
      *" host_restart "*)
        echo "  the daemon logs of host-r:"
        host_r_log 1 | tail -20
        host_r_log 2 | tail -40
        echo "  its agents, and the harness processes of them:"
        borant_on host-r agents 2>/dev/null |
          jq -c '.[] | {agent, host, lease: .lease.state}' | head -10
        harness_pids
        ;;
    esac
    if [ "$SCENARIOS" = "pause" ]; then
      echo "  agents:"
      borant_on host-pause agents 2>/dev/null |
        jq -c '.[] | {agent, role, host, lease: .lease.state}' | head -10
      echo "  inbox:"
      borant_on host-pause inbox --json 2>/dev/null | head -40
      echo "  pause events:"
      borant_on host-pause events --json 2>/dev/null |
        grep -E 'agent.paused|agent.resumed' | tail -10
      echo "  the boss and capped logs:"
      log_of host-pause boss 2>/dev/null | tail -20
      log_of host-pause capped 2>/dev/null | tail -30
      echo "  hub-pause and host-pause:"
      dc --profile pause logs --no-color hub-pause host-pause 2>/dev/null | tail -200
    fi
    if [ "$SCENARIOS" = "nats_restart" ]; then
      echo "  the log of nr-a:"
      log_of host-b nr-a 2>/dev/null | tail -20
      echo "  the log of nr-b:"
      log_of host-c nr-b 2>/dev/null | tail -20
      echo "  agents:"
      borant_on host-a agents 2>/dev/null |
        jq -c '.[] | {agent, host, lease: .lease.state}' | head -10
    fi
    dc --profile elastic --profile sandbox --profile members --profile pause --profile move logs --no-color 2>/dev/null | tail -200
  fi
  exit 1
fi
echo "All scenarios passed."
