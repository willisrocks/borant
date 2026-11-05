---
name: release
description: Use when cutting, preparing, or announcing a Borant release, writing release notes, choosing a patch, minor, or major bump, or when the Release workflow fails.
---

# Release Borant

A release is the `Release` workflow on Depot CI, on `main`. It runs `ci`, builds
the binaries and the container image, tags `vX.Y.Z`, publishes a GitHub Release,
and bumps the version on `main`. `docs/guide.md` ("Releases") describes each job.

Start a release on the machine that holds the Depot login. Other hosts have no
Depot login, so an agent on one of them must ask the owner of that machine.

## Steps

A release ships the tip of `main`. Merge the work first.

1. Make sure that the last `ci` run on `main` is green:
   `gh pr checks` on the last PR, or the `ci / *` checks on the tip of `main`. The
   release runs `ci` itself and stops if it fails, so this step only saves time.
2. List the changes since the last release: `just changes`.
3. Pick the bump from the changes:

   | The release has | Bump |
   |---|---|
   | Only fixes, docs, or maintenance | `patch` |
   | A new feature, or a change that needs an upgrade step | `minor` |
   | A break of a promise in `docs/guide.md`, at 1.0 or later | `major` |

   Before 1.0, a breaking change is a `minor` bump. The first release is
   always `0.1.0`, whatever the bump.
4. Write the notes to `target/release-notes.md`. Git ignores `target/`.
   Read `references/notes.md` before you write them.
5. Show the user the bump and the notes, and wait for approval. A release is
   public and cannot be undone.
6. Start it: `just release <bump> target/release-notes.md`.
7. Watch it with the run ID that `just release` prints:
   `depot ci status <run-id> --wait`.

If a job fails, read `references/troubleshooting.md`.
