# Helpers for the Docker multi-host scenarios. Source this file.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE="$ROOT/docker/compose.yaml"
PROJECT=borant-e2e
NETWORK="${PROJECT}_default"

dc() { docker compose -f "$COMPOSE" "$@"; }

# Removes the containers that the fake provisioner of the elastic scenario
# started: `compose down` does not know them, because the plugin starts them
# by hand with the label borant.managed=e2e.
cleanup_e2e_containers() {
  local ids
  ids=$(docker ps --all --quiet --filter label=borant.managed=e2e 2>/dev/null)
  # Word splitting is the point: one id per argument of `docker rm`.
  [ -n "$ids" ] && docker rm -f $ids >/dev/null 2>&1
  return 0
}

# Stops the swarm and the containers of the fake provisioner.
down() {
  cleanup_e2e_containers
  dc --profile move down -v --remove-orphans
}


# Runs a command in a service container.
on() {
  local svc=$1
  shift
  dc exec -T "$svc" "$@"
}

borant_on() {
  local svc=$1
  shift
  on "$svc" borant "$@"
}

container() { echo "${PROJECT}-$1-1"; }

# Starts a fake agent by hand in its own clone of the demo repository.
start_agent() {
  local svc=$1 agent=$2 channel=${3:-}
  local env="BORANT_AGENT=$agent"
  [ -n "$channel" ] && env="$env BORANT_CHANNEL=$channel"
  dc exec -d -T "$svc" sh -c "git clone -q git://git/demo.git /work/$agent 2>/dev/null; cd /work/$agent && $env exec borant-fake-harness"
}

log_of() { on "$1" cat "/root/.borant/fake/$2.log" 2>/dev/null; }

log_has() { log_of "$1" "$2" | grep -q -- "$3"; }

# The host that holds the lease of an agent, or nothing.
where() {
  borant_on host-a agents 2>/dev/null | jq -r --arg a "$1" '.[] | select(.agent==$a) | if .lease.state=="held" then .lease.host else empty end'
}

is_at() { [ "$(where "$1")" = "$2" ]; }

not_at() {
  local h
  h=$(where "$1")
  [ -n "$h" ] && [ "$h" != "$2" ]
}

ref_exists() { on git git -C /srv/git/demo.git for-each-ref --format='%(refname)' "$1" | grep -q .; }

# Retries a command once per second until it succeeds.
wait_for() {
  local desc=$1
  shift
  local n=${WAIT:-60}
  for _ in $(seq 1 "$n"); do
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "  timed out after ${n}s: $desc" >&2
  return 1
}

# The ID of the last task with this title in an agent log.
task_id() {
  log_of "$1" "$2" | awk -v t="$3" '$1=="CREATED" && $3==t {id=$2} END {print id}'
}

pass() { echo "PASS $1"; }

fail() {
  echo "FAIL $1"
  FAILED=1
}
