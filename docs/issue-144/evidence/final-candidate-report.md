# Final candidate report — RepoDB 0.3.2 / issue #144

## Candidate and baseline

- Candidate source: commit `6544cff83d12bcfd3af5b5df5259a3b83d008051` on `integration/repodb-032` (the code under test was unchanged from the prior verified implementation).
- Candidate release-profile binary: `/home/emfrom/opt/rust/cargo/target/release/repodb`, SHA-256 `ce084de6427d925816c7631537d3674213dd6c56973a3e0fe0a0414ba7a30fb8`.
- Baseline: verified v0.3.1 binary from source commit `f0a6b5d13ef3f8c5a4196aa655255a8f4f5d934d`, SHA-256 `a7fa5ee72e5ff83419918275c8eb813891a5e942997133ad99bcb2bffb395f9a`.
- The implementation commit's full suite previously passed: 329 passed, 1 ignored; formatting, clippy with warnings denied, and diff checks passed. These results are also recorded in the authorized-run ledger.
- Real embedder, daemon, model revision, Semgrep Core, and benchmark-script hashes are recorded in [`environment-final.json`](environment-final.json). The candidate and baseline used the same pinned tools and offline model.

## Paired full-refresh measurements

The strict set used the accepted five-pair alternating order, one independent fresh clone/database per binary per pair, warm-cache-only operation, and a fixed two-string embedder warmup before each full runner invocation. `init` and the subsequent full unchanged `refresh` were timed within each runner. No-op changed refreshes were measured only after both full-throughput commands in each pair. The RepoDB history was pinned to `f7cc6bc5451204c5696f1bbef7e5575772820f47` (231 commits); combined-26 used head `6740e8af4d89bbb1b2b3577749fbf499d6c15e5e` (26 fixtures, 260 files per commit, two commits).

The RepoDB fresh-init median paired ratio was **0.756**. The accepted 100,000-resample geometric-mean bootstrap (seed 134032, nearest-rank upper 95th percentile) was **0.807**, below 1.0; the maximum allowed median was 0.90 and the upper bound had to be below 1.0. The primary gate passes.

Secondary medians were 0.257 for combined-26 fresh init, 0.770 for RepoDB full unchanged refresh, 0.250 for combined-26 full unchanged refresh, 0.854 for RepoDB `refresh --changed`, and 0.922 for combined-26 `refresh --changed`. All meet the 1.05 median threshold. The individual changed/no-op ratios above 1.15 were not removed: RepoDB pair 2 (1.288), combined-26 pair 2 (1.182), and combined-26 pair 4 (1.342). Investigation alternated both binaries 20 times against each same unchanged database for pairs 2 and 4; all crossed medians were below 1.15 and ranges overlapped. The same-database crossover changed the sign/magnitude of small differences, supporting startup/SQLite-open/shared-host noise rather than a stable binary-specific regression. These diagnostics are separate and did not replace any gate sample.

Every strict-run measurement completed successfully. A prior baseline-only attempt on `/tmp` failed with SQLite `disk I/O error`; it is retained as an incident and excluded. Another complete five-pair set is also retained but not pooled because changed/no-op runs were interposed between paired full-throughput runs. The strict set uses fresh data roots on the larger local filesystem, reachable through `/tmp` aliases; details are in [`environment-final.json`](environment-final.json).

## Search-quality and references

The final binary was checked on the combined-26 index and the 14 frozen issue-83 tasks at their immutable source pins:

- Exact positive: 52/52 expected marker/version hits with correct commit/path/line.
- Exact negative: 26/26 absent-marker queries returned no results.
- Duplicate explore: all 30 references were paginated over six pages; the snapshot identity was stable, IDs unique, all ten expected JavaScript paths present, and all references resolved to source.
- Semantic markers: 33/78 successful retrievals for both baseline and candidate, corresponding to the same 11/26 languages passing all three repeats. This remains a narrow marker-based check.
- Frozen issue-83 tasks: no semantic or combined per-task median regressed for source recall, source-reference correctness, or answer correctness. Both versions returned 3/42 source-reference-correct and 9/42 answer-correct outcomes per mode. These sparse results are regression floors, not general relevance claims.
- Every issue-83 task reference was validated against the pinned Git source. In the 26-language marker suite, the existing helper's strict full-line equality flag is false for 39 content snippets per version. The independent audit in `raw/quality/combined-26/semantic-reference-audit.json` confirms that all 303 references per version resolve to valid source ranges and every returned unit string occurs within its cited source excerpt. The strict flag and the exact-equality differences are retained in raw evidence.

The query-resource suite ran once per task/mode, baseline then candidate; its timings are descriptive and were not treated as a paired latency gate. Median exact/semantic/combined timings were RepoDB 0.122/0.850/0.864 seconds baseline vs 0.130/0.898/0.918 candidate, and ProtonSync 0.120/0.816/0.841 vs 0.133/0.748/0.934. Query peak-RSS samples include the persistent embedder daemon and were below about 644,000 KiB for both versions.

## Resources and limits

Across the five strict pairs, RepoDB fresh-refresh process-tree peak RSS had a median of about 0.94 GiB baseline and 2.49 GiB candidate (candidate maximum about 2.53 GiB). Combined-26 medians were about 0.67 GiB and 1.30 GiB. The accepted criteria define no numeric RSS ceiling but require investigation. Explicit-worker diagnostics support native parser concurrency as the driver: on the same candidate indexes, RepoDB peaks were 990,520 KiB at one worker, 1,782,624 KiB at four, and about 2.6 GiB at default eight; combined-26 peaks were 704,712, 1,013,940, and about 1.3 GiB. These one-run diagnostics are resource investigations only, not paired performance comparisons.

Database sizes and persisted vector counts were equal across baseline/candidate: RepoDB 107,868,160 bytes and 46,681 `unit_embeddings` rows; combined-26 6,709,248 bytes and 2,782 rows. Embedder invocation/batch counts and GPU telemetry were not instrumented, so no claim is made about them.

## Reproduction

From the integration worktree:

```bash
python3 docs/issue-144/analyze.py
```

This validates binary/source pins and raw sample completeness, computes all accepted ratios and the prescribed bootstrap, compares quality floors, reports resources, and writes `evidence/analysis.json`. Raw measurements are under `evidence/raw/runset-02-strict/`; checksums are in [`SHA256SUMS.txt`](SHA256SUMS.txt).
