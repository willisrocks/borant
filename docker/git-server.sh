#!/bin/sh
# A git daemon with one repository, demo.git, that accepts pushes.
set -e
mkdir -p /srv/git
if [ ! -d /srv/git/demo.git ]; then
  git init -q --bare -b main /srv/git/demo.git
  tmp=$(mktemp -d)
  git -C "$tmp" init -q -b main
  echo "# demo" > "$tmp/README.md"
  git -C "$tmp" add README.md
  git -C "$tmp" commit -q -m init
  git -C "$tmp" push -q /srv/git/demo.git main
fi
exec git daemon --reuseaddr --base-path=/srv/git --export-all --enable=receive-pack --informative-errors /srv/git
