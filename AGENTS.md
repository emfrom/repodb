## RepoDB search

Use `repodb search 'phrase'` for exact search of the checked-out commit, or add `--semantic` / `--combined` for similarity search. Search does not update the index. Run `repodb refresh` on demand after ref changes or whenever indexed content may be stale.

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

For dependency installations or removals, read `UNINSTALL.md` and update its linked ledger.
