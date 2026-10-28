# Release-gate decision — issue #144

**Decision: PASS for the measured RepoDB 0.3.2 candidate and the accepted workload set.** The criteria in `../../issue-134/evidence/comparison-criteria.json` are satisfied by the strict runset and quality evidence summarized below. This decision authorizes closing issue #144; it does not authorize or perform a release, publication, deployment, or push.

## Acceptance evidence

1. **Primary performance:** Five accepted alternating pairs on independent fresh clones/databases. RepoDB fresh-init median candidate/baseline ratio **0.756** (limit 0.90). The prescribed 100,000-resample paired geometric-mean bootstrap upper 95th percentile is **0.807** (must be below 1.0). All five measurements completed.
2. **Secondary timing:** Combined-26 fresh init 0.257; full unchanged refresh RepoDB 0.770 and combined-26 0.250; changed/no-op refresh RepoDB 0.854 and combined-26 0.922. Each median is within the 1.05 limit. The three single-sample outliers above 1.15 were investigated rather than removed. Twenty balanced same-database repetitions for pairs 2 and 4 showed overlapping ranges and no stable candidate-specific regression; exact ratios and raw diagnostic samples are in `analysis.json` and `raw/runset-02-strict/`.
3. **Quality/reference floors:** 52/52 exact positives, 26/26 exact negatives, 30/30 duplicate-explore references over pagination, ten expected JavaScript paths, and the same 11/26 passing semantic-marker languages as baseline. Across all 14 frozen issue-83 tasks, no semantic/combined per-task median regressed; aggregate reference/answer counts equal the baseline floors. The marker helper's 39 strict full-line-equality mismatches per version are disclosed; an independent audit verifies every marker-suite reference path/range and that all returned unit text occurs within the cited source excerpt. The issue-83 reference validator passed.
4. **Resources:** Database bytes and persisted vector counts match baseline. The larger default-eight-worker process-tree RSS was investigated with explicit one- and four-worker diagnostics; the evidence shows RSS scales with parser concurrency. The accepted criteria set no numeric memory ceiling. Embedder call/batch counts and GPU telemetry were unavailable and are explicitly not claimed.
5. **Evidence integrity:** Baseline/candidate binary hashes, source pins, tool versions, environment, all raw strict-pair outputs, analysis, and checksums are retained under this evidence directory. The initial tmpfs failure and the separate interleaved runset are preserved and excluded from the strict gate.

## Final-candidate status

The tested candidate is source commit `6544cff83d12bcfd3af5b5df5259a3b83d008051`, binary SHA-256 `ce084de6427d925816c7631537d3674213dd6c56973a3e0fe0a0414ba7a30fb8`. The implementation/test suite previously passed: 329 passed, 1 ignored; formatting, clippy with warnings denied, and diff checks passed (recorded in `/tmp/automate-spec-repodb-20261027/run.md`). No release artifacts were published, no deployment occurred, and no branch was pushed.

The measured speedup is limited to the pinned workloads and this shared host. The frozen issue-83 tasks are sparse and are not representative of general search relevance.
