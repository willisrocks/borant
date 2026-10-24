#!/usr/bin/env bash
# Prints the next release version from the newest `vX.Y.Z` tag.
#
# Usage: BUMP=<major|minor|patch> next-version.sh
#
# Prints one JSON object: {"currentVersion": "...", "nextVersion": "..."}.
# currentVersion is "" when no release tag exists yet. The first release is
# 0.1.0 whatever bump was asked for, because there is nothing to increment.
set -euo pipefail

: "${BUMP:?BUMP must be major, minor or patch}"
case "$BUMP" in
  major | minor | patch) ;;
  *)
    echo "invalid BUMP: $BUMP" >&2
    exit 1
    ;;
esac

# No `|| true`: a swallowed fetch failure would compute the version from stale
# local tags.
git fetch --quiet --no-tags origin 'refs/tags/v*:refs/tags/v*'

current="$(
  git tag --list 'v*' |
    sed -n 's#^v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)$#\1#p' |
    sort -t. -k1,1n -k2,2n -k3,3n |
    tail -n 1
)"

if [[ -z "$current" ]]; then
  next='0.1.0'
else
  IFS=. read -r major minor patch <<<"$current"
  case "$BUMP" in
    major) next="$((major + 1)).0.0" ;;
    minor) next="${major}.$((minor + 1)).0" ;;
    patch) next="${major}.${minor}.$((patch + 1))" ;;
  esac
fi

jq -n --arg currentVersion "$current" --arg nextVersion "$next" \
  '{currentVersion: $currentVersion, nextVersion: $nextVersion}'
