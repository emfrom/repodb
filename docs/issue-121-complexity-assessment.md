# Issue #121 — large-operation complexity and change locality

## Scope and baseline

The original lexical screen was prepared against integration commits `c0bfcde8e5249c87269b45448367edc4dbeba994` and `27c3f80fd403dec642691937773df62aca31d88d`; it is retained below as historical context. The actual analyzer run is pinned to integration commit `12b00d2ffbc2697158e8477808d211de523f252c`, with source-file hashes in its retained artifact. The issue #90 review's nine largest-operation candidates are measured at function-body scope. Criteria are the soft gates in `docs/CODING_STANDARDS_FULL.md`: ≤60 logical lines, cyclomatic ≤10, cognitive ≤15, direct fan-out ≤5, parameters ≤5, nesting ≤3, and call depth ≤3. These are review triggers, not automatic refactoring targets.

## Initial lexical method (historical only)

- Logical lines: count nonblank, noncomment source lines from the function signature through its closing brace; omit lines consisting only of `{` or `}`. Embedded SQL/string contents count as one physical source line when written on one line. This is a simple, reproducible source-line convention, not a claim about executable statements.
- Branch/complexity screen: a lexical scan counts `if`/`for`/`while`/`loop`, `match` occurrences, `=>` arms, and `&&`/`||` tokens as decision points; the displayed `CC*` is `1 + those counts`. This intentionally conservative proxy overcounts match patterns/closures and is not a canonical Rust cyclomatic or cognitive analyzer. It identifies review hotspots, not a passed threshold. Nesting is a lexical control-block estimate; nested expressions and formatting can affect it.
- Fan-out: count distinct explicit call identifiers that also have a function declaration in `src`; method calls are excluded when syntactically dotted. Qualified calls and Rust name resolution, trait dispatch, macros, and callbacks make this an approximate internal-call set. It is not an external-library dependency count.
- Call paths below are source-traced paths, not a whole-program call-graph proof. Indirect calls/callbacks and external library internals are excluded.

The source measurements were made with a one-off Python lexical brace scan over the checked-out Rust files; no complexity package or dependency was installed. Function line ranges below are the declaration start lines at the measured commit.

## Initial lexical screening (historical; superseded by the pinned analyzer results below)

| Operation (file:line) | Logical lines | CC* | internal call candidates | max control nesting* | Parameters |
|---|---:|---:|---:|---:|---:|
| `execute` (`main.rs:1876`) | 962 | 299 | 55 | 6 | 0 |
| `refresh_with_size_limit` (`imports.rs:644`) | 540 | 84 | 18 | 6 | 2 |
| `refresh_scoped` (`history.rs:3202`) | 274 | 31 | 8 | 3 | 3 |
| `prepare_refresh_excluding` (`history.rs:2905`) | 237 | 19 | 10 | 3 | 2 |
| `stage_diff_pair` (`history.rs:485`) | 249 | 38 | 6 | 4 | 5 |
| `search_impl` (`history.rs:4040`) | 155 | 29 | 5 | 3 | 7 |
| `search_exact` (`worktree.rs:166`) | 179 | 42 | 15 | 3 | 3 |
| `map_semantic_ranges` (`worktree.rs:398`) | 161 | 34 | 13 | 2 | 3 |
| `rank_evidence` (`ranking.rs:122`) | 156 | 38 | 5 | 4 | 5 |

The preceding table is retained only to preserve the original screening record. Its logical-line counts, `CC*`, internal-call candidates, and nesting are lexical estimates and are not the actual measurements used for the disposition.

## Actual pinned measurements

Measured against integration commit `12b00d2ffbc2697158e8477808d211de523f252c` (the source commit recorded by the artifact) on 2026-10-21. The measurement script hash and source hashes for the five analyzed Rust files are embedded in [`issue-121-measurement-artifact.json`](issue-121-measurement-artifact.json), along with each derived result. The full per-file `rust-code-analysis-cli` JSON output is retained in the companion compressed [`issue-121-measurement-artifact-raw-metrics.json.gz`](issue-121-measurement-artifact-raw-metrics.json.gz). Toolchain: `rust-code-analysis-cli 0.0.25`, `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0`, Clippy `0.1.99 (b940084d7e 2026-09-28)`.

| Operation (file:line) | Rust LLOC | Rust cyclomatic | Clippy cognitive | AST call sites | Distinct syntax callees | Max control nesting |
|---|---:|---:|---:|---:|---:|---:|
| `execute` (`main.rs:1876`) | 403 | 352 | 98 | 498 | 133 | 7 |
| `refresh_with_size_limit` (`imports.rs:644`) | 101 | 88 | 24 | 118 | 68 | 5 |
| `refresh_scoped` (`history.rs:3243`) | 70 | 59 | 8 | 72 | 36 | 3 |
| `prepare_refresh_excluding` (`history.rs:2951`) | 46 | 41 | 7 | 55 | 33 | 3 |
| `stage_diff_pair` (`history.rs:513`) | 76 | 46 | 14 | 74 | 35 | 4 |
| `search_impl` (`history.rs:4081`) | 36 | 23 | 12 | 24 | 22 | 3 |
| `search_exact` (`worktree.rs:166`) | 94 | 41 | 14 | 74 | 44 | 3 |
| `map_semantic_ranges` (`worktree.rs:398`) | 122 | 32 | 6 | 72 | 32 | 2 |
| `rank_evidence` (`ranking.rs:122`) | 57 | 29 | 20 | 78 | 33 | 4 |

The soft gates in `docs/CODING_STANDARDS_FULL.md` are LLOC ≤60, cyclomatic ≤10, cognitive ≤15, distinct direct callees ≤5, and nesting ≤3. The table reports tool-defined measurements, not hard release gates. Rust LLOC counts `Statement`, `EmptyStatement`, `ExpressionStatement`, `LetDeclaration`, `AssignmentExpression`, and `CompoundAssignmentExpr` AST nodes; signatures and standalone block braces do not count. The AST callee count is syntax-normalized: method calls use the final field name, scoped identifiers retain their path, and repeated names count once. It is not a type- or trait-resolved call graph; macros are excluded. Nesting counts AST ancestors among `if`, `match`, `for`, `while`, and `loop` expressions. Rust LLOC and cyclomatic values are from `rust-code-analysis-cli` 0.0.25; nested closure/function aggregates are subtracted to report the named function's own value. Cognitive values are Clippy's `cognitive_complexity` lint scores; the analyzer was configured with threshold 0 only to emit scores for all positive functions. Tool-specific score definitions are not interchangeable.

Prerequisite: `rust-code-analysis-cli 0.0.25` in `PATH` (install with `cargo install rust-code-analysis-cli --version 0.0.25 --locked`).

Reproduce from the pinned source checkout with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 benchmarks/issue-121/measure.py --root . --output docs/issue-121-measurement-artifact.json --clippy-log /tmp/issue121-clippy-full.log --target-dir /tmp/issue121-clippy-target
```

The run exited 0. Raw Clippy output is retained at `/tmp/issue121-clippy-full.log` (SHA-256 recorded in the artifact); the script writes the exact tool output, source hashes, method definitions, and per-operation measurements to the tracked JSON artifact. No production code was changed by measurement.

## Cohesion, dependency paths, and state ownership

- **CLI (`execute`)** owns argument parsing, command dispatch, database selection/opening, user-facing output and orchestration for status, refresh/init, imports, settings, re-embedding and search. It is a single dispatcher, but contains substantial command-specific flows: `search` alone includes option parsing, history/worktree selection, multiple backends, ranking, serialization and text output. It therefore changes command behavior in one large shared function and has high change-locality cost; command-specific behavior changes could interact with parsing/output branches. Database/data work is delegated to `history`, `imports`, `worktree`, `ranking`, and `pagination` modules.
- **Import refresh** owns refresh of registered document/URL/Git sources: stage source/version/unit data, obtain and transform source content, embed, prune staged versions, and prepare the publication result. Its size reflects the multi-source refresh state machine rather than an accidental wrapper chain, but the staging, source-kind, and content-conversion flows coexist in one function with substantial branch count. SQLite TEMP staging is local to the connection; durable publication is a caller-controlled transaction. No state-ownership defect is asserted by this structural review.
- **Native history refresh** owns discovery/staging of commits, blobs, diffs, segmented units and embeddings in `history.rs`; `refresh_scoped` enforces the scoped-refresh preconditions and stages then publishes through the supplied connection/transaction boundary. `prepare_refresh_excluding` and `stage_diff_pair` are detailed history/diff transformations rather than unrelated subsystems. Database mutation stays behind the passed SQLite connection; process output and byte budgets are explicit local inputs.
- **History search** (`search_impl`) owns SQL candidate retrieval, mode-dependent vector setup, hydration and occurrence selection. Public `search` and `search_all_occurrences` are intentional backend variants that share this implementation. It reads through a borrowed connection and does not own persistence.
- **Worktree exact/semantic operations** own best-effort observation of Git HEAD/index/live files, safe path checks, mapping/coverage and result construction. `search_exact` and `map_semantic_ranges` share path/text/Git helpers but have distinct exact-search versus semantic-range responsibilities. The public API documents they do not write the index or persist live text; semantic mapping uses already-indexed embeddings.
- **Ranking** owns filtering, overlap grouping, related-diff lifting and deterministic ordering over passed candidates. It is pure with respect to the database and filesystem; small private helpers carry identity/group/order rules.

Representative dependency paths:

1. CLI search: `main::execute` (`src/main.rs:1876`) selects indexed-content retrieval through `history::search` / `history::search_all_occurrences` when current, or local exact/semantic helpers when not (`src/main.rs:2463-2500`). Optional live evidence uses `worktree::search_exact` (`src/main.rs:2403`) or semantic range mapping (`src/main.rs:2608`). `search_evidence` (`src/main.rs:221`, called at 2803) converts returned records; `ranking::rank_evidence` (`src/ranking.rs:122`, called at `src/main.rs:2880`) owns grouping/order before pagination and rendering. The history backend owns retrieval, the adapter owns payload identity, and `ranking` owns aggregation/order.
2. Native refresh: `main::execute` (`src/main.rs:2046`) → `history::prepare_refresh_excluding` (`src/history.rs:2951`) → reachable-commit/tree/diff staging → `stage_diff_pair` (`src/history.rs:513`, called at 3139/3142) / segmentation / embedding-batch flush → publication transaction. This path shares the refresh snapshot and staging state; diff path transforms line/file data before it is staged.
3. Scoped refresh: `main::execute` (`src/main.rs:2036/2038`) → `refresh_changed_excluding` or `refresh_update_excluding` (`src/history.rs:3228/3235`) → `refresh_scoped` (`src/history.rs:3243`) → Git traversal and batch embedding → publish staged refresh data.
4. Imported-source refresh: `main::execute` (`src/main.rs:2030/2045`) → `imports::refresh_with_size_limit` (`src/imports.rs:644`) → source-specific fetch/clone, conversion/segmentation and embedding → staged-version pruning → caller-owned publication transaction (`src/main.rs:2032-2034` or 2048-2051).
5. Live exact search: `main::execute` (`src/main.rs:2403`) → `worktree::search_exact` (`src/worktree.rs:166`) → validate indexed HEAD → Git path discovery → safe live-path/read/filter helpers → matches and coverage. Semantic mapping is `main::execute` (`src/main.rs:2608`) → `worktree::map_semantic_ranges` (`src/worktree.rs:398`) → indexed HEAD/staged/live layer loads → hunk/range mapping; it does not embed or publish.

The core subsystem boundaries are cohesive and own their respective state; no cross-module dependency cycle or duplicate state owner was identified in this scoped trace. The clearest locality concern is the CLI dispatcher: CLI parsing/formatting for multiple commands and backend orchestration share one large operation, with actual tool scores recorded above. `refresh_with_size_limit` is also above the measured size/complexity triggers, but its current control flow is a single refresh state machine with source-kind branches and shared staging. Splitting either function solely to meet soft thresholds would create a risky migration of staging state and publication invariants without evidence that those steps are independently changing or owned elsewhere. The same applies to history and worktree operations: their responsibilities correspond to cohesive subsystems and explicit data/state boundaries.

## Disposition

No production refactor was made. This assessment demonstrates significant structural threshold exceptions and a plausible CLI-dispatcher locality cost, but the ticket asks for refactoring only when evidence shows a concrete locality problem; it does not identify a repeated defect/change pattern or an independently owned command boundary whose extraction can be safely verified from this ticket alone. The principal operations are cohesive around command dispatch, a refresh's staged state machine, history indexing/search, live-worktree inspection, and pure result ranking. No behavior changed, so no behavior tests were added solely for a metric review. Existing CLI/backend tests remain the behavior baseline if a later, narrower extraction is justified by specific change friction.

## Commands and validation

- `git worktree add -b issue-121-assess /tmp/repodb-issue-121 HEAD` — created isolated worktree at the integration tip; source changes were not made in the shared checkout.
- `repodb search 'refresh_with_size_limit' --exact --worktree --limit 5` — successful RepoDB exact search corroborated the prior review finding and source location.
- `cargo test --no-run` — passed; all library, binary, and integration test targets compiled.
- `CARGO_TARGET_DIR=/tmp/repodb-issue-121-target cargo test --all-targets -- --test-threads=1` — passed from an isolated clean build directory after the integration-tip merge: library 5, binary 52, agent skills 5, CLI 174, pagination 6, ranking 7, status CLI 3, worktree 10 passed and 1 ignored (262 passed, 1 ignored). No behavior tests were added by this documentation-only assessment.
- An earlier all-target run using the shared Cargo target directory produced one failure in a test name not present in this checkout; the clean-target run above enumerated the checkout's 173 CLI tests and passed. The shared target appeared to contain a stale test executable, so that first result is not treated as evidence against this commit.
- `git merge-base --is-ancestor integration/repodb-spec HEAD` and `git rev-parse HEAD integration/repodb-spec` — passed after merging; both resolve to `27c3f80fd403dec642691937773df62aca31d88d`, so the isolated branch includes the current integration tip.
- `git diff --check` — passed.

Residual limitations: the old table is lexical screening only. The new values are reproducible, tool-defined AST/Clippy metrics; syntax-level callee names are not type/trait-resolved, macros are excluded, and the source-traced paths are not a whole-program call graph. External library internals, runtime behavior, and a full duplication/cycle analysis are not covered. No runtime defect is asserted.
