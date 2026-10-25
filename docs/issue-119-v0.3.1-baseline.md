# Issue #119 — released v0.3.1 search baseline

Status: **candidate-only baseline measured; not an old/new comparison or a release-gate pass.** This run records the released v0.3.1 build on the small, source-derived multi-repository task set. It does not retroactively satisfy the v0.3.1 waiver or establish general search quality. The source-derived task set is not user-approved ground truth and is explicitly not claimed representative.

## Results

The evaluator ran the 14 tasks in `benchmarks/issue-83/tasks.json` (6 RepoDB, 8 ProtonSync) with top `k=5` and the common 5,000-byte response budget. Each task was run in three fresh CLI processes; all task scores were identical across repeats. `answer_correct` is a lexical check that every explicit task answer term appears in returned result text. `source_reference_correct` requires every expected path/line span to overlap a returned reference. `recall@5` is the fraction of expected references with such an overlap. These mechanical indicators are not human judgments or calibrated quality estimates.

| Corpus | Tasks | Answer-term checks | All expected references correct | Mean recall@5 |
|---|---:|---:|---:|---:|
| RepoDB Rust/docs | 6 | 1/6 (16.7%) | 0/6 (0%) | 0% |
| ProtonSync TypeScript/tests/license | 8 | 2/8 (25%) | 1/8 (12.5%) | 12.5% |
| **Overall** | **14** | **3/14 (21.4%)** | **1/14 (7.1%)** | **7.1%** |

| Corpus | Refresh wall time¹ | Refresh peak RSS² | SQLite bytes | Indexed units (unsupported) | Query latency, median (range)³ |
|---|---:|---:|---:|---:|---:|
| RepoDB | 147.84 s | 263,008 KiB | 105,738,240 (100.82 MiB) | 45,871 (2) | 442.22 ms (405.57–476.80 ms) |
| ProtonSync | 42.63 s | 162,576 KiB | 45,129,728 (43.04 MiB) | 18,926 (0) | 441.43 ms (403.29–477.16 ms) |

¹ One `init` per corpus; total sequential refresh time was 190.48 s. Refresh repeat variance is unavailable. ² GNU `time` peak RSS for the measured command; the persistent embedding service's memory is not separately measured. ³ 18 RepoDB and 24 ProtonSync query runs (3 per task). Across all 42, median latency was 441.43 ms, range 403.29–477.16 ms; median per-task three-run range was 38.56 ms. Median query-process RSS was 23,996 KiB (maximum 24,336 KiB). Raw response size had a 4,696-byte median and 4,916-byte maximum. The host OS cache was not dropped; these are fresh-process runs, not cold-cache trials.

## Pins and environment

- RepoDB release source: clean `v0.3.1` tag, `f7cc6bc5451204c5696f1bbef7e5575772820f47`; binary `/tmp/repodb-120-target/release/repodb`, version `0.3.1`, SHA-256 `30e876d3d480207bce4b166658e4c6b7555fe8cc53c3719e6b4d49a3070c4aa5`. The recorded build was `CARGO_TARGET_DIR=/tmp/repodb-120-target cargo build --release --all-targets` from the clean tag checkout (see `docs/reviews/security/issue-120-v0.3.1-verification.md`).
- RepoDB corpus: `13639d6dbfed410bfa513f95861556acdf32cb5d`; ProtonSync corpus: `7c77aab91aff586591a00092a19760d5dc15e90a`. Each benchmark clone had only one `benchmark-corpus` ref at its pin; this prevents unrelated local branch refs from changing the indexed HEAD. The runner verifies both pins and rejects additional refs or preexisting indexes.
- Embedder: `generate_embeddings` 1.0.0, actual direct executable at `/home/emfrom/bin/generate_embeddings` (resolved path `/home/emfrom/src/projects/generate_embeddings/generate_embeddings`), SHA-256 `4ee97a7df677a156c617502ef9e9cd0d7d422fce02062350e81276a72551267f`; model `sentence-transformers/all-MiniLM-L6-v2`, cached model revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`. The embedding source checkout is at `938d9ed73402249e5ac407afed6e2159bc73dc9e` but was **dirty** during the run (`README.md`, `generate_embeddings`, `generate_embeddings.daemon`, and `tests/test_daemon_cli.py` modified). The actual daemon script SHA-256 was `2a5f7bfe71592c62232ae08996aa123163f9bb5f3a4019b25174699e6864ec0f`. The run used the direct configured executable with no wrapper or input rewriting; runtime hashes and dirty paths are retained in the raw JSON. The dirty source state is a reproducibility limitation; the source commit alone does not identify the measured executable.
- Semgrep Core: `1.181.0 (0f6e09374a)`, SHA-256 `14f4b18720d881156bc0cc507b19130aa8d8db0882a5a479fb3919aadefca857`.
- Host: Ubuntu 26.04.1 LTS, Linux `7.0.0-38-generic`, x86_64; Intel Core i5-13450HX, 16 logical CPUs, 16.46 GB RAM; an NVIDIA GeForce RTX 5060 Laptop GPU was present, but inference device/utilization was not measured. Python 3.13.14, `uv` 0.12.10, Rust 1.99.0, Cargo 1.99.0. `HF_HUB_OFFLINE=1` and `TRANSFORMERS_OFFLINE=1`; `HOME` retained and RepoDB config isolated via `XDG_CONFIG_HOME`.
- The runner issued a direct two-item embedder batch before timing to warm the local model service; it returned two finite, nonzero 384-dimensional vectors in 4.80 s. Warm-up time is excluded from refresh/query figures. The model-service startup/cold latency is not measured. The model service persists separately; GPU use and embedding subprocess counts/bytes are not measured (the harness records those as unavailable, not zero).

## Evaluation limits and thresholds

The only numeric evaluation constraints applied were top 5 results and the 5,000-byte serialized response budget. No numeric quality or performance pass/fail cutoffs were specified for this v0.3.1 baseline; the v0.3.1 spec explicitly waived the baseline-derived release gate. These results are therefore observations, not a pass/fail decision, and do not change that waiver. This run is candidate-only: it makes no v0.2.0 compatibility or before/after claim. It does not stratify by history depth, exercise dirty/stale worktree state, or separately test exact-only and semantic-only modes. See `docs/issue-83-benchmark.md` for the task-set provenance and scoring limitations and `docs/repodb-0.3.2-pre-spec.md` for the separate requirement to review comparable conditions and numeric criteria before any 0.3.2 implementation.

A no-prewarm attempt failed with `embedding worker stopped`. A separate early clone carried additional refs and indexed a newer integration tip, so its stale-index results were rejected. Neither attempt contributes measurements. The final single-ref, explicitly warmed run completed successfully; the earlier failures' cause is not asserted.

## Reproduction and raw result pointers

The runner is `benchmarks/issue-119/baseline.py` (SHA-256 `ce09506d3122a037fce482649902579f467ac689c1939cd7d4415a7935ed829b`). It calls the shared `benchmarks/issue-83/bench.py` harness (SHA-256 `e39b4db3c87ace516cfb76ef3658051dccb4660e6d3e40195bb62c246b5d3f24`) and task file (SHA-256 `7643f7ceadad52989124a9e6211dcd3e4bfdc8e64b14308cd2803575eadc1384`). The runner itself records these hashes, release/model/tool pins, corpus refs, and per-task raw results.

Materialize each corpus as a fresh Git repository with a single branch at the required commit (the first fetch source must contain the pinned object):

```sh
RUN=/tmp/issue119-v030-pinned-final.sO6fpt
mkdir -p "$RUN/corpora"

git init "$RUN/corpora/repodb"
git -C "$RUN/corpora/repodb" remote add pinned /tmp/repodb-030
git -C "$RUN/corpora/repodb" fetch --no-tags pinned 13639d6dbfed410bfa513f95861556acdf32cb5d
git -C "$RUN/corpora/repodb" checkout -b benchmark-corpus FETCH_HEAD

git init "$RUN/corpora/protonsync_ts"
git -C "$RUN/corpora/protonsync_ts" remote add pinned /home/emfrom/src/projects/protonsync-ts
git -C "$RUN/corpora/protonsync_ts" fetch --no-tags pinned 7c77aab91aff586591a00092a19760d5dc15e90a
git -C "$RUN/corpora/protonsync_ts" checkout -b benchmark-corpus FETCH_HEAD
```

Run from the repository checkout containing the pinned runner and task assets:

```sh
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 RUST_BACKTRACE=1 \
python3 benchmarks/issue-119/baseline.py \
  --source /tmp/repodb-030 \
  --binary /tmp/repodb-120-target/release/repodb \
  --corpus-root /tmp/issue119-v030-pinned-final.sO6fpt/corpora \
  --embedder /home/emfrom/bin/generate_embeddings \
  --semgrep-core /home/emfrom/.local/share/uv/tools/semgrep/lib/python3.13/site-packages/semgrep/bin/semgrep-core \
  --output /tmp/issue119-v030-pinned-final.sO6fpt/raw-results.json \
  --repeat 3 --limit 5 --timeout 3600
```

The completed raw JSON is `/tmp/issue119-v030-pinned-final.sO6fpt/raw-results.json` (321,765 bytes; SHA-256 `0db12a9611143997f3e95691d0934135ba72d6c99f920a54466f86c4f15f0671`). Run log, exit status, and timestamps are in the same directory (`run.log`, `run.status`, `run-meta.txt`). The JSON contains retrieved source snippets and is intentionally kept outside the repository; the committed report contains only aggregates and provenance.
