# syntax=docker/dockerfile:1.7-labs
# Builds borant and borant-fake-harness for the multi-host tests.
FROM rust:1.96-bookworm AS build
WORKDIR /src
# The manifests alone build every dependency into a layer that survives a
# change to the sources, so a cached build only compiles the local crates.
# `--parents` keeps the `crates/<name>/` paths of the glob, and the stub
# `lib.rs` files give cargo the targets that the manifests expect.
COPY --parents Cargo.toml Cargo.lock crates/*/Cargo.toml ./
RUN for f in crates/*/Cargo.toml; do d=${f%/*}; mkdir -p "$d/src"; : > "$d/src/lib.rs"; done
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p borant -p borant-testkit

COPY . .
# A copied source keeps the mtime of the checkout, which is older than the
# stub sources that built the dependencies, and cargo would keep their
# artifacts. One touch makes every real source newer than that build.
RUN touch Cargo.toml Cargo.lock crates/*/Cargo.toml \
 && find crates -name '*.rs' -exec touch {} +
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p borant -p borant-testkit \
 && cp target/release/borant target/release/borant-fake-harness /usr/local/bin/

# The Docker CLI, for the elastic scenario: its hub container starts the pool
# hosts with the socket of the Docker daemon that runs the swarm.
FROM docker:27-cli AS dockercli

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates procps tmux jq \
 && rm -rf /var/lib/apt/lists/*
COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=build /usr/local/bin/borant /usr/local/bin/borant-fake-harness /usr/local/bin/
ENV GIT_AUTHOR_NAME=borant GIT_AUTHOR_EMAIL=borant@example.com \
    GIT_COMMITTER_NAME=borant GIT_COMMITTER_EMAIL=borant@example.com
WORKDIR /work
