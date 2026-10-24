#!/usr/bin/env bash
# Writes $VERSION into every manifest that carries the Borant version:
# the workspace Cargo.toml, Cargo.lock, the Claude Code plugin, and the Pi
# extension package. Prints nothing if every file already has it.
#
# Usage: VERSION=X.Y.Z set-version.sh
set -euo pipefail

: "${VERSION:?VERSION must be X.Y.Z}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid VERSION: $VERSION" >&2; exit 1; }

# Only the first `version = ` line, which is in [workspace.package].
awk -v v="$VERSION" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
  Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
cargo update --workspace --quiet --offline 2>/dev/null || cargo update --workspace --quiet

for f in integrations/claude-code/.claude-plugin/plugin.json integrations/pi/package.json; do
  jq --arg v "$VERSION" '.version = $v' "$f" > "$f.new" && mv "$f.new" "$f"
done
