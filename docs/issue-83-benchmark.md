# Issue #83: agent-task benchmark readiness

Status: **blocked; issue #83 remains open**. This note records an asset audit from the `integration/repodb-spec` checkout and the reproducible evidence that was safe to collect. It is not a quality or performance benchmark.

## Available assets checked

- RepoDB checkout: `41bb2b4872116929283f983474b9158c90d31470` (package version `0.2.3`). The repository has the historical `v0.2.0` tag (`7d183627c82f053717e9198b712764f6f6f1a95f`) used by the Issue #32 benchmark, but no `v0.3.0` tag or release candidate is available on this branch. The Issue #32 script and measurements compare a narrow local search workload using a deterministic two-dimensional stub. Its own documentation explicitly excludes real-model quality, cold-cache, and scale claims.
- Several other local Git checkouts exist under `/home/emfrom/src/projects` (including `protonsync-rs`, `protonsync-sdk`, `protonsync-ts`, `schemin`, `medium-publisher`, and `automation.trial`). Their presence is not a benchmark pin: no Issue #83 manifest selects repositories/revisions, records corpus rights/provenance, or supplies verified questions and source references. No such question/reference set was found in the repository or local benchmark assets.
- A real embedder is present at `/home/emfrom/bin/generate_embeddings`, from local checkout `generate_embeddings` revision `938d9ed73402249e5ac407afed6e2159bc73dc9e`. It documents `sentence-transformers/all-MiniLM-L6-v2`; its cached Hugging Face snapshot is pinned locally at `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`. A direct smoke invocation returned a finite, nonzero, 384-dimensional vector for the fixed text below. This proves only that this local model path can embed one string; it does not establish repeatable benchmark setup, speed, or retrieval quality.

Recheck the model smoke without permitting a model download:

```sh
HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
  /home/emfrom/bin/generate_embeddings --json -- \
  'Where does RepoDB store embeddings?' |
python3 -c 'import json,math,sys; v=json.load(sys.stdin); assert len(v)==384 and all(math.isfinite(x) for x in v) and any(v); print("all-MiniLM-L6-v2 smoke: 384 finite, nonzero dimensions")'
```

The embedding daemon should already be running with the cached model; if not, the offline mode makes missing model/dependencies fail instead of fetching them. Do not substitute the Issue #32 stub for this check.

## Missing prerequisites for the required evidence

1. **A reviewed repository pin set:** at least two repositories selected for this evaluation, immutable commit IDs (and any imported-source pins), source/license/provenance notes, and a repeatable way to materialize each exact corpus. The local checkout inventory alone is not an approved corpus.
2. **Verified task data:** question/answer expectations and manually validated source-reference targets at those pins, covering source location, edit navigation, rationale, deletion/rename/regression, documentation, and dependency questions. No verified task set is currently available; inventing one or treating code literals as ground truth would manufacture evidence.
3. **Comparable software builds:** a pinned baseline and a build implementing the specified 0.3.0 behavior. The available checked-out package is `0.2.3`; the required release candidate is absent. `v0.2.0` is usable only as the historical Issue #32 baseline, not as the missing benchmark corpus or candidate.
4. **A defined fixed response budget and evaluation protocol:** scoring rules for recall@k, answer and source-reference correctness, coverage false negatives and path diversity, plus fixed per-task response/token budgets and controlled repetitions. The 0.3.0 spec requires these comparisons but does not provide this task data or a scored protocol.
5. **Comparable resource accounting and conditions:** a pinned machine/runtime description; cold/warm cache procedure; and collection of end-to-end latency, peak RSS, embedding invocation counts, refresh time, and bytes split by origin/model. No Issue #83 run or results exist. A vector smoke test is not a benchmark of any of these.

## Disposition

Do not publish numeric release thresholds or claim baseline/candidate improvements from the available assets. The real embedding model is locally usable, so acquiring another model is not a blocker. The missing multi-repository pins, verified task/reference set, 0.3.0 candidate, and agreed measurement protocol prevent a reproducible benchmark that meets Issue #83. Keep the ticket open until these inputs are reviewed and the actual pinned runs are collected. The historical Issue #32 table remains a separate, explicitly stub-vector storage/query comparison.
