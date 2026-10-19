# Research: Additional searchable source context and bounded storage

## Summary
RepoDB should treat direct dependency source and whole-change context (including additions, deletions, and renames) as co-primary candidates, then add repository documentation and standards that the repository explicitly names. Keep acquisition local-only by default; let users/agents opt into public-source fetching and transitive dependencies. Bound growth by deduplicating immutable content, storing compact source references and metadata, making large context hydration on-demand, and reporting estimated net growth before import—not by imposing a hard size cap. Thresholds of 50 MB or 2× current DB are proposed warning triggers, not measured optimal limits.

This is a preliminary, source-based design brief, not a benchmark. No storage/embedding measurements were available. The October 19 grill decisions are treated as product constraints supplied in the task; they were independently found in the historical Pi session, but not independently verified by the researcher. **Source validation is incomplete:** the researcher ran without its expected web-search/fetch/source-check tools. Verify the linked primary references before treating factual claims as established.

## Findings

1. **Dependency repositories and complete diffs are co-primary candidates.** Direct dependencies are a bounded default because manifests and lockfiles identify concrete dependency/version relationships; diffs can preserve change context, including removed or renamed paths, absent from the current checkout. Preserve full patch context as retrievable source, while evaluating compact searchable representations and on-demand hydration. Git documents diff and rename behavior; this does not itself prescribe a RepoDB indexing strategy. **References:** [Git diff documentation](https://git-scm.com/docs/git-diff), [Cargo lockfile reference](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html). Recommendation is an inference; confidence medium, pending source verification.

2. **Use explicit dependency scope and version identity.** Default to direct dependencies, recording ecosystem and resolved version/revision with manifest/lockfile provenance. Offer selective transitive inclusion rather than recursively importing the full graph. **References:** [Cargo lockfile reference](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html), [Git submodule documentation](https://git-scm.com/docs/gitsubmodules). Scope is a recommendation, not a source mandate; confidence medium.

3. **Include repo-local docs and explicitly referenced standards as candidates.** Resolve project-local documentation and standards explicitly named by the project before considering broad external catalogs. Record source path/URL, retrieval time, version/edition, content hash, and page/section when available. Rights and reuse terms vary by source; fetching does not imply permission to redistribute full text. **References:** [W3C Intellectual Rights Policy](https://www.w3.org/policies/ipr/), [W3C document licensing](https://www.w3.org/copyright/software-license/). Verify applicability per source; confidence medium.

4. **Keep local-only processing as the baseline.** Human/agent-selected imports preserve an explicit acquisition boundary. Treat public-source fetching as a separate opt-in; pin immutable revisions where possible, preserve provenance, and do not send source to external embedding services absent explicit user choice. **References:** [GitHub REST API: repository contents](https://docs.github.com/en/rest/repos/contents), [GitHub REST API: commits](https://docs.github.com/en/rest/commits/commits). Privacy baseline is a recommendation; confidence medium.

5. **Control growth without forbidding large imports.** Deduplicate content by hash while keeping source identity separate; avoid embedding identical content repeatedly; separate original payload from searchable units/embeddings; use compact metadata and source references; evaluate on-demand hydration for full diffs/documents. Allow imports of any size. Warn at estimated net DB growth of ≥50 MB or >2× the existing DB; show source size and estimated growth, falling back to source size if growth cannot be estimated. These thresholds are grill decisions, not measured optima. **References:** [SQLite database file format](https://www.sqlite.org/fileformat.html), [SQLite FTS5](https://www.sqlite.org/fts5.html), [Git object model](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects). Architecture is an inference; confidence medium.

6. **Measure before fixing retention and embedding policy.** No RepoDB-specific evidence was found for bytes per indexed unit, embedding cost, compression ratio, refresh time, or quality impact by context type. Measure raw source, searchable units, metadata, and embeddings separately; report deduplication, growth per source MiB, import/refresh time, and retrieval quality. Compare full diff indexing with compact representations plus hydration, and direct-only with selected transitive dependencies. Confidence high that project-specific measurements are missing; medium for the proposed evaluation.

## Contradictions and missing evidence

No source contradiction was identified, but the researcher could not fetch or validate the links in this run. Git's documentation describes patches and renames; it does not settle RepoDB's representation. No RepoDB-specific growth/quality benchmarks, source-by-source license review, universal retention policy, or empirical validation of the 50 MB / 2× thresholds is available here. Dependencies and local repositories can contain sensitive or third-party content, so locality alone does not establish rights or safety.

## Recommendation and evaluation plan

Treat these as explicit, independently selectable source categories: repository source, complete change/diff context, direct dependencies, repo-local docs, and explicitly referenced external standards. Selective transitive dependencies and network acquisition remain opt-in. Preserve immutable content identity and useful source references, including old/new revision and path information for changes. Keep full content retrievable while testing compact searchable representations and on-demand hydration.

Allow imports of any size. Estimate deduplicated net growth and warn at the agreed ≥50 MB or >2× current DB thresholds; if estimation is unavailable, use source size. Do not hard-block based on size; approval remains governed by applicable policy.

Before setting implementation defaults, run a reproducible evaluation across small, medium, and large repositories. Measure raw/index/embedding bytes, deduplication, import/refresh cost, and query usefulness for dependency questions, change rationale, documentation, and standards. Compare full versus hydrated diffs and direct-only versus selected transitive dependency scope; report retrieval quality and storage/operational cost separately. Verify source references against pinned revisions and check each source's license/terms before network acquisition or retention.

## References

- [Git diff documentation](https://git-scm.com/docs/git-diff) — diff and rename behavior.
- [Cargo lockfile reference](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html) — resolved dependency versions.
- [Git submodule documentation](https://git-scm.com/docs/gitsubmodules) — revision-linked repository context.
- [W3C Intellectual Rights Policy](https://www.w3.org/policies/ipr/) and [W3C document licensing](https://www.w3.org/copyright/software-license/) — rights caveat for standards.
- [GitHub REST API: repository contents](https://docs.github.com/en/rest/repos/contents) and [commits](https://docs.github.com/en/rest/commits/commits) — source/revision API references.
- [SQLite database file format](https://www.sqlite.org/fileformat.html), [SQLite FTS5](https://www.sqlite.org/fts5.html), [Git objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) — storage, indexing, and content-addressing concepts.

## Next steps

Verify the primary-source references and their applicability, then run the cost/quality fixture before making storage, retention, or import defaults normative.
