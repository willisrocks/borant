# Release workflow failures

A failure before the `publish` job leaves no tag and no change on `main`.
Fix the cause and start the release again with the same bump.

| Error or symptom | Cause | Action |
|---|---|---|
| `release must run from main` | The run started from another branch | Start it with `--ref main`. `just release` does this. |
| `confirm must be release` | A run from the Actions tab without the confirmation | Type `release` in the confirm field. |
| `ci_not_green` | No green `ci` run at the tip of `main` | Wait for CI, or fix it, then release again. |
| `release_tag_exists` | The tag of the next version exists | Fetch the tags (`git fetch --tags`) and check them. Pick another bump if a release already used the version. |
| `version mismatch` in `build` | `BORANT_RELEASE_VERSION` did not reach the build | Check the `build` job env and `crates/borant/src/cli.rs` (`VERSION`). |
| `image` job fails at login or push | The workflow token cannot write packages | Check that the job has `packages: write`. |
| `publish` fails after the tag exists | The release object was not created | Run `gh release create` by hand with the files from the run artifacts, or delete the tag and release again. |
| `bump` fails | The push to `main` was rejected, often because `main` moved | Run `VERSION=X.Y.Z bash .github/scripts/set-version.sh`, commit `chore(release): vX.Y.Z`, and push. |
| The image is private | GitHub created the package as private | Open the package settings on GitHub and set the visibility to public. This is needed once. |
