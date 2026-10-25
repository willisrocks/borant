#!/bin/sh
env > "$1"
cat > /dev/null
echo '{"ok": null}'
