#!/bin/sh
# A provisioner plugin for the test of one op at a time: it holds a lock
# directory while it works and answers `internal` if a second op of the same
# provisioner runs at once. `$1` is the lock path. A request needs
# `config.marker = "ok"`, so the test also sees the config of every request.
set -u

req=$(cat)
marker=$(printf '%s' "$req" | jq -r '.config.marker // empty')
if [ "$marker" != ok ]; then
  echo '{"error": {"code": "invalid", "message": "the request has no config.marker"}}'
  exit 0
fi
if ! mkdir "$1.lock" 2>/dev/null; then
  echo '{"error": {"code": "internal", "message": "two operations ran at once"}}'
  exit 0
fi
sleep 0.2
rmdir "$1.lock"
echo '{"ok": null}'
