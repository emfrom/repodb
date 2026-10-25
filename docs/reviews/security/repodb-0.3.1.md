# RepoDB 0.3.1 candidate security and release-readiness review

- Status: **In progress — candidate not frozen; no release authorization.**
- Target commit/tag: Candidate baseline `592a0422917f249df800de751dd8e5bad14a430b`; latest #124 implementation commit `c755cb5` on `integration/repodb-spec`. This is not a frozen candidate. Final target commit/tag must be pinned after all remediation and reviews.
- Commit range: Pending final candidate selection; compare from the agreed release baseline and record exact range before sign-off.
- Release date/window: As soon as review passes; date not set.
- Supported platforms: Linux only. Current work environment: x86_64 Linux (`Sancho`, kernel `7.0.0-38-generic`); confirm against the final candidate.
- Release owner: emfrom (@emfrom).

## Scope and threat model

This is the active 0.3.1 release candidate review, separate from the immutable retrospective v0.3.0 assessment at `docs/reviews/security/repodb-0.3.0.md` (tag `v0.3.0`, commit `f7cc6bc5451204c5696f1bbef7e5575772820f47`). The active scope is the current integration candidate and its eventual frozen commit; this record must be updated whenever the candidate changes. The locally operated CLI trust model treats imported URLs, repository content, files, Git objects, and parser/converter inputs as untrusted. URL fetching must not reach loopback, private, link-local, multicast, reserved, or otherwise restricted destinations, including through redirects or DNS changes. The release owner makes the final go/no-go decision and any authorized time-bounded exception.

Scope owner and release owner: emfrom. Exact changed-component inventory, candidate commit range, target architecture/environment, and operational evidence remain to be recorded after implementation is complete.

## Review areas

### Security, scope and data flow
- [ ] Status: In progress
- Scope / owner: Candidate implementation, imports/URL egress, Git/process boundaries, persistence, and observable CLI behavior / emfrom.
- Evidence: Scope decision recorded in GitHub issue #123; remediation and evidence tasks #124–#131 and standards review #133. Candidate commit is not frozen.
- Findings / rationale: No final candidate-wide review is claimed. Security-sensitive changed paths and residual risks must be assessed against the exact frozen commit.

### Dependencies and supply chain
- [ ] Status: In progress
- Scope / owner: Complete Linux-target locked production, development, build, and platform-specific dependency graph / emfrom.
- Evidence: At baseline `592a042`, `CARGO_NET_OFFLINE=true /home/emfrom/opt/rust/cargo/bin/cargo-audit audit --no-fetch` (cargo-audit 0.22.2; cached database contained 1,348 advisories; 52 locked dependencies; exit 0, no findings) and `CARGO_NET_OFFLINE=true /home/emfrom/opt/rust/cargo/bin/cargo-deny check advisories bans licenses sources` (cargo-deny 0.20.2; all checks passed). Existing duplicate-`hashbrown` warning is reported. `Cargo.toml`, `Cargo.lock`, and `deny.toml` are unchanged from baseline/evidence commit `402e5e8` and prior baseline `43bda0b`.
- Findings / rationale: Re-run or tie this evidence to the final exact candidate; offline audit covers the local advisory cache only. The `deny.toml` license allowlist is not legal advice. No artifact provenance is established here.

### Static analysis and security-sensitive code
- [ ] Status: In progress
- Scope / owner: Exact candidate source, unsafe/FFI, parsing, conversions, paths, subprocesses, logging, permissions, and secrets / emfrom.
- Evidence: Existing Semgrep progress is recorded on issue #128 at earlier integration commit `9646191`; it is not a final-candidate result. Exact-candidate formatting, strict Clippy, static analysis, manual unsafe/FFI review, and secret scan remain to be recorded.
- Findings / rationale: No full final-candidate audit or secret scan is claimed.

### Input, parser robustness and resource exhaustion
- [ ] Status: In progress
- Scope / owner: Local files, Git histories/repositories, URLs/responses, PDFs/converters, segmentation, embedding, staging and persistence / emfrom.
- Evidence: Commit `c755cb5` introduced refresh-wide runtime limits, SIGINT cancellation, database/temp/Git/PDF storage caps, and cooperative cache locks. The current follow-up work also streams SHA-256 and PDF page serialization, bounds legacy Git-row staging and prune-report memory, makes import-schema migration savepoint-atomic/retryable, and extends publication metadata rollback coverage. Latest validation: formatting and strict Clippy pass; serial all-target/all-feature suite passes (299 passed, 1 ignored).
- Reproducible workload: run `scripts/benchmark-refresh-resources.sh` (Python 3, Git, GNU `/usr/bin/time`, local release build; no network). The deterministic fake embedder isolates RepoDB overhead. Workload: 5,000 tracked text files / 10,075,000 bytes across two commits, 100 files modified in commit two, plus a 1,034,240-byte imported document split into 2,048 paragraphs. The final index has 10,302 history units/vectors plus 2,048 imported units, two Git commits, ~11.58 MB SQLite DB and ~20.37 MB `.git` directory; final `status` is usable with all capabilities ready.
- Repeated Linux x86_64 (`Sancho`, kernel `7.0.0-38-generic`) candidate measurements, release binary, `--size-limit=warn`: init 107.63s / 16,160 KiB and 146.25s / 15,908 KiB peak RSS (mean 126.94s); refresh 117.98s / 17,820 KiB and 131.51s / 17,796 KiB (mean 124.75s). No swap. Same harness at baseline `592a042`: init 125.90s / 15,704 KiB; refresh 113.18s / 17,272 KiB. Candidate elapsed-time differences (+0.8% init, +10.2% refresh vs that single baseline run) are not conclusive: repeated candidate init range is 30.4% of its mean and refresh range is 10.8%. Database and `.git` sizes differ from baseline by only a few KiB. Raw runs are at `/tmp/repodb-resource-repeat-large.p8O2ws/`; baseline source/build/run are under `/tmp/repodb-benchmark-baseline-592a042.RxghJa/` and `/tmp/repodb-benchmark-baseline-run.7okNoB/`.
- Findings / rationale: Measurements use a synthetic text corpus and fake embeddings, not a production model; timing and RSS comparisons are environment-dependent, and measured process RSS is not the same as the 512 MiB retained-preparation accounting cap. The README documents that allocations outside explicit accounting and filesystem use outside the capped RepoDB paths are not covered by a global OS quota. No final resource-safety conclusion or release sign-off is claimed.

### Correctness, security tests and data integrity
- [ ] Status: In progress
- Scope / owner: Source-byte/range mapping, deterministic refresh/search/history behavior, imported content and metadata, transaction/publication invariants / emfrom.
- Evidence: At `c755cb5` and current follow-up work, publication uses an IMMEDIATE transaction plus `PRAGMA data_version` validation so stale preparation cannot overwrite a concurrent model/index change. Imported content, versions, Git pins/object-store metadata, successful URL provenance, and history rows publish together. Failed URL fetch/parse/embed attempts intentionally update only `fetch_status`, `last_fetch_error`, and `last_fetch_attempt_at` outside content publication; prior version text and retrieval timestamps stay visible. Regression coverage includes `failed_url_publication_rolls_back_source_attempt_and_version_metadata`, `import_publication_sql_failure_preserves_the_previous_snapshot`, failed Git pin publication, URL attempt-state preservation, concurrent model changes, and preparation/process failure preservation. Import schema migrations run in a savepoint; injected migration failure proves DDL rollback and successful retry. Hash backfill is idempotent and verifies UTF-8 representation hashes incrementally. Full suite results are recorded above.
- Findings / rationale: The successful transaction commit is the publication linearization point; later CLI reporting is non-mutating and cannot roll back a committed snapshot. Crash-after-commit fault injection and broader concurrent-writer/migration corruption cases remain for #126/#127 review; no final integrity sign-off is claimed.

### Failure recovery
- [ ] Status: In progress
- Scope / owner: Refresh, subprocess, persistence, migration, cancellation, rollback, and concurrency failures / emfrom.
- Evidence: #124, #126, and #127; exact commands and fault-injection outcomes to be recorded after implementation.
- Findings / rationale: Until each required invariant is tied to executable evidence, this gate remains blocked.

### Interface, maintainability and performance
- [ ] Status: In progress
- Scope / owner: CLI/API contracts, diagnostics, maintainability, large-input workload and resource budgets / emfrom.
- Evidence: The versioned benchmark harness defines the workload, and two candidate runs plus one same-workload baseline run record elapsed time, peak RSS, disk use, unit counts, environment, and status above.
- Findings / rationale: Candidate timing variance is high (30.4% init and 10.8% refresh range/mean); fake embedding does not measure production-model latency. No user-facing latency SLO or performance regression is established. CLI contract matrix, install/upgrade/rollback/uninstall behavior, and final-candidate compatibility remain for #131. Historical v0.3.1 baseline waivers are not a pass of this security/release gate.

### Build, artifacts, release hygiene and operations
- [ ] Status: In progress
- Scope / owner: Linux release build, artifacts, secrets, CI/publishing controls, data flows, reporting and rollback readiness / emfrom.
- Evidence: #130; candidate-specific build provenance, artifact checksums/contents, secret scan, permissions, incident/rollback evidence, and supported-version plan remain to be recorded.
- Findings / rationale: No release, publish, or deployment action is authorized by this review record.

### Conditional reviews
- Network/URL fetching: **Triggered**. At `c755cb5`, deterministic URL-security tests cover restricted/public IPv4/IPv6, mixed DNS answers, per-hop redirect revalidation, pinned DNS entries, and parser/scheme/credential rejection. CLI fixtures cover private redirect rejection before the next request, loopback default-deny/explicit allowlist, real curl with proxy variables set, response/redirect handling, failed-fetch snapshot preservation, and offline search. Full serial suite passed at this commit. Policy and the bounded-resolver availability tradeoff are documented; #125 evidence is posted in the issue.
- Subprocess/executable behavior: **Triggered**; bounds, deadlines, output capture, cancellation, environment, and child cleanup tracked in #124 and #128.
- Filesystem/Git object access: **Triggered**; hostile objects, path/symlink/submodule behavior, and resource costs tracked in #124 and #127.
- Database/persistence/migration: **Triggered**; transaction, metadata, corruption, migration, and rollback behavior tracked in #126 and #127.
- Secrets/artifacts/operations: **Triggered**; evidence tracked in #128 and #130.
- Web UI/browser surface: **N/A for current CLI candidate**; re-evaluate if scope changes.
- Cryptographic/key-management changes: **Not yet assessed**; record N/A with evidence or perform specialist review if triggered by final diff.
- Other platforms/distribution channels: **Out of scope by owner decision**; Linux only. Record exact Linux architecture/environment before final sign-off.

## Findings and exceptions

No new findings are asserted against the unfrozen candidate by this in-progress record. Historical findings SEC-93-1, SEC-93-2, and S-92-1 belong to the immutable v0.3.0 tag assessment; candidate remediation and verification are tracked in #124–#127. Record any newly verified candidate findings here with severity, reproducible scenario, affected invariant/component, disposition, mitigation, owner, due date, and any explicitly authorized exception with approver and expiry.

## Final release gate

- Functional: Pending exact-candidate full relevant test suite.
- Security: **BLOCKED / not yet reviewed** against frozen candidate.
- Data integrity: Pending #126/#127 evidence.
- Failure handling: Pending #124/#126/#127 evidence.
- Interface: Pending #131 evidence.
- Structure: Pending #133 review.
- Performance: Pending measured workload and budget under #124/#131.
- Reproducibility: Pending exact-candidate build/artifact evidence under #130.
- Decision and rationale: No go/no-go decision made. The release owner decides after all required evidence is complete; missing/failed checks block absent an authorized, explicit, time-bounded exception.
- Release owner / date: emfrom; decision date pending.
