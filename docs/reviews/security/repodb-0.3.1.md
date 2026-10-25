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
- Evidence: At exact HEAD `d90772e` (`Cargo.lock` SHA-256 `995fd7c15706d241afa6249d7cdf8ce4e795fca4`), with Cargo 1.99.0, `CARGO_NET_OFFLINE=true /home/emfrom/opt/rust/cargo/bin/cargo-audit audit --no-fetch` (cargo-audit 0.22.2; 1,348 cached advisories; 52 locked dependencies; exit 0, no findings) and `CARGO_NET_OFFLINE=true /home/emfrom/opt/rust/cargo/bin/cargo-deny check advisories bans licenses sources` (cargo-deny 0.20.2; all checks passed). Existing duplicate-`hashbrown` warning is reported (0.14.5 via hashlink/rusqlite, 0.17.1 via indexmap/toml). `Cargo.toml`, `Cargo.lock`, and `deny.toml` are unchanged from `592a042`.
- Findings / rationale: Offline audit covers only the local advisory cache; source checksum/provenance and legal compatibility are not established by these checks. No artifact provenance is established here.

### Static analysis and security-sensitive code
- [ ] Status: In progress
- Scope / owner: Exact candidate source, unsafe/FFI, parsing, conversions, paths, subprocesses, logging, permissions, and secrets / emfrom.
- Evidence: At `d90772e`, Semgrep 1.181.0 ran `semgrep scan --config p/rust --metrics off --json src` (11 rules, 19 tracked Rust files, 13 INFO findings, 0 scan errors). All findings were manually triaged: `signal`/`gmtime_r`/`MaybeUninit`/process-group/pre-exec/libsqlite-vec ABI and the new `flock` calls have adjacent safety invariants; temporary-file helpers use process/counter-derived unique names, `create_new`, and Unix mode 0600; `env::args` is ordinary CLI parsing, not a security authorization. JSON output: `/tmp/repodb-static-review-d90772e/semgrep.json`. Format and strict Clippy pass at `d90772e`.
- Findings / rationale: Semgrep's Rust rules are a limited signal, not a full Rust security audit. `gitleaks`, `trufflehog`, `detect-secrets`, `git-secrets`, and `cargo-geiger` were unavailable; no secret scanner or comprehensive unsafe/dependency FFI audit is claimed. Manual review of parsing/path/permissions is incomplete; #128 remains open.

### Input, parser robustness and resource exhaustion
- [ ] Status: In progress
- Scope / owner: Local files, Git histories/repositories, URLs/responses, PDFs/converters, segmentation, embedding, staging and persistence / emfrom.
- Evidence: Commit `c755cb5` introduced refresh-wide runtime limits, SIGINT cancellation, database/temp/Git/PDF storage caps, and cooperative cache locks. Commit `f9a1f41` streams SHA-256 and PDF page serialization, bounds legacy Git-row staging and prune-report memory, makes import-schema migration savepoint-atomic/retryable, and extends publication metadata rollback coverage. At `f9a1f41`, formatting and strict Clippy pass; serial all-target/all-feature suite passes (299 passed, 1 ignored).
- Reproducible workload: run `scripts/benchmark-refresh-resources.sh` (Python 3, Git, GNU `/usr/bin/time`, local release build; no network). The deterministic fake embedder isolates RepoDB overhead. Workload: 5,000 tracked text files / 10,075,000 bytes across two commits, 100 files modified in commit two, plus a 1,034,240-byte imported document split into 2,048 paragraphs. The final index has 10,302 history units/vectors plus 2,048 imported units, two Git commits, ~11.58 MB SQLite DB and ~20.37 MB `.git` directory; final `status` is usable with all capabilities ready.
- Repeated Linux x86_64 (`Sancho`, kernel `7.0.0-38-generic`) candidate measurements, release binary, `--size-limit=warn`: init 107.63s / 16,160 KiB and 146.25s / 15,908 KiB peak RSS (mean 126.94s); refresh 117.98s / 17,820 KiB and 131.51s / 17,796 KiB (mean 124.75s). No swap. Same harness at baseline `592a042`: init 125.90s / 15,704 KiB; refresh 113.18s / 17,272 KiB. Candidate elapsed-time differences (+0.8% init, +10.2% refresh vs that single baseline run) are not conclusive: repeated candidate init range is 30.4% of its mean and refresh range is 10.8%. Database and `.git` sizes differ from baseline by only a few KiB. Raw runs are at `/tmp/repodb-resource-repeat-large.p8O2ws/`; baseline source/build/run are under `/tmp/repodb-benchmark-baseline-592a042.RxghJa/` and `/tmp/repodb-benchmark-baseline-run.7okNoB/`.
- Findings / rationale: Measurements use a synthetic text corpus and fake embeddings, not a production model; timing and RSS comparisons are environment-dependent, and measured process RSS is not the same as the 512 MiB retained-preparation accounting cap. The README documents that allocations outside explicit accounting and filesystem use outside the capped RepoDB paths are not covered by a global OS quota. No final resource-safety conclusion or release sign-off is claimed.

### Correctness, security tests and data integrity
- [ ] Status: In progress
- Scope / owner: Source-byte/range mapping, deterministic refresh/search/history behavior, imported content and metadata, transaction/publication invariants / emfrom.
- Evidence: At `c755cb5` and follow-up commits, publication uses an IMMEDIATE transaction plus `PRAGMA data_version` validation so stale preparation cannot overwrite a concurrent model/index change. Imported content, versions, Git pins/object-store metadata, successful URL provenance, and history rows publish together. Failed URL fetch/parse/embed attempts intentionally update only `fetch_status`, `last_fetch_error`, and `last_fetch_attempt_at` outside content publication; prior version text and retrieval timestamps stay visible. Regression coverage includes `failed_url_publication_rolls_back_source_attempt_and_version_metadata`, `import_publication_sql_failure_preserves_the_previous_snapshot`, `post_publication_report_failure_does_not_revert_the_committed_snapshot`, failed Git pin publication, URL attempt-state preservation, concurrent model changes, and preparation/process failure preservation. The post-publication CLI test forces a write failure to `/dev/full`, verifies exit 2, then confirms the new version is searchable and the pruned old version is absent. Import schema migrations run in a savepoint; injected migration failure proves DDL rollback and successful retry. Hash backfill is idempotent and verifies UTF-8 representation hashes incrementally. At `4055251`, formatting and strict Clippy pass; serial all-target/all-feature suite passed (301 passed, 1 ignored); the focused post-publication test also passed after a locale-independent assertion adjustment.
- Findings / rationale: The successful transaction commit is the publication linearization point; subsequent CLI reporting is non-mutating. Its write failures now propagate as an error without reverting the committed snapshot. Process crash immediately after commit and broader corrupt-database/concurrent-writer cases remain residual gaps; no final integrity sign-off is claimed.

### Failure recovery
- [ ] Status: In progress
- Scope / owner: Refresh, subprocess, persistence, migration, cancellation, rollback, and concurrency failures / emfrom.
- Evidence: #124, #126, and #127; exact commands and fault-injection outcomes to be recorded after implementation.
- Findings / rationale: Until each required invariant is tied to executable evidence, this gate remains blocked.

### Interface, maintainability and performance
- [ ] Status: In progress
- Scope / owner: CLI/API contracts, diagnostics, maintainability, large-input workload and resource budgets / emfrom.
- Evidence: The versioned benchmark harness defines the workload, and two candidate runs plus one same-workload baseline run record elapsed time, peak RSS, disk use, unit counts, environment, and status above. At source `03b6e6b`, release-binary smoke checks: `--help` and `--version` exit 0 with stdout-only output and no DB side effects; `--version` reports `repodb 0.3.0`; an unknown command in a Git repo exits 2 with an actionable `unknown command` diagnostic. Existing exact-HEAD tests include `help_documents_shipped_commands_options_and_no_crawl_behavior_without_initializing`, `version_flag_prints_package_version_without_database_side_effects`, path/database validation, JSON/status/pagination contracts, and cancellation/output behavior; full results are recorded above.
- Findings / rationale: Candidate timing variance is high (30.4% init and 10.8% refresh range/mean); fake embedding does not measure production-model latency. No user-facing latency SLO or performance regression is established. Candidate package version remains 0.3.0 rather than the scoped 0.3.1. Unknown-command handling outside a Git repository currently reports `git rev-parse --git-dir failed` rather than the unknown-command diagnostic observed inside a repository; this is a low-severity interface/diagnostic inconsistency, not a data-integrity finding. Install/upgrade/rollback/uninstall behavior and final-candidate compatibility remain unverified for #131. Historical v0.3.1 baseline waivers are not a pass of this security/release gate.

### Build, artifacts, release hygiene and operations
- [ ] Status: In progress
- Scope / owner: Linux release build, artifacts, secrets, CI/publishing controls, data flows, reporting and rollback readiness / emfrom.
- Evidence: Provisional release build at source HEAD `03b6e6b` (Cargo metadata still reports package version `0.3.0`): `CARGO_NET_OFFLINE=true cargo build --release --locked` passed on the Linux x86_64 host with Cargo 1.99.0. Local binary `/home/emfrom/opt/rust/cargo/target/release/repodb`: ELF64 PIE, 5,617,864 bytes, SHA-256 `9b09fb7b6987da949a5b48fd5013fd326868fbe37c9ca06150ff1b5bd9517ec1`, BuildID `c6c3dfdd518bc5ebb841e78c664ab3a87b913d4f`, dynamically links glibc/libm/libgcc_s; this is a local build, not a release artifact. Separately, `cargo package --list --locked --allow-dirty` on the progress worktree at HEAD `895161d` (with the draft standards report present) listed 132 entries, emitted missing description/documentation/homepage/repository metadata warning, and included `.scratch/repodb-agent-skills/spec.md`, benchmark scripts/data, prior and in-progress security/standards review reports, and test fixtures. `.github/workflows` contains only `semgrep-core.yml`; no release/signing/publish workflow was found. `gh api repos/emfrom/repodb/private-vulnerability-reporting` reports `{"enabled":false}`; the repository API also reports GitHub secret scanning and Dependabot security updates disabled. No root `SECURITY.md`/security contact, incident/rollback/backport procedure, or supported-version plan was identified.
- Findings / rationale: Release blockers: the current Cargo package version is 0.3.0 while README/candidate scope says 0.3.1; package inclusion needs an explicit disposition (especially the in-progress candidate review and scratch spec); signing/checksum/release permissions and security-reporting/incident process need owner evidence. No publish, deploy, release, credential, or signing action is authorized. The binary/package preview is provisional and must be repeated at the eventual frozen commit.

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
