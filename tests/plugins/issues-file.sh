#!/bin/sh
# An issue-tracker exec plugin for tests: it keeps the issues of one JSON
# file, named by `config.file` in every request, and needs no network. The
# wire fields are those of `borant_core::ports::issues`.
#
# The file holds `{"issues": {<key>: {...}}, "next": <n>, "writes": {<uuid>:
# <key>}}`. A create maps its write key to an issue, so a create that is sent
# twice makes one issue, and a child without a target takes its parent's
# team.
set -eu

req=$(cat)

bad() { # bad CODE MESSAGE
  jq -n -c --arg code "$1" --arg message "$2" \
    '{error: {code: $code, message: $message}}'
  exit 0
}

# The key of an issue named by a key or a URL.
key_of() {
  printf '%s' "$1" | sed 's#^.*/##'
}

op=$(printf '%s' "$req" | jq -r '.op // empty')
[ -n "$op" ] || bad invalid "the request has no op"

case "$op" in
describe)
  jq -n -c '{ok: {v: 1, kind: "issues"}}'
  ;;
create)
  key=$(printf '%s' "$req" | jq -r '.write.key // empty')
  [ -n "$key" ] || bad invalid "the write has no key"
  state=$(printf '%s' "$req" | jq -r '.write.state // empty')
  [ -n "$state" ] || bad invalid "the write has no state"
  file=$(printf '%s' "$req" | jq -r '.config.file // empty')
  [ -n "$file" ] || bad invalid "the config has no file"
  [ -f "$file" ] || printf '{"issues":{},"next":1,"writes":{}}\n' > "$file"

  # The same write key is the same issue.
  had=$(jq -r --arg k "$key" '.writes[$k] // empty' "$file")
  if [ -n "$had" ]; then
    url=$(jq -r --arg k "$had" '.issues[$k].url' "$file")
    jq -n -c --arg k "$had" --arg u "$url" '{ok: {key: $k, url: $u}}'
    exit 0
  fi

  team=$(printf '%s' "$req" | jq -r '.write.target // empty')
  parent=$(printf '%s' "$req" | jq -r '.write.parent // empty')
  if [ -z "$team" ] && [ -n "$parent" ]; then
    parent_key=$(key_of "$parent")
    team=$(jq -r --arg k "$parent_key" '.issues[$k].team // empty' "$file")
  fi
  [ -n "$team" ] || bad invalid "a create needs a target or a parent"

  n=$(jq -r '.next' "$file")
  made="${team}-${n}"
  title=$(printf '%s' "$req" | jq -r '.write.title // ""')
  body=$(printf '%s' "$req" | jq -r '.write.body // ""')
  jq -c --arg k "$key" --arg made "$made" --arg team "$team" --arg parent "$parent" \
    --arg title "$title" --arg body "$body" --arg state "$state" '
      .next = .next + 1
      | .writes[$k] = $made
      | .issues[$made] = {
          url: ("https://issues.example.test/" + $made),
          team: $team, parent: $parent, title: $title, body: $body, state: $state
        }
    ' "$file" > "$file.tmp"
  mv "$file.tmp" "$file"
  jq -n -c --arg k "$made" --arg u "https://issues.example.test/$made" \
    '{ok: {key: $k, url: $u}}'
  ;;
update)
  issue=$(printf '%s' "$req" | jq -r '.issue // empty')
  [ -n "$issue" ] || bad invalid "the request has no issue"
  state=$(printf '%s' "$req" | jq -r '.write.state // empty')
  [ -n "$state" ] || bad invalid "the write has no state"
  file=$(printf '%s' "$req" | jq -r '.config.file // empty')
  [ -n "$file" ] || bad invalid "the config has no file"
  [ -f "$file" ] || bad not_found "no issue $issue"
  issue_key=$(key_of "$issue")
  found=$(jq -r --arg k "$issue_key" '.issues[$k] // empty' "$file")
  [ -n "$found" ] || bad not_found "no issue $issue_key"

  full=$(printf '%s' "$req" | jq -r '.full // false')
  if [ "$full" = "true" ]; then
    title=$(printf '%s' "$req" | jq -r '.write.title // ""')
    body=$(printf '%s' "$req" | jq -r '.write.body // ""')
    parent=$(printf '%s' "$req" | jq -r '.write.parent // ""')
    target=$(printf '%s' "$req" | jq -r '.write.target // ""')
    jq -c --arg k "$issue_key" --arg title "$title" --arg body "$body" \
      --arg parent "$parent" --arg target "$target" '
        .issues[$k].title = $title
        | .issues[$k].body = $body
        | .issues[$k].parent = $parent
        | if $target == "" then . else .issues[$k].team = $target end
      ' "$file" > "$file.tmp"
  else
    # An adopted issue gets the state only.
    jq -c --arg k "$issue_key" --arg state "$state" \
      '.issues[$k].state = $state' "$file" > "$file.tmp"
  fi
  mv "$file.tmp" "$file"
  url=$(jq -r --arg k "$issue_key" '.issues[$k].url' "$file")
  jq -n -c --arg k "$issue_key" --arg u "$url" '{ok: {key: $k, url: $u}}'
  ;;
*)
  bad invalid "unknown op $op"
  ;;
esac
