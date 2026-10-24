#!/usr/bin/env bash
# Fails unless a `ci` workflow run on a push to main finished green at $SHA.
# A release must ship only a commit that CI tested.
#
# Usage: SHA=<commit> GH_TOKEN=<token> require-green-ci.sh
set -euo pipefail

: "${SHA:?SHA must name the release commit}"
conclusion="$(
  gh run list --repo "$GITHUB_REPOSITORY" --workflow ci.yml --branch main --event push \
    --commit "$SHA" --json conclusion,status \
    --jq '[.[] | select(.status == "completed")][0].conclusion // ""'
)"
if [[ "$conclusion" != "success" ]]; then
  echo "ci_not_green: the ci workflow on main has no green run at $SHA (got '${conclusion:-none}')" >&2
  exit 1
fi
echo "CI is green at $SHA."
