## RepoDB search

For repository code and documentation discovery, use RepoDB whenever it is available and applicable; treat this as repository policy, not an optional suggestion. Prefer `repodb search 'phrase'` for exact search of the checked-out commit, or add `--semantic` / `--combined` for similarity search. Search does not update the index. Run `repodb refresh` when indexed content may be stale. RepoDB covers indexed commits, so use filesystem tools for uncommitted changes and read known files directly. If RepoDB is unavailable, use the appropriate filesystem tools instead.

An opt-in Git post-commit hook is provided at `hooks/post-commit`. To enable it for this repository, configure `git config core.hooksPath hooks` (or an absolute path to this repository's `hooks` directory). Do not enable it without user approval.

If the hook or its configuration is missing, check for it and ask the user what to do; do not silently install it or ignore the missing hook.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Release security reviews

Security reviews follow the release cadence in `docs/reviews/README.md`. For review-required releases, follow `docs/reviews/SECURITY.md` and record the review plan, evidence, findings, and release decision in `docs/reviews/security/repodb-<version>.md`. Agents do not create or output tickets.

### Version bumps

When changing a project/package version, follow `docs/agents/versioning.md`; a checked, reviewed version bump must be merged to `main` before it is complete.

### Triage labels

Use the five default triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

Use the single-context layout. See `docs/agents/domain.md`.

### Installations

For installations or removals, read `UNINSTALL.md` and update its linked ledger.
