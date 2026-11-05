#!/usr/bin/env bash
# Fails unless a `ci` workflow run on a push to main finished green at $SHA.
# A release must ship only a commit that CI tested.
#
# Green means one of these:
# - a GitHub Actions `ci` run on a push to main
# - a Depot CI `ci` workflow in a run with trigger `push`, at a commit on main
#
# Do not read check runs here. `depot ci run` posts checks on its base commit,
# even when it runs with a local patch, so a green check proves nothing. Such a
# run has trigger `api`.
#
# Usage: SHA=<commit> GH_TOKEN=<token> require-green-ci.sh
# CI_SOURCE=depot reads only Depot CI, for a dry run of this gate. A dry run
# needs that because main can also have a green GitHub run.
set -euo pipefail

: "${SHA:?SHA must name the release commit}"

github_green() {
  local conclusion
  conclusion="$(
    gh run list --repo "$GITHUB_REPOSITORY" --workflow ci.yml --branch main --event push \
      --commit "$SHA" --json conclusion,status \
      --jq '[.[] | select(.status == "completed")][0].conclusion // ""'
  )"
  [[ "$conclusion" == "success" ]]
}

depot_green() {
  git merge-base --is-ancestor "$SHA" origin/main || return 1
  local id
  for id in $(
    depot ci run list --repo "$GITHUB_REPOSITORY" --sha "$SHA" --trigger push --status finished --output json |
      jq -r --arg sha "$SHA" '.[] | select(.trigger == "push" and .sha == $sha) | .run_id'
  ); do
    if depot ci status "$id" --output json |
      jq -e '[.workflows[] | select(.name == "ci" and .status == "finished")] | length > 0' >/dev/null; then
      return 0
    fi
  done
  return 1
}

if depot_green; then
  echo "CI is green at $SHA (Depot CI)."
  exit 0
fi
if [[ "${CI_SOURCE:-}" != "depot" ]] && github_green; then
  echo "CI is green at $SHA (GitHub Actions)."
  exit 0
fi
echo "ci_not_green: the ci workflow on main has no green run at $SHA" >&2
exit 1
