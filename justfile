# Borant development commands. Run `just` to list them.
#
# Outside CI, prefer focused tests: `just test-crate <crate> [filter]`.
# `just test` runs the whole workspace, as CI does.

default:
    @just --list

# Build the binaries in debug mode.
build:
    cargo build -p borant -p borant-testkit

# Build release binaries for this machine.
build-release:
    cargo build --release --locked -p borant

# Format all Rust code.
fmt:
    cargo fmt --all

# Check formatting and lints, as CI does.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Run every Rust test in the workspace. CI runs this.
test:
    cargo test --workspace

# Run one crate's tests, with an optional name filter.
test-crate crate filter="":
    cargo test -p {{crate}} {{filter}}

# Run the NATS adapter contract tests against `just nats-up`.
test-nats:
    BORANT_NATS_URL=nats://127.0.0.1:4222 cargo test -p borant-nats

# Type-check and test the Pi and omp extension.
test-pi:
    cd integrations/pi && bun install --frozen-lockfile && bun run check && bun test

# Start a local NATS server with JetStream for adapter tests.
nats-up:
    docker run -d --rm --name borant-nats -p 4222:4222 nats:2.11 -js

nats-down:
    docker rm -f borant-nats

# Run the Docker multi-host scenarios.
e2e *scenarios:
    tests/e2e/run.sh {{scenarios}}

# Show the commits since the last release tag, as input for release notes.
changes:
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch --quiet --tags origin
    last="$(git tag --list 'v*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -n 1 || true)"
    if [[ -n "$last" ]]; then
      echo "Commits since $last:"
      git log --no-merges --format='- %s (%h)' "$last..origin/main"
    else
      echo "No release yet. All commits on main:"
      git log --no-merges --format='- %s (%h)' origin/main
    fi

# Start a GitHub release (bump: patch|minor|major; notes: optional Markdown file).
release bump="patch" notes="":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{bump}}" in patch|minor|major) ;; *) echo "bump must be patch, minor, or major" >&2; exit 1 ;; esac
    args=(-f confirm=release -f bump={{bump}})
    if [[ -n "{{notes}}" ]]; then
      [[ -f "{{notes}}" ]] || { echo "no notes file at {{notes}}" >&2; exit 1; }
      args+=(-f notes="$(cat "{{notes}}")")
    fi
    gh workflow run release.yml --ref main "${args[@]}"
    sleep 3
    gh run list --workflow release.yml --limit 1

# Render the chief mod at 200x50 and 120x40 and write ANSI + PNG captures.
cos-visual plugin-dir="integrations/claude-code" out="docs/chief-mod-board-ux/captures" prefix="":
    nix shell nixpkgs#charm-freeze -c scripts/cos-visual.sh {{plugin-dir}} {{out}} {{prefix}}
