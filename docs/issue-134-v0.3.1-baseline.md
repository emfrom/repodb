# Issue #134: v0.3.1 baseline for RepoDB 0.3.2

**Status: baseline gate closed per owner direction.** This is the pre-implementation baseline only. No 0.3.2 candidate was measured and no improvement is claimed. Issue #134 was closed after this evidence was published; this does not authorize implementation or claim a measured improvement.

## Pinned builds and workloads

- **Measured RepoDB binary:** `repodb 0.3.1`, built from the detached, clean source worktree at `f0a6b5d13ef3f8c5a4196aa655255a8f4f5d934d` (`v0.3.1`). Measurement binary SHA-256: `91ad2c7977d80580da35c2c8f851b1691d7519718299d40c35894e68e87e651f`.
- **Required full-refresh history:** RepoDB history rooted at peeled `v0.3.0`, `f7cc6bc5451204c5696f1bbef7e5575772820f47`, 231 reachable commits and one benchmark branch. This intentionally differs from the binary's source pin: it is the history the 0.3.2 spec explicitly selects for the before/after throughput comparison.
- **Combined language history:** generated from all 26 checked-in Semgrep fixtures at `55a722572845ff8556c6be2e7efdcf7d441fdf6e`; ten copies per fixture; 260 files in each of two commits; 23,630 tracked source bytes. HEAD: `6740e8af4d89bbb1b2b3577749fbf499d6c15e5e`. The generator, fixture hash, commits, file counts, and source bytes are in [`combined-26-manifest.txt`](issue-134/evidence/corpus/combined-26-manifest.txt).
- **Embedder:** real `generate_embeddings` 1.0.0 at source commit `1a2f677af874180fa663426ba7d8ec4c1c15f80d`; executable and daemon SHA-256s are recorded in [`environment.json`](issue-134/evidence/environment.json). The source checkout had one unrelated test-file modification; both executable scripts exactly matched their committed blobs.
- **Model:** `sentence-transformers/all-MiniLM-L6-v2`, revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`; model and tokenizer artifact hashes are recorded in `environment.json`. Model downloads were disabled. Semgrep Core was 1.181.0 (`0f6e09374a`), with its executable hash recorded there.
- Host: Linux 7.0.0-38-generic, Intel i5-13450HX, 16 visible logical CPUs. The run used warm caches, warmed the same embedder with two fixed strings before each runner invocation, and used 900-second RepoDB and 1,800-second outer command timeouts. No cold-cache run, CPU isolation, load trace, or GPU telemetry was collected.

The v0.3.1 release worktree passed the release build, `cargo test --all-targets` (306 passed, one ignored), Clippy, rustfmt, and diff checks; the clean source pin and full test output are in [`release-validation-f0a.log`](issue-134/evidence/raw/release-validation-f0a.log). After moving the original build directory, a subsequent build from the same clean f0a source produced a different executable hash (`a7fa5e…`); the exact measurement executable is no longer retained. Its original hash is preserved in the raw measurement records. `environment.json` records the verified rebuild and provenance. Use that verified rebuild consistently for any new paired before/candidate measurements; the historical unpaired timings below are context, not the paired denominator.

## Throughput baseline

Three runs were designated as the preplanned baseline set. Each starts with a fresh clone and database; `init` is the fresh full refresh, and `refresh` without `--changed` is a full unchanged-history refresh. Times are wall-clock seconds; the raw measurements include process-tree RSS samples and command output tails.

| Corpus / command | Run 1 | Run 2 | Run 3 | Median |
|---|---:|---:|---:|---:|
| RepoDB v0.3.0 history — fresh `init` | 360.438 | 365.157 | 368.522 | **365.157** |
| RepoDB v0.3.0 history — full `refresh` | 364.076 | 374.402 | 384.667 | **374.402** |
| Combined 26 — fresh `init` | 105.344 | 113.632 | 107.905 | **107.905** |
| Combined 26 — full `refresh` | 104.887 | 107.543 | 121.561 | **107.543** |

Each RepoDB index was 107,868,160 bytes and contained 46,681 indexed units/embedding rows from 231 commits. Each combined-26 index was 6,709,248 bytes and contained 2,782 indexed units/embedding rows from two commits. These persisted row counts are a safe proxy for generated vectors; embedder invocation/batch counts were not instrumented.

`refresh --changed` no-op is a different operation and is reported separately: 15 samples per corpus across the three indexes. RepoDB median **12.52 ms** (10.68–15.23 ms); combined-26 median **11.86 ms** (10.41–14.92 ms). Do not substitute these values for the full `refresh` measurements.

### Additional observations (not part of the preplanned triplicate)

One later full run per corpus measured RepoDB fresh/full-refresh at 419.246/401.506 seconds and combined-26 at 138.196/121.385 seconds. These are preserved in [`throughput-supplemental-run-4.json`](issue-134/evidence/raw/throughput-supplemental-run-4.json), not silently dropped or folded into the preplanned medians. They show that absolute timings vary materially, particularly on the small combined corpus. The actual candidate decision must therefore use the predeclared paired procedure below, report every pair, and treat unexplained variation as inconclusive.

A separate `run-baseline.py --measure-parser-stage` 50-ms process-sampling diagnostic estimated the union of time intervals with `semgrep-core` active: 303.49 s fresh / 254.87 s full refresh on RepoDB, and 54.41 / 51.64 s on combined-26. These include parser-process startup/teardown and are **not parser CPU time**. The extra process sampling makes this diagnostic unsuitable as a replacement for the primary wall-time series; raw results are in [`parser-diagnostic.json`](issue-134/evidence/raw/parser-diagnostic.json). Embedder invocation/batch counts and GPU utilization were not instrumented; persisted vector-row counts are listed above.

The refresh runner samples the RepoDB process tree and persistent embedder daemon RSS every 50 ms. In the preplanned runs, sampled fresh-refresh peak RSS was 1.52 GiB maximum on RepoDB and 1.22 GiB maximum on combined-26. These are observations, not memory gates. Query RSS samples also include the persistent embedder daemon.

## Search, relevance, and references

### Exact and duplicate controls on combined-26

- Three runs each passed **52/52** exact V1/V2 marker probes across 26 languages. Each run returned 130 references; every returned reference resolved to source containing the exact probe, and each target had the expected commit/path/line. That is 156 successful queries and 390 source-checked returned references across the three runs.
- **26/26** generated absent-marker negative controls returned no results. These are synthetic negative strings, not a broad false-positive study.
- A paginated exact-history explore returned all **30/30** occurrences over six pages, with a stable snapshot and unique reference IDs. It covered all ten expected JavaScript copy paths; every reference's commit/path/line contained the query. The control checks source spans and pagination, not general semantic correctness.
- Semantic-only marker probes found the expected V2 target in top five for **11/26 languages** on all three repetitions (33/78 attempts). Fifteen languages were 0/3. These repeats are not independent trials; treat this as a narrow per-language regression check, not a general semantic-quality estimate.

Raw exact, negative, explore, and semantic outputs are under [`evidence/raw/quality/`](issue-134/evidence/raw/quality/).

### Frozen 14-task source-derived suite

The issue-83 task definitions were run in exact, semantic, and combined modes, three times, against pinned RepoDB (`13639d6dbfed410bfa513f95861556acdf32cb5d`) and ProtonSync (`7c77aab91aff586591a00092a19760d5dc15e90a`) checkouts. The revised capture validates every returned reference against its pinned source commit/path/line; when text is present it is checked against the source excerpt (content) or diff-side excerpt. Byte-budget-omitted text is validated by source path/range.

The baseline is weak: exact mode returned zero results for all 42 task-runs. In both semantic and combined modes, answer-term checks passed **3/18** RepoDB and **6/24** ProtonSync task-runs (9/42 total); source-reference overlap passed **0/18** and **3/24** respectively (3/42 total). These scores are preserved as per-task regression floors only; they are not representative user tasks and do not support a broad relevance claim. The exact positive/negative language probes above provide the meaningful exact-search floor.

Query-resource samples for the task suite are also retained. Median exact/semantic/combined wall times were approximately 0.125/0.891/0.893 s on RepoDB and 0.126/0.865/0.862 s on ProtonSync. Measurements were one sample per task/mode, with 50-ms RSS sampling and the persistent embedder daemon included.

## Accepted comparison criteria

The independent review found a 10% RepoDB fresh-refresh target defensible and the 5% secondary non-regression tolerance reasonable, while noting that five pairs yield a fragile interval and that the weak task suite must not be overinterpreted. The precise paired-bootstrap method, failure policy, outlier rule, and quality floors are recorded in [`comparison-criteria.json`](issue-134/evidence/comparison-criteria.json): five alternating before/candidate pairs; primary median candidate/baseline ratio ≤0.90 and one-sided 95% bootstrap upper bound for the geometric-mean ratio below 1.0; secondary medians ≤1.05; and an investigation trigger at any unexplained individual ratio above 1.15. Exact, negative, duplicate-explore, semantic-marker, and task-suite regression rules are also explicit.

These criteria are accepted for the candidate comparison per the owner-directed closure of #134. The reviewer specifically cautioned that the 14-task suite is sparse, the semantic marker results are not 78 independent trials, and the duplicate/reference controls must verify every returned source span. The updated captures now perform those source checks. No candidate result exists, and no implementation or improvement claim follows from closing the baseline gate.

## Reproduction artifacts and limits

- Benchmark programs and evidence: [`issue-134/`](issue-134/).
- Machine-readable provenance: [`environment.json`](issue-134/evidence/environment.json).
- Raw throughput, resource, and search evidence: [`evidence/raw/`](issue-134/evidence/raw/).
- The scaled Git corpus itself is generated from the pinned fixture inputs; its manifest and generator hash are retained. It contains tiny repeated fixtures, not varied real-world projects or large files. Regenerate with `python3 docs/issue-134/generate-corpus.py /tmp/combined-26 --copies 10`.
- The throughput runner is `docs/issue-134/run-baseline.py`; the primary wall-time series used its equivalent 50-ms RSS/timeout measurement path with parser-stage sampling disabled. The current script adds optional `--measure-parser-stage` diagnostics and incremental raw-file persistence; neither changes the primary timing logic. The baseline used the v0.3.1 binary, `--repeat 3 --process-timeout 900 --timeout 1800`, and the pinned repo/scaled-corpus commits listed above. `environment.json` and raw command arrays record binary, tool, and model provenance. Evidence files are checksum-listed in [`SHA256SUMS.txt`](issue-134/evidence/SHA256SUMS.txt).
- The primary source history is one fixed 231-commit history. No candidate was measured, no cold-cache results exist, and no claim is made about general semantic relevance, per-query GPU memory, embedding work counts, or parser CPU time.

**Decision:** the timing and baseline search evidence are published and #134 is closed per owner direction. No candidate was measured; do not claim improvement or start implementation before following the accepted comparison criteria.
