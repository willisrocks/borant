# Borant development commands. Outside CI, run focused tests only.

default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Run one crate's tests, with an optional name filter.
test-crate crate filter="":
    cargo test -p {{crate}} {{filter}}

# Start a local NATS server with JetStream for adapter tests.
nats-up:
    docker run -d --rm --name borant-nats -p 4222:4222 nats:2.11 -js

nats-down:
    docker rm -f borant-nats

# Run the Docker multi-host scenarios.
e2e *scenarios:
    tests/e2e/run.sh {{scenarios}}
