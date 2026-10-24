#!/bin/bash
# Spike: wait for a message file, then wake Claude via asyncRewake (exit 2).
DIR="$(cd "$(dirname "$0")" && pwd)"
input=$(cat)
echo "$(date +%T) waiter start pid=$$ input=$input" >> "$DIR/waiter.log"
while true; do
  for f in "$DIR"/inbox/*.msg; do
    [ -e "$f" ] || continue
    msg=$(cat "$f"); rm -f "$f"
    echo "$(date +%T) waiter deliver pid=$$ msg=$msg" >> "$DIR/waiter.log"
    echo "New swarm message from agent 'tester': $msg" >&2
    exit 2
  done
  sleep 1
done
