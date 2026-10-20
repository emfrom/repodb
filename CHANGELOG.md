# Changelog

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
