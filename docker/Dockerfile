# syntax=docker/dockerfile:1.7
# Builds borant and borant-fake-harness for the multi-host tests.
FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p borant -p borant-testkit \
 && cp target/release/borant target/release/borant-fake-harness /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates procps tmux jq \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/borant /usr/local/bin/borant-fake-harness /usr/local/bin/
ENV GIT_AUTHOR_NAME=borant GIT_AUTHOR_EMAIL=borant@example.com \
    GIT_COMMITTER_NAME=borant GIT_COMMITTER_EMAIL=borant@example.com
WORKDIR /work
