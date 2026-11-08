# Release workflow failures

A failure before the `publish` job leaves no tag and no change on `main`.
Fix the cause and start the release again with the same bump.

| Error or symptom | Cause | Action |
|---|---|---|
| `release must run from main` | The run started from another branch | Start it with `--ref main`. `just release` does this. |
| `confirm must be release` | A dispatch without the confirmation | Pass `--input confirm=release`. `just release` does this. |
| A `ci / *` job fails | The release runs `ci` at the release commit, and no later job starts | Fix the failure on `main`, then release again. |
| `no release-darwin run for request` | The dispatch of the macOS builds did not start a GitHub Actions run | Check that `release-darwin.yml` is on `main` and that GitHub Actions is on, then release again. |
| `no release-image run for request` | The dispatch of the image push did not start a GitHub Actions run | Check that `release-image.yml` is on `main` and that GitHub Actions is on, then release again. |
| `release_tag_exists` | The tag of the next version exists | Fetch the tags (`git fetch --tags`) and check them. Pick another bump if a release already used the version. |
| `version mismatch` in `build` | `BORANT_RELEASE_VERSION` did not reach the build | Check the `build` job env and `crates/borant/src/cli.rs` (`VERSION`). |
| `release-image` fails with `permission_denied: Invalid token` | The Depot project has no OIDC trust for this repo | In the Depot project's settings, add a GitHub trust relationship for this owner and repo. |
| `release-image` fails at login or push | The workflow token cannot write packages | Check that the job has `packages: write`. |
| `publish` fails after the tag exists | The release object was not created | Run `gh release create` by hand with the files from the run artifacts, or delete the tag and release again. |
| `bump` fails | The push to `main` was rejected, often because `main` moved | Run `VERSION=X.Y.Z bash .github/scripts/set-version.sh`, commit `chore(release): vX.Y.Z`, and push. |
| Depot CI cannot start `release.yml` | The workflow is dispatch only, and it needs a Depot login | Run `just release` on the machine that holds the login. |
| The image is private | GitHub created the package as private | Open the package settings on GitHub and set the visibility to public. This is needed once. |
