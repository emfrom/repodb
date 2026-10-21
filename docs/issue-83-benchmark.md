# Issue #83: agent-task benchmark readiness

Status: **blocked; issue #83 remains open**. The current run confirmed the RepoDB candidate and historical baseline pins, materialized-content provenance instructions for the requested ProtonSync pin, and a real offline embedder smoke. It did not produce benchmark results: no approved source-verified question/reference set or second repository pin is available, and the worktree has no initialized benchmark indexes. The task truth cannot be invented.

Run-specific pin evidence, commands and limitations are recorded in [`benchmarks/issue-83/asset-audit.md`](../benchmarks/issue-83/asset-audit.md). The ProtonSync corpus is `/home/emfrom/src/projects/protonsync-ts` at `7c77aab91aff586591a00092a19760d5dc15e90a`; materialize it using `git archive` at that commit rather than copying untracked checkout files. Candidate RepoDB is `49ac71a5fd2328e93110a5369e578e4eff7a25d2`; historical baseline is `v0.2.0` at `7d183627c82f053717e9198b712764f6f6f1a95f`. The embedder smoke is not a retrieval-quality or resource benchmark.

No recall, answer/reference accuracy, coverage, latency, RSS, embedding-call/byte, refresh or storage measurements are claimed. Required stratified baseline/candidate comparisons and cold/warm runs remain outstanding. Keep #83 open and do not close #84 until reviewed task truth and reproducible measured evidence exist.
