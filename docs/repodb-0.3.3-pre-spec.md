# RepoDB 0.3.3 — embedding protocol and database boundary pre-spec

Status: **early proposal, not an approved architecture or implementation plan**. This document captures items deferred from the [0.3.2 pre-spec](repodb-0.3.2-pre-spec.md) while 0.3.2 focuses on measuring and, if warranted, improving Semgrep throughput. A Rust model-execution port is also deferred from 0.3.2 to 0.3.3 as a candidate, not an approved runtime or implementation.

## Embedding protocol ownership

The previous 0.3.2 pre-spec said the `generate_embeddings -0` CLI would wait until EOF to flush a partial batch. That describes an older version, not the generator currently on disk: its NUL reader now yields available complete texts and flushes output vectors without requiring EOF. The updated generator behavior is committed at `1a2f677af874180fa663426ba7d8ec4c1c15f80d`; pin and verify the executable and model artifacts before relying on it as benchmark behavior. RepoDB still creates a CLI process per native-history embedding batch and invokes the embedder per imported unit. The updated behavior removes the asserted deadlock premise, but does not settle whether a different protocol would improve full-run throughput.

Investigate whether RepoDB should own a versioned two-way embedding protocol. Compare the current CLI/daemon path, a framed persistent stream, and—if justified by measurements—a memory-mapped bidirectional ring buffer or another shared-memory transport. Fewer syscalls alone do not establish a meaningful end-to-end gain. Evaluate startup and steady-state latency, throughput, memory and copy costs, portability, concurrency, backpressure, timeouts, crash recovery, model identity, and failure isolation. Specify ownership, lifecycle, compatibility and upgrade behavior only after comparing options; do not select shared memory in advance. Preserve the supported configurable external-command path unless a separate migration decision changes that contract. Do not silently run a background service or download model artifacts.

## Rust model execution

Evaluate a Rust port of **model execution**, not merely a Rust client for the current Python daemon. Compare ownership inside RepoDB (in-process, workspace crate, or bundled binary) against a separately maintained component. Decide runtime, model artifacts, tokenizer/pooling/normalization, target platforms, CPU/GPU support, offline lookup, packaging and license/redistribution feasibility only after primary-source review. Do not add a dependency, bundle weights or change the shipped default merely to explore this option.

Pin comparison vectors and semantic top-k tasks against the existing executable; assess numerical and ranking drift rather than assuming bitwise equivalence. Require finite nonempty 384-dimensional outputs for normal, whitespace and Unicode inputs, reject partial batches and incompatible model identities, and make any re-embedding explicit and reversible. Preserve the configurable external-command compatibility and privacy boundary unless separately reviewed. Compare full-run refresh and search behavior, peak memory, startup and failure recovery before adopting a port. A 0.3.3 investigation does not imply that the port must ship.

## Database input/output boundary

Investigate whether source discovery, parsing, embedding, migration and writes should remain alongside reads, ranking and presentation in one executable, move to internal modules/subcommands, or be divided into separate binaries sharing a database contract. A binary split is **not** predetermined. Compare change locality and deployment costs against schema ownership, migration and write-lock coordination, readers during refresh or partial failure, shared types and schema compatibility, model loading, installation footprint and CLI backward compatibility. Preserve SQLite atomic publication and existing search behavior. Do not infer a benefit from Semgrep or embedding microbenchmarks alone.

## Imported-source search efficiency

The imported-document search path currently scans imported units, calculates semantic distance through a separate SQLite call for each candidate, and lowercases the query inside the exact-match loop. These are plausible optimization opportunities, **not** a measured bottleneck. Profiling during 0.3.2 is optional and is not a release gate; no imported-source search rewrite is in the 0.3.2 implementation scope. For 0.3.3, compare pinned small and large imported corpora before deciding whether to change the query. If a rewrite is warranted, verify exact, embedding and combined search behavior, scope filtering, stable ties, legacy schemas, source references, case-insensitive matching, query time and memory before adoption. No optimization is committed by this pre-spec.

## Refresh change locality

Imported-source and native-history refresh are large state machines, but their source handling, staging and atomic publication are cohesive workflows. Consider extraction only if a concrete change reveals an independently changing responsibility and a testable seam. Preserve rollback, byte budgets, deterministic indexed-unit associations and SQLite's single-writer publication. Do not refactor merely to meet soft complexity thresholds; the existing [pinned complexity assessment](issue-121-complexity-assessment.md) is historical and should be checked against the implementation revision before using its metrics.

## Evidence and decisions still needed

- Pin the generator revision and test its live partial-batch behavior, output ordering and counts, failures, resource limits and interaction with RepoDB before making protocol claims.
- Use comparable, reproducible full-run measurements to decide whether embedding transport or process overhead warrants a redesign. Record search quality and reference accuracy as well as timing and resources.
- Evaluate the protocol and binary boundaries separately; record actual alternatives and tradeoffs before choosing an architecture or creating implementation tickets.
- Decide whether a Rust model port is worth adopting on evidence; deferring it to 0.3.3 does not select a runtime, owner or distribution model.
