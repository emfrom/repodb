# Issue #121 — large-operation complexity and change locality

## Scope and baseline

Measurements are against integration commit `c0bfcde8e5249c87269b45448367edc4dbeba994` (the `integration/repodb-spec` tip when this isolated worktree was created); the current integration tip `27c3f80fd403dec642691937773df62aca31d88d` was subsequently merged into this isolated branch before reporting. Intervening integration changes stage Git pins as part of refresh publication, add focused tests, and correct recorded strict-Clippy findings; the operation measurement remains explicitly pinned to the earlier baseline rather than implying exact post-merge LOC. The issue #90 review's nine largest-operation candidates were measured at function-body level rather than by approximate next-function spans. Criteria are the soft gates in `docs/CODING_STANDARDS_FULL.md`: ≤60 logical lines, cyclomatic ≤10, cognitive ≤15, direct fan-out ≤5, parameters ≤5, nesting ≤3, and call depth ≤3. These are review triggers, not automatic refactoring targets.

## Method

- Logical lines: count nonblank, noncomment source lines from the function signature through its closing brace; omit lines consisting only of `{` or `}`. Embedded SQL/string contents count as one physical source line when written on one line. This is a simple, reproducible source-line convention, not a claim about executable statements.
- Branch/complexity screen: a lexical scan counts `if`/`for`/`while`/`loop`, `match` occurrences, `=>` arms, and `&&`/`||` tokens as decision points; the displayed `CC*` is `1 + those counts`. This intentionally conservative proxy overcounts match patterns/closures and is not a canonical Rust cyclomatic or cognitive analyzer. It identifies review hotspots, not a passed threshold. Nesting is a lexical control-block estimate; nested expressions and formatting can affect it.
- Fan-out: count distinct explicit call identifiers that also have a function declaration in `src`; method calls are excluded when syntactically dotted. Qualified calls and Rust name resolution, trait dispatch, macros, and callbacks make this an approximate internal-call set. It is not an external-library dependency count.
- Call paths below are source-traced paths, not a whole-program call-graph proof. Indirect calls/callbacks and external library internals are excluded.

The source measurements were made with a one-off Python lexical brace scan over the checked-out Rust files; no complexity package or dependency was installed. Function line ranges below are the declaration start lines at the measured commit.

## Measurements

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

`CC*`, internal-call candidates, and nesting are screening estimates as defined above; they must not be reported as exact canonical complexity scores. Several trigger deviations are nevertheless not borderline: every listed function is over 60 logical lines, and the top-level dispatcher and import refresh function are especially broad. Exact cognitive complexity was not measured with an analyzer.

## Cohesion, dependency paths, and state ownership

- **CLI (`execute`)** owns argument parsing, command dispatch, database selection/opening, user-facing output and orchestration for status, refresh/init, imports, settings, re-embedding and search. It is a single dispatcher, but contains substantial command-specific flows: `search` alone includes option parsing, history/worktree selection, multiple backends, ranking, serialization and text output. It therefore changes command behavior in one large shared function and has high change-locality cost; command-specific behavior changes could interact with parsing/output branches. Database/data work is delegated to `history`, `imports`, `worktree`, `ranking`, and `pagination` modules.
- **Import refresh** owns refresh of registered document/URL/Git sources: stage source/version/unit data, obtain and transform source content, embed, prune staged versions, and prepare the publication result. Its size reflects the multi-source refresh state machine rather than an accidental wrapper chain, but the staging, source-kind, and content-conversion flows coexist in one function with substantial branch count. SQLite TEMP staging is local to the connection; durable publication is a caller-controlled transaction. No state-ownership defect is asserted by this structural review.
- **Native history refresh** owns discovery/staging of commits, blobs, diffs, segmented units and embeddings in `history.rs`; `refresh_scoped` enforces the scoped-refresh preconditions and stages then publishes through the supplied connection/transaction boundary. `prepare_refresh_excluding` and `stage_diff_pair` are detailed history/diff transformations rather than unrelated subsystems. Database mutation stays behind the passed SQLite connection; process output and byte budgets are explicit local inputs.
- **History search** (`search_impl`) owns SQL candidate retrieval, mode-dependent vector setup, hydration and occurrence selection. Public `search` and `search_all_occurrences` are intentional backend variants that share this implementation. It reads through a borrowed connection and does not own persistence.
- **Worktree exact/semantic operations** own best-effort observation of Git HEAD/index/live files, safe path checks, mapping/coverage and result construction. `search_exact` and `map_semantic_ranges` share path/text/Git helpers but have distinct exact-search versus semantic-range responsibilities. The public API documents they do not write the index or persist live text; semantic mapping uses already-indexed embeddings.
- **Ranking** owns filtering, overlap grouping, related-diff lifting and deterministic ordering over passed candidates. It is pure with respect to the database and filesystem; small private helpers carry identity/group/order rules.

Representative dependency paths:

1. CLI search: `main::execute` → `search_evidence` → history/import/worktree backend(s) → `ranking::rank_evidence` → `pagination`/JSON or human rendering. The dispatcher selects filters/scope and composes evidence; backend modules own retrieval and `ranking` owns aggregation/order.
2. Native refresh: `main::execute` → `history::prepare_refresh_excluding` → reachable-commit/tree/diff staging → `stage_diff_pair` / segmentation / embedding-batch flush → publication transaction. This path shares the refresh snapshot and staging state; diff path transforms line/file data before it is staged.
3. Scoped refresh: `main::execute` → `refresh_changed_excluding` or `refresh_update_excluding` → `refresh_scoped` → Git traversal and batch embedding → publish staged refresh data.
4. Imported-source refresh: `main::execute` → `imports::refresh_with_size_limit` → source-specific fetch/clone, conversion/segmentation and embedding → staged-version pruning → caller publication transaction.
5. Live exact search: `main::execute` → `worktree::search_exact` → validate indexed HEAD → Git path discovery → safe live-path/read/filter helpers → matches and coverage. Semantic mapping follows `map_semantic_ranges` → indexed HEAD/staged/live layer loads → hunk/range mapping; it does not embed or publish.

The core subsystem boundaries are cohesive and own their respective state; no cross-module dependency cycle or duplicate state owner was identified in this scoped trace. The clearest locality concern is specifically the CLI dispatcher, where CLI parsing/formatting for multiple commands and backend orchestration share one 962-line body. `refresh_with_size_limit` is also above every size/branch trigger, but its current control flow is a single refresh state machine with source-kind branches and shared staging. Splitting it solely to meet line counts would create a risky migration of staging state and publication invariants without evidence that those steps are independently changing or owned elsewhere. The same applies to history and worktree operations: their responsibilities correspond to cohesive subsystems and explicit data/state boundaries.

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

Residual limitations: complexity values are lexical screens, not canonical analyzer results; indirect dispatch, trait implementations, macro expansion, and external dependency internals are not covered. This is source structure evidence, not a claim of a runtime defect or a full duplication/cycle analysis.
