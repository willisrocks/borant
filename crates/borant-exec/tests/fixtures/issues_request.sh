#!/bin/sh
# Records the request of the runner in the file of argv[1], and replies with
# the wire shape of each op.
cat > "$1"
op=$(jq -r '.op' "$1")
case "$op" in
describe) echo '{"ok": {"v": 1, "kind": "issues"}}' ;;
create | update) echo '{"ok": {"key": "T-1", "url": "u"}}' ;;
*) echo '{"error": {"code": "invalid", "message": "unknown op"}}' ;;
esac
