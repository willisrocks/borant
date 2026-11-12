#!/bin/sh
cat > /dev/null
echo "stderr-marker: server limit reached" >&2
echo '{"error": {"code": "limit_reached", "message": "message-marker"}}'
exit 1
