# Changelog

## v0.3.0

- Index committed file content, commit messages, and both sides of historical diffs with Git-backed source references; add bounded history selection and reference-only exploration.
- Add opt-in staged/worktree search with explicit provenance and semantic mapping to live text; expose capability and coverage status in machine-readable output.
- Bound and paginate search responses, group related change evidence, and provide focused agent skills for search, history, status, and imports.
- Make single-URL website snapshots explicitly refreshed, versioned and searchable offline; add import-size modes and preserve the last successful snapshot on fetch failure. Link crawling remains deferred.
- **Release verification exception:** the operator explicitly waived the 0.3.0 baseline-derived numeric quality/performance gates for this release. #83 and #84 were closed as superseded, not passed. No baseline comparison or benchmark-backed performance/quality claim is made; see `docs/repodb-0.3.0-spec.md`. The separate v0.3.0 baseline gate for 0.3.1 remains in effect.

## v0.2.3

- Document the proposed 0.3.0 search and import specification and supporting research; no 0.3.0 features are implemented in this release.

## v0.2.1

- Store Git history text as blob-backed byte and line ranges, hydrating results from Git objects at search time.
- Add `repodb refresh --changed` for current-branch first-parent updates; the opt-in post-commit hook uses this scoped mode.

## v0.2.0

- Index imported PDF documentation by paragraph and preserve page metadata.
- Versioned baseline immediately preceding scoped incremental refresh.

## v0.0.1

- Initial RepoDB CLI release with exact search scoped to the checked-out commit.
