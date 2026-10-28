# Issue #144 — final paired performance and search-quality gate

The final candidate is the release-profile binary built from `6544cff83d12bcfd3af5b5df5259a3b83d008051` (SHA-256 `ce084de6427d925816c7631537d3674213dd6c56973a3e0fe0a0414ba7a30fb8`). The paired baseline is the verified v0.3.1 binary (SHA-256 `a7fa5ee72e5ff83419918275c8eb813891a5e942997133ad99bcb2bffb395f9a`). Neither binary was published or released.

## Decision

**The #144 acceptance gate passes for the measured workloads.** The strict five-pair series met the primary performance threshold and bootstrap rule, every secondary median met its threshold, and the triggered `refresh --changed` outliers were investigated with balanced same-database crossover runs. Search/reference floors passed without a per-task issue-83 regression. The memory increase was investigated and is attributable to the intended worker-count trade-off; the accepted criteria set no numeric RSS ceiling.

This is a workload-specific gate result, not a general search-relevance claim or a guarantee of the same speedup on other hosts/corpora. The issue may be closed with the evidence below. No push, release, deployment, or publication occurred.

## Strict paired results

Ratios are candidate/baseline; lower is faster. The accepted order was baseline/candidate, candidate/baseline, baseline/candidate, candidate/baseline, baseline/candidate. Each side used an independent fresh clone and database. Full `refresh` followed `init` within each runner invocation; `refresh --changed` was measured only after both full-throughput invocations in each pair.

| Workload | Baseline median | Candidate median | Median paired ratio | Gate |
|---|---:|---:|---:|---|
| RepoDB fresh init, 231-commit v0.3.0 history | 396.32 s | 308.01 s | 0.756 | Pass (maximum 0.90; bootstrap upper bound 0.807 < 1.0) |
| Combined-26 fresh init | 121.33 s | 29.44 s | 0.257 | Pass (maximum 1.05) |
| RepoDB full unchanged `refresh` | 390.15 s | 315.24 s | 0.770 | Pass (maximum 1.05) |
| Combined-26 full unchanged `refresh` | 120.65 s | 29.68 s | 0.250 | Pass (maximum 1.05) |
| RepoDB `refresh --changed` no-op | — | — | 0.854 | Pass median (maximum 1.05); pair 2 outlier investigated |
| Combined-26 `refresh --changed` no-op | — | — | 0.922 | Pass median (maximum 1.05); pair 2 and 4 outliers investigated |

The primary paired ratios were `0.865, 0.795, 0.756, 0.641, 0.724`. The 100,000-resample geometric-mean bootstrap used `random.Random(134032)` and nearest-rank one-sided 95th percentile; its upper bound was `0.807`.

The one-shot changed/no-op runs were only about 10–22 ms. The outliers were retained. For pairs 2 and 4, both binaries were then alternated 20 times against each unchanged database. All crossed medians were below the 1.15 investigation trigger, baseline/candidate ranges overlapped, and changing which database was used changed which binary was slightly faster. These diagnostic repeats were not substituted into the five-pair ratios. Raw comparisons and investigations are in [`analysis.json`](evidence/analysis.json) and `raw/runset-02-strict/`.

## Search and reference quality

- **52/52 exact positive probes** retrieved the expected V1/V2 content and correct commit/path/line; all returned source references validated.
- **26/26 exact-negative controls** returned no results.
- Duplicate `explore` pagination returned **30/30** results over six pages with stable snapshot identity, unique reference IDs, all ten expected JavaScript paths, and source-valid references.
- Semantic marker retrieval passed on **11/26 languages** (33/78 repeated attempts) for both baseline and candidate, with the same passing-language set. This is only a narrow regression check.
- Across the 14 frozen issue-83 tasks, no semantic/combined per-task median source-recall, reference-correctness, or answer-correctness score regressed. Aggregate outcomes remained 3/42 source-reference-correct and 9/42 answer-correct per mode for both versions. The task set is sparse and not representative.
- All issue-83 task references validated against the pinned source. In the marker suite, the existing strict helper flags 39 content-unit strings per binary because it requires each parsed unit to equal the entire source line. A separate retained audit confirms all 303 returned references per binary resolve to valid commit/path/line ranges and every returned unit string occurs within its cited source excerpt; the strict equality differences are disclosed, not hidden.

Query-resource timings are descriptive rather than a paired latency gate: the quality/resource script ran baseline then candidate, once per task/mode. It reports exact/semantic/combined medians and sampled RSS in `analysis.json`.

## Resource findings

Whole-refresh RSS includes the persistent embedder daemon. The median RepoDB fresh-refresh peak was about **0.94 GiB baseline vs 2.49 GiB candidate** (candidate maximum about 2.53 GiB); combined-26 was about **0.67 GiB vs 1.30 GiB**. Database sizes and persisted vector counts were unchanged between binaries: RepoDB 107,868,160 bytes with 46,681 indexed/vector rows; combined-26 6,709,248 bytes with 2,782 indexed/vector rows.

The material RSS increase was investigated with explicit-worker diagnostics on the same candidate indexes. RepoDB peaks were approximately 0.94 GiB at one worker, 1.70 GiB at four, and 2.49 GiB at the default eight; combined-26 peaks were approximately 0.67, 0.97, and 1.30 GiB. This supports concurrency as the resource driver. These one-run diagnostics are not paired performance claims and do not change the default-worker gate results. The accepted criteria specify no numeric memory ceiling. Embedder invocation/batch counts and GPU telemetry were unavailable; persisted vector counts are reported instead.

## Evidence and deviations

- [`evidence/release-gate-decision.md`](evidence/release-gate-decision.md): detailed gate decision.
- [`evidence/final-candidate-report.md`](evidence/final-candidate-report.md): reproducibility and findings.
- [`evidence/analysis.json`](evidence/analysis.json): ratios, bootstrap, quality checks, resources, and raw-file inventory; reproduce with `python3 docs/issue-144/analyze.py`.
- [`evidence/environment-final.json`](evidence/environment-final.json): binary hashes, workloads, runner protocol, host, tools, and storage location.
- [`evidence/SHA256SUMS.txt`](evidence/SHA256SUMS.txt): checksums for retained evidence.
- [`evidence/incidents/initial-tmpfs-failure.md`](evidence/incidents/initial-tmpfs-failure.md): baseline-only failed attempt on tmpfs, retained but excluded.
- `evidence/raw/runset-01-interleaved/`: an earlier complete set retained separately because no-op refreshes were interposed between each pair's full-throughput runs. It is not pooled into the strict gate.
- `evidence/raw/runset-02-strict/`: the accepted five-pair series and separate outlier diagnostics.
