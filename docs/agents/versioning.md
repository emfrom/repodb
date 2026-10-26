# Version-bump workflow

When changing the project/package version, update the versioned implementation and its metadata, then run the applicable tests and release reviews. Keep the changelog and user-facing version text consistent with the selected version; distinguish an unreleased candidate from a published release.

A version bump is not complete until the checked, reviewed version-bump branch is merged into `main` and `main` contains the versioned changes. Fetch and inspect the current `main` first; use a normal fast-forward or merge, never rewrite shared history. If `main` has diverged, conflicts arise, or branch protection requires a pull request, stop and use the repository's approved merge path rather than forcing the update. After merging, verify the version on `main`; push the updated `main` when the task authorizes pushing.

Merging to `main` does not create a release. Tags, package publication, deployment, and release actions still require separate authorization and their applicable release gates.
