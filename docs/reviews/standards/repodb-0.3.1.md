# RepoDB 0.3.1 coding-standards release review

- Status: **BLOCKED — reviewed against the frozen verification target, but required independent and release-readiness evidence remains incomplete.**
- Target commit/tag: Source candidate `0c9d908b0f9c3bc9aa641663f0227c2436c20eb4`; no release tag.
- Commit range: `592a0422917f249df800de751dd8e5bad14a430b..0c9d908b0f9c3bc9aa641663f0227c2436c20eb4`.
- Review date: 2026-10-25.
- Supported platforms: Linux x86_64 only, per scope decision #123; host `Sancho`, kernel `7.0.0-38-generic`, Cargo 1.99.0.
- Review owner: emfrom.
- Release owner: emfrom.

## Scope and evidence index

This review targets the exact source commit above. The source freeze is for verification only, not release approval; code changes reset the freeze. Normative sources `docs/CODING_STANDARDS_FULL.md` and `docs/reviews/STANDARDS.md` were read in full. Changed areas include Rust import/cache/URL/process/storage/history/CLI code, integration and unit tests, Cargo version/package metadata, user documentation, and release-review/benchmark records. The historical `v0.3.0` security/standards reports remain separate.

Evidence is recorded in `docs/reviews/security/repodb-0.3.1.md`. At `0c9d908`, formatting, strict Clippy, and the serial all-target/all-feature test suite passed: **317 passed, 1 ignored**. These checks were repeated from `/tmp/repodb-clean-final-0c9d908.pmxDSk/source`, a `git archive` export whose 130 paths exactly matched the commit tree; the clean-export release build reproduced SHA-256 `375d4c32cb3b6d2ce21267c5e818644e0aec7cbe036c387f1c5b6174c41b93b7`. Local package verification passed (102 files from the exact Git checkout); this is not a published or signed artifact. Semgrep 1.181.0 `p/rust` reports 13 INFO findings / 0 scan errors; `p/secrets` reports 0 findings on the source diff and packaged files.

The user explicitly prohibited local agents/subagents. Accordingly, self-review is not represented as an independent comprehension or release audit. Verification tasks #127, #128, #130, and #131 are closed with the limits recorded; this standards review is complete with a BLOCKED verdict. The release owner later directed GO WITH EXCEPTIONS for #132 on 2026-10-26; that decision is recorded in the security review and does not convert this standards review into a PASS or independent review.

## Sub-review 1 — Precedence and fundamental principles
- Status: **PASS**
- Evidence: The candidate prioritizes integrity and safe failure: refresh/import publication stays transactional, invalid status schemas fail rather than appear empty, credential-bearing imports are rejected, and sensitive PDF cache files receive private Unix permissions. User-visible credential behavior is documented. No intentional public-format migration was found in the reviewed candidate.
- Findings / rationale: Existing behavior is preserved except for explicit security rejection/redaction behavior. The change to reject credential-bearing URL/Git locators is documented and regression-tested. No MUST exception or style-over-integrity optimization was identified.

## Sub-review 2 — Structure and complexity
- Status: **PASS (documented soft deviation)**
- Evidence: `cargo clippy --locked --all-targets --all-features -- -D warnings` passes. Manual line comparison found `refresh_with_size_limit_and_allowlist` in `src/imports.rs` is approximately 805 source lines at `0c9d908`, versus approximately 803 at baseline `592a042`; it predates this candidate's net changes and owns the staged import/refresh transaction orchestration. New cache/URL/resource behavior remains in cohesive existing modules. No dedicated cyclomatic/cognitive/fan-out/indirect-call analyzer was run.
- Findings / rationale: The function substantially exceeds the 60-line soft threshold; this is an explicit maintainability deviation, not a hard-gate failure or numerical pass. It was not split mechanically during security remediation because the stateful staging/publication path is high risk and refactoring would enlarge the candidate diff. Track a deliberate extraction with characterization tests in later work; #131 records the maintainability concern. Other complexity metrics remain unmeasured.

## Sub-review 3 — Interfaces and contracts
- Status: **PASS**
- Evidence: `help_documents_shipped_commands_options_and_no_crawl_behavior_without_initializing`, `version_flag_prints_package_version_without_database_side_effects`, URL/Git credential rejection tests, legacy metadata redaction test, JSON/status/pagination tests, and the full CLI suite passed at the exact candidate. README records import credential policy and trusted executable/data-flow limitations. Errors do not print the rejected locator or Git stderr in the new regression cases.
- Findings / rationale: The version is 0.3.1. A pre-existing low-severity diagnostic inconsistency remains: an unknown command outside a Git repository can report Git discovery failure rather than the inside-repository `unknown command` message. No stable JSON/exit-status regression was observed. README now documents Cargo installation from an exact local source checkout; install, upgrade, rollback, and uninstall were exercised against v0.3.0 and final v0.3.1 source in an isolated Linux x86_64 root. Published registry/tag distribution is not verified.

## Sub-review 4 — State, data flow, and persistence
- Status: **PASS**
- Evidence: Existing transactional publication and `PRAGMA data_version` checks preserve the last usable snapshot. Named tests cover import/publication SQL failure, URL metadata rollback, migration rollback/retry, failed Git pin publication, post-commit reporting failure, concurrent model changes, interruption, and corrupted database rejection without replacement. New credential checks run before registration/network use; legacy credential metadata is redacted from search output and refresh refuses to reuse credential-bearing legacy sources. PDF cache readers discard non-private cache files before deserialization.
- Findings / rationale: Durable database/cache paths still follow their selected ownership/permissions/umask and README warns operators to protect shared-host data. No partial-publication defect was observed in the exact suite. The #127 verification deliverable is closed with those limits documented; arbitrary crash-at-every-write-boundary and general repair remain unproven and were accepted only under the time-bounded #132 exception, for gate closure rather than as a pass.

## Sub-review 5 — Security and resource safety
- Status: **PASS**
- Evidence: Resource ceilings, deadlines, output bounds, cache/database locks, URL egress validation, Git argument arrays, safe failure, and cancellation are described in the security review. Semgrep `p/rust` and `p/secrets` results are recorded there. Manual inspection covered production unsafe sites, temporary-file creation, command execution, credential-bearing URL rejection, Git stderr suppression, legacy result redaction, and the `sqlite-vec` 0.1.9 callback/header signature. The Linux runtime FFI inventory is direct `libc`, `rusqlite` → `libsqlite3-sys` 0.30.1 (bundled SQLite), and `sqlite-vec` 0.1.9; the full dependency tree is recorded at `/tmp/repodb-dependency-tree-0c9d908.txt`. `cargo-geiger` is unavailable, so a complete transitive unsafe-line audit is not claimed. A PDF-cache permission defect was caught and fixed at `0a64388`: default creation mode produced 0664 under umask 0002; new cache files are now created with mode 0600 on Unix and legacy group/world-accessible entries are discarded. Tests verify both conditions.
- Findings / rationale: The fixed PDF-cache issue and focused runtime FFI inventory are recorded in the security review. The #128 focused audit/scan criteria are complete, but no line-by-line audit of every transitive/upstream C implementation is claimed; `cargo-geiger` and standalone secret scanners were unavailable. GitHub secret scanning remains disabled. These are explicit limitations, not findings silently passed or exceptions.

## Sub-review 6 — Behavioural verification
- Status: **PASS**
- Evidence: At `0c9d908`, serial `CARGO_NET_OFFLINE=true cargo test --locked --all-targets --all-features -- --test-threads=1`: 317 passed, 1 ignored. The clean `git archive` export produced the same result. Coverage includes the generated 24-transition history/tree consistency and refresh-idempotence test, seven SQL publication-boundary failures, URL/Git credential handling, source/range invariants, malformed Git/PDF input, SQLite corruption, atomic rollback, timeouts/output floods, cancellation, concurrency, and private PDF-cache modes. Exhaustive deterministic testing checks UTF-8 range offset pairs; workload measurements and variance are in the security report.
- Findings / rationale: Deterministic generated history coverage and seven injected SQL publication-boundary failures are present, but this is not randomized property testing or a `SIGKILL`-at-every-write-boundary simulation. Mutation, coverage, and fuzz tools were not run; no fuzz/property dependency was installed. RepoDB does not implement automatic repair for corrupted databases; it fails closed without overwriting them. These limits are documented under #127, and critical recovery-risk disposition remains for the release owner under #132.

## Sub-review 7 — Architecture
- Status: **PASS**
- Evidence: Changes remain within cohesive existing modules: URL validation/redaction in `url_security.rs`, source registration in `imports/sources.rs`, cache serialization/permissions in `imports/cache.rs`, and cross-process resource controls in `refresh_storage.rs`. The cache permission fix adds no public API. `git diff --stat 592a042..0c9d908` shows 27 changed files, including review/history records and tests; unrelated changes were not introduced during the final cache fix.
- Findings / rationale: `imports.rs` is a large existing coordinator and remains a soft maintainability concern; this review found no concrete reason for risky restructuring in the release candidate. Formal coupling/complexity metrics were not run (see sub-review 2).

## Sub-review 8 — Agent comprehensibility and reasoning cost
- Status: **NOT RUN**
- Evidence: No fresh independent reviewer walkthrough or measured comprehension/tool-cost study was performed. The user prohibited local agents.
- Findings / rationale: The security review maps major invariants to tests and README documents executable/data-flow trust assumptions, but self-review cannot satisfy the independent comprehensibility criterion. Do not treat this as passed.

## Sub-review 9 — Tooling baselines
- Status: **PASS**
- Evidence: `cargo fmt --all -- --check`; `CARGO_NET_OFFLINE=true cargo clippy --locked --all-targets --all-features -- -D warnings`; and serial `CARGO_NET_OFFLINE=true cargo test --locked --all-targets --all-features -- --test-threads=1` passed at `0c9d908`, and were repeated from a clean export. `CARGO_NET_OFFLINE=true cargo package --locked --allow-dirty` and `cargo build --release --locked` passed. Semgrep 1.181.0 `p/rust` and `p/secrets` results are captured in `/tmp/repodb-static-review-0c9d908-*`. Offline cargo-audit/cargo-deny and checksum checks were previously run; the lockfile/dependency graph did not change in this candidate follow-up.
- Findings / rationale: `cargo-fuzz`, `cargo-quickcheck`, `cargo-mutants`, `cargo-llvm-cov`, cargo-geiger, and dedicated standalone secret scanners were unavailable/not run. Offline advisories may be stale. These are recorded as NOT RUN/limitations; no warnings were suppressed.

## Sub-review 10 — Quality gates
- Status: **FAIL**
- Evidence: Supported Linux build, format, strict Clippy, test, package verification, clean archived-source rebuild, and isolated local Cargo install/upgrade/rollback/uninstall pass. The release gate is not complete: #127's test deliverable closed with crash/repair limits accepted only under the time-bounded #132 exception; #128's focused evidence is complete but whole-upstream FFI auditing is not claimed; #130's evidence deliverable is complete, but provenance/signing/reporting/incident-plan gaps remain explicit risks. The owner later directed GO WITH EXCEPTIONS, recorded in the security review; it does not make these checks pass.
- Findings / rationale: Hard release gates are blocked by the explicit unresolved items, including public registry/tag distribution and owner-dependent operational controls; no exception or release approval is inferred.

## Sub-review 11 — Independent comprehension review
- Status: **NOT RUN**
- Evidence: No independent reviewer was assigned because the user prohibited local agents/subagents; no external reviewer was engaged.
- Findings / rationale: Self-review cannot be reported as independent. This remains a release review limitation, not a hidden pass.

## Sub-review 12 — Agent change workflow and reporting
- Status: **PASS**
- Evidence: Scope/issues and project standards were read; relevant source, tests, and candidate docs were inspected; changes were targeted and tested; static checks, serial tests, package/build inspection, and clean-export verification were run. The automation ledger is `/tmp/automate-spec-repodb-security-20261025/run.md`. Review records and issue comments distinguish evidence from missing checks. No local agents, push, publish, deploy, release, credential change, or destructive cleanup occurred.
- Findings / rationale: An earlier exact candidate freeze was reset when review found the PDF-cache permissions defect; the fix was separately tested and the new source commit was frozen only after exact-candidate verification.

## Sub-review 13 — Independent release audit and findings
- Status: **NOT RUN**
- Evidence: No separate architecture/security/correctness/interface/release auditor was engaged; prohibited local delegation was not bypassed.
- Findings / rationale: The implementer performed a focused self-review and records one fixed cache confidentiality issue plus previously fixed status corruption behavior. This is not an independent audit.

## Sub-review 14 — Definition of done / release readiness
- Status: **FAIL**
- Evidence: Candidate code is frozen and reproducible locally from a clean archive; the exact full suite and Linux target pass. The review and issues still list unresolved crash/recovery, transitive FFI, operations, public distribution, and owner sign-off requirements.
- Findings / rationale: The standards review cannot produce a release pass while these gates remain unresolved. No version tag, publish, deploy, or go/no-go action is taken.

## Sub-review 15 — Prohibited practices and final principle
- Status: **PASS**
- Evidence: No tests were removed/weakened; failures were investigated (including the failed first permission regression, which exposed default mode 0664, and a cache-reader test updated to create a correctly private fixture). Unrun tools are explicitly listed; no release approval or ticket closure is claimed without evidence. A found permissions defect was fixed and retested.
- Findings / rationale: The work favors tested safe failure and records limitations rather than presenting incomplete checks as passes.

## Findings

- **SEC-0.3.1-CACHE-PERMISSIONS — Low; fixed at `0a64388`, retained in final candidate `0c9d908`.** Location: `src/imports/cache.rs::{write_cache_limited,visit_cache_pages_limited}`. Reproduction/evidence: cache creation under umask `0002` produced mode `0664`, making extracted PDF page text readable outside the owner; the new regression fails before the fix. Violated invariant: source-derived cached text should not gain group/world access by default. Fix: create new Unix cache temp files with mode `0600`; discard pre-existing entries with group/world permission bits before reading. Acceptance tests: `pdf_cache_is_created_with_private_permissions` and `pdf_cache_with_non_private_permissions_is_discarded_before_reading` pass at the frozen candidate. Disposition: mitigated; no exception. Owner: emfrom.
- **SEC-0.3.1-STATUS-CORRUPTION — Low; fixed at `e7b3c7b`.** Location: `src/main.rs` status schema/count queries. A missing required table was previously converted to zero counts and reported as an empty index. Fix propagates query errors; `status_rejects_a_structurally_corrupted_database_without_replacing_it` confirms visible failure and unchanged DB bytes.
- **IFACE-0.3.1-UNKNOWN-COMMAND — Low; deferred.** Unknown command diagnostics vary by whether the working directory is a Git repository. No data-integrity impact; existing behavior is documented in `docs/reviews/security/repodb-0.3.1.md`. The low-severity diagnostic inconsistency was documented and deferred in #131; it has no data-integrity impact.

## Hard release gates

- Supported builds: **PASS** — Linux x86_64 candidate release build succeeded; other platforms out of scope by #123.
- Configured formatting/static checks: **PASS** — fmt, strict Clippy, and configured Semgrep scans pass with findings triaged.
- Relevant tests: **PASS WITH LIMITATIONS** — 317 passed / 1 ignored, including clean-export rerun; fuzz/property/mutation and process-crash-boundary gaps remain.
- Security/data invariants and public contracts: **BLOCKED** — implemented and tested controls are recorded; #127's explicitly documented recovery limits and #128's bounded FFI audit scope remain release-owner risks.
- Release-blocking findings: **BLOCKED** — no known unfixed code finding, but operational/security-reporting and published-channel/provenance gates remain.
- Unsafe/undefined behavior: **BLOCKED** — production unsafe sites and sqlite-vec callback were reviewed; comprehensive transitive dependency FFI audit is not complete.
- Clean-checkout reproducibility: **PASS** — exact `git archive` tree (130 paths) passed format, Clippy, tests, and release build; binary hash reproduced.
- Supported platforms: **PASS** — Linux x86_64 scope only; no claim for other platforms.
- Dependency/security review: **BLOCKED** — prior offline cargo-audit/cargo-deny/checksum evidence is limited by advisory-cache freshness; broader FFI audit and GitHub controls remain open.
- Recovery, interface, performance, limitations: **BLOCKED** — #127's documented crash/repair limits remain unproven and were excepted for candidate-gate closure under #132; benchmark timing variance and documented trust/storage limits remain. Local source-checkout installation behavior is verified; published channel and database rollback are not.

## Final verdict

**BLOCKED.** This coding-standards review was performed against the frozen verification candidate, with subreviews explicitly marked PASS/NOT RUN/FAIL and one newly discovered low-severity cache confidentiality issue fixed and tested. At the 2026-10-25 review date, independent reviewer checks, residual security/integrity evidence, operational release controls, public registry/tag channel and provenance, and release-owner go/no-go were unresolved. The owner later directed GO WITH EXCEPTIONS on 2026-10-26 as recorded in the security review; that decision does not convert this standards verdict into a PASS or claim unavailable checks passed.
