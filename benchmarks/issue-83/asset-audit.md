# Issue #83 run asset audit

Status: **blocked; no benchmark scores are reported and issue #83 remains open**. This audit records this run's pins and checks. It is not a substitute for the required task suite or baseline/candidate measurements.

## Pins and materialization

- RepoDB managed worktree: `integration/repodb-spec` base/HEAD `49ac71a5fd2328e93110a5369e578e4eff7a25d2` before edits (verified with `git rev-parse HEAD`). This is the candidate code pin.
- Historical baseline: `v0.2.0`, commit `7d183627c82f053717e9198b712764f6f6f1a95f`; the object and tag resolve in this worktree.
- ProtonSync source checkout: `/home/emfrom/src/projects/protonsync-ts`, commit `7c77aab91aff586591a00092a19760d5dc15e90a`. Its checked-out HEAD is exactly the requested pin. The checkout has untracked files; do not benchmark its live tree. Materialize only pinned tracked files with:

  ```sh
  git -C /home/emfrom/src/projects/protonsync-ts archive \
    --format=tar 7c77aab91aff586591a00092a19760d5dc15e90a \
    | tar -xf - -C /path/to/isolated-corpus
  ```

  The commit resolves to a commit object. The archive listing was inspected; the working untracked files are absent from `git archive` by construction. This is one source repository, not the required multi-repository corpus.
- The requested fixture `rust-fixture-3690a8f3cf8c287f252100bbbe55a410` was not found and was not used.
- Embedder: `/home/emfrom/bin/generate_embeddings`, source revision `938d9ed73402249e5ac407afed6e2159bc73dc9e`, model `sentence-transformers/all-MiniLM-L6-v2`. Offline smoke invocation returned 384 finite, nonzero dimensions. This validates an embedder invocation only, not indexing, retrieval or benchmark quality.

## Checks run

```sh
git rev-parse HEAD
git rev-parse 49ac71a5fd2328e93110a5369e578e4eff7a25d2^{commit}
git rev-parse 7d183627c82f053717e9198b712764f6f6f1a95f^{commit}
git -C /home/emfrom/src/projects/protonsync-ts rev-parse HEAD
git -C /home/emfrom/src/projects/protonsync-ts cat-file -t 7c77aab91aff586591a00092a19760d5dc15e90a
git -C /home/emfrom/src/projects/protonsync-ts archive --format=tar 7c77aab91aff586591a00092a19760d5dc15e90a | tar -tf -
git -C /home/emfrom/src/projects/generate_embeddings rev-parse HEAD
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 /home/emfrom/bin/generate_embeddings --json -- 'Where does RepoDB store embeddings?' | python3 -c 'import json,math,sys; v=json.load(sys.stdin); assert len(v)==384 and all(math.isfinite(x) for x in v) and any(v); print("all-MiniLM-L6-v2 smoke: 384 finite, nonzero dimensions")'
repodb search 'issue 83 benchmark task'
```

The RepoDB search did not run: it returned `repodb: no embeddings; run refresh` (exit 2). No isolated baseline/candidate index or database was built, and no refresh/query measurements were collected.

## Why the benchmark is not runnable as a valid #83 evaluation

The available source pin does not supply an approved, source-verified question/expected-answer/reference set, label protocol or task response budget. Creating labels now would violate the requirement not to invent task labels. There is only one task corpus pin, and there are no initialized isolated indexes for either build. No run can therefore establish recall@k, answer/source-reference correctness, coverage false negatives, or path diversity. Consequently there are no defensible baseline/candidate comparisons for language/history/duplicate strata, qualified cold/warm conditions, latency, peak RSS, embedding calls/bytes, refresh time or storage. Do not infer any such metric from the embedding smoke test or from the distinct Issue #32 stub-vector benchmark.

No task records, scores, thresholds or placeholder metrics are fabricated. A reviewed multi-repository pin/task set with exact source references and a fixed budget must be supplied before the required scoring harness can be meaningfully run. Keep #83 open; do not close #84.
