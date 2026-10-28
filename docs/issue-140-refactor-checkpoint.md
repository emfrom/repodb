# RepoDB 0.3.2 refactor-only CLI checkpoint

Status: verified **before any parser-parallelism changes**. This report and its raw evidence are separate from the v0.3.1 baseline and from the final-candidate release gate. It makes no performance-improvement claim.

## Candidate and procedure

- Integration source: `5010074111092f03ad01805cd4d96cb6cad63f58` (`integration/repodb-032`), package version `repodb 0.3.1` (no version change).
- Release binary: `/tmp/automate-spec-repodb-20261027/target-refactor/release/repodb`, SHA-256 `e97675833a953421a694799356beafd3ff485ea84c7ce20c87582d640a255f7b`.
- Before binary: verified v0.3.1 release rebuild from `f0a6b5d13ef3f8c5a4196aa655255a8f4f5d934d`, SHA-256 `a7fa5ee72e5ff83419918275c8eb813891a5e942997133ad99bcb2bffb395f9a`.
- Embedder: real `generate_embeddings` 1.0.0, source commit `1a2f677af874180fa663426ba7d8ec4c1c15f80d`; executable SHA `9216649f498981fe2b1a1a85a2677b0611a9a060dda75b7a585095b3bca5df1d`; daemon SHA `838e05f224f491a10fc641b9c065a411c23a23d9e43b7b3f0f160f644a201628`.
- Semgrep Core 1.181.0 (`0f6e09374a`), SHA `14f4b18720d881156bc0cc507b19130aa8d8db0882a5a479fb3919aadefca857`; model revision and weights match the accepted baseline manifest; downloads disabled.
- Host: Linux 7.0.0-38-generic, Intel i5-13450HX, 16 visible logical CPUs. Warm-cache procedure; embedder warmed with two strings before each throughput/quality runner. The host is shared; no load trace or CPU isolation. Timeouts: process 900s, outer command 1800s.
- Throughput: 3 fresh-index and full unchanged-refresh runs for each history; same pinned 231-commit RepoDB history at `f7cc6bc5451204c5696f1bbef7e5575772820f47`, and combined-26 corpus HEAD `6740e8af4d89bbb1b2b3577749fbf499d6c15e5e` (generator `docs/issue-134/generate-corpus.py`, 26 fixtures x 10 copies, 260 files/commit). Raw records retain commands, process-tree RSS, database bytes and row counts.
- Quality: frozen issue-83 task suite (3 repeats), exact/negative/source-reference controls, duplicate pagination control, and 26-language semantic marker suite. Raw outputs include full results and references.

## Results

| Workload | Fresh `init` seconds (3 runs; median) | Full unchanged `refresh` seconds (3 runs; median) | Fresh index bytes | Indexed units |
|---|---:|---:|---:|---:|
| RepoDB history | 388.81, 367.56, 362.91; **367.56** | 379.89, 357.51, 379.22; **379.22** | 107,868,160 | 46,681 |
| Combined-26 | 101.26, 99.30, 100.99; **100.99** | 99.99, 99.17, 105.04; **99.99** | 6,709,248 | 2,782 |

Sampled process-tree peak RSS (RepoDB + persistent embedder daemon) ranged 0.76–1.01 GiB on RepoDB and 0.70–0.72 GiB on combined-26. These are reports, not memory gates. Five `refresh --changed` samples on each of three indexes per corpus (15/corpus) had medians 12.13 ms RepoDB (10.77–15.46 ms) and 12.01 ms combined-26 (10.67–14.38 ms).

- Exact language markers: **52/52** correct source-reference probes.
- Combined-26 negative controls: **26/26** returned no results. Duplicate `--explore`: **30/30** results over 6 pages, unique reference IDs, stable snapshot, all 10 expected JavaScript copies, all source spans valid.
- Combined-26 semantic marker checks: **33/78** successful attempts, matching the baseline's 11/26 language targets passing all three repeats (not 78 independent observations).
- Frozen task suite: no per-task median regression in `recall_at_k`, source-reference correctness, or answer correctness in semantic or combined mode. Aggregate successful semantic/combined source-reference counts and answer counts matched the prior frozen suite: RepoDB 0 reference / 3 answer; ProtonSync 3 references / 6 answers per mode. The suite remains a sparse regression floor, not general relevance evidence.
- Query resource suite completed for both pinned corpora; raw wall times and sampled process-tree peak RSS are retained.

Throughput medians are unpaired checkpoint observations and are **not** a gate comparison. Historical baseline medians and host noise prohibit interpreting these values as a speedup or regression. The quality-run RepoDB database was 158,052,352 bytes versus 105,738,240 bytes in the older v0.3.1 frozen-task capture. That size is not a numeric gate; the full-refresh pinned-history indexes had the expected 107,868,160 bytes and 46,681 rows, and all frozen-task floors matched. The checked-out source pin and release-source binary are different revisions; this discrepancy is recorded rather than attributed to the CLI extraction.

## CLI contract and validation

- Public integration suite covers help, status, settings, import/preview, re-embedding, init/refresh modes, search modes/scopes/pagination/references, errors, JSON/text output and exit status. Invalid status/settings arguments now have an explicit regression test.
- `cargo test --all-targets`: **313 passed, 1 ignored**. An earlier attempt had one transient `stdin writer failed` in a PDF fixture; its isolated rerun passed, followed by the complete passing suite.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo fmt --check`: passed. `git diff --check`: passed.
- `src/main.rs::execute` now handles only global options, database selection, help routing, and command routing. Command parsing/orchestration/presentation is in `src/commands/{help,status,settings,import,refresh,search}.rs`; no additional executable or shared all-dependencies context was introduced.
- One exploratory invocation of the duplicate/explore control against the RepoDB-history corpus failed because that corpus does not contain the ten generated copies the test asserts. The control was correctly rerun against the pinned combined-26 corpus and passed; no source or index changes resulted.

## Raw evidence

All evidence files under [`issue-140/evidence/`](issue-140/evidence/) are the actual checkpoint outputs. The benchmark runner and quality scripts are the pinned scripts listed in `docs/issue-134/evidence/environment.json`. Verify raw output hashes against `issue-140/evidence/SHA256SUMS.txt`.

Decision: CLI-only extraction checkpoint passes behavior and reviewed quality floors. Performance is intermediate, unpaired evidence only. Proceed to native parser configuration/retry/concurrency tickets; do not claim a 0.3.2 release-gate pass.
