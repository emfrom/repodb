# Changelog

## v0.3.0 (release pending verification)

- Index committed file content, commit messages, and both sides of historical diffs with Git-backed source references; add bounded history selection and reference-only exploration.
- Add opt-in staged/worktree search with explicit provenance and semantic mapping to live text; expose capability and coverage status in machine-readable output.
- Bound and paginate search responses, group related change evidence, and provide focused agent skills for search, history, status, and imports.
- Make single-URL website snapshots explicitly refreshed, versioned and searchable offline; add import-size modes and preserve the last successful snapshot on fetch failure. Link crawling remains deferred.
- **Release pending:** the approved 0.3.0 specification still requires a reproducible real-model baseline, numeric quality/performance thresholds and verified release gates. Closing #83/#84 as superseded did not satisfy or remove those requirements. Do not publish or tag v0.3.0 as a verified release until the contract is resolved.

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
