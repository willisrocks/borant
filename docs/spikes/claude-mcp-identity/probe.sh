#!/bin/bash
# Log process ancestry and env for a hook or MCP server.
DIR="$(cd "$(dirname "$0")" && pwd)"; tag=$1
{
echo "== $tag pid=$$ $(date +%T)"
p=$$; while [ "$p" -gt 1 ]; do line=$(ps -o pid=,ppid=,comm= -p $p); echo "  $line"; p=$(echo $line | awk '{print $2}'); done
env | grep -iE 'claude|session|mcp' | sed 's/=.*TOKEN.*/=<redacted>/' | sort | sed 's/^/  env /'
} >> "$DIR/probe.log"
if [ "$tag" = mcp ]; then cat >/dev/null; else cat >/dev/null; echo '{}'; fi
