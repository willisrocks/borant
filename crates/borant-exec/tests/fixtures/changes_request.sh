#!/bin/sh
# Records the request of the runner in the file of argv[1], and replies with a
# status of the wire shape.
cat > "$1"
echo '{"ok": {"state": "open", "head": "h1", "checks": {"pass": {}}, "review": "none"}}'
