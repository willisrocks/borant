# Writing release notes

The model for the shape and the tone is `docs/release/notes.example.md`.
Copy its structure.

## Input

- `just changes` lists the commit subjects since the last tag.
- For a commit whose subject does not explain its effect on users, read it:
  `git show --stat <sha>` and the PR body (`gh pr view <number>`).

## Shape

1. One paragraph of two or three sentences: what the release gives users, and
   whether an upgrade needs action. For the first release, say that it is the
   first release.
2. `## Upgrade notes`: only if users must do something. Name the action first.
3. `## New`, `## Fixes`, `## Maintenance`: one line for each change, in that
   order. Leave out a section that has no entries.

The workflow adds the install command and the image name above the notes.
Do not repeat them.

## Each line

- State the effect for a user, not the code change. Write "A broadcast that
  arrived during the last tool call is no longer lost", not "Fix drain in
  event()".
- End with the PR number in parentheses, for example `(#42)`. A squash merge
  changes the SHAs, so use a short SHA only for a commit that reached `main`
  without a PR.
- Use plain English: short sentences, active voice, no hype words.
- Group related commits into one line. Leave out commits that only fix an
  earlier commit in the same release.

## Upgrade notes are required when

These apply only when users upgrade from an earlier release. The first
release has no upgrade notes.

- A type in `crates/borant-core/src/proto.rs` changed. Hub and hosts then
  must upgrade together.
- The session-log format in `crates/borant-core/src/model.rs` changed.
- A key in `hub.toml` or `host.toml` changed its name or its meaning.
- The Claude Code plugin hooks or the Pi extension changed. Users must update
  their installed copy.
