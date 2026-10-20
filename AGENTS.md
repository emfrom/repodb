## RepoDB search

For repository code and documentation discovery, use RepoDB whenever it is available and applicable; treat this as repository policy, not an optional suggestion. Prefer `repodb search 'phrase'` for exact search of the checked-out commit, or add `--semantic` / `--combined` for similarity search. Search does not update the index. Run `repodb refresh` when indexed content may be stale. RepoDB covers indexed commits, so use filesystem tools for uncommitted changes and read known files directly. If RepoDB is unavailable, use the appropriate filesystem tools instead.

An opt-in Git post-commit hook is provided at `hooks/post-commit`. To enable it for this repository, configure `git config core.hooksPath hooks` (or an absolute path to this repository's `hooks` directory). Do not enable it without user approval.

If the hook or its configuration is missing, check for it and ask the user what to do; do not silently install it or ignore the missing hook.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five default triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

Use the single-context layout. See `docs/agents/domain.md`.

### Installations

For installations or removals, read `UNINSTALL.md` and update its linked ledger.
