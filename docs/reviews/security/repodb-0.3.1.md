# RepoDB 0.3.1 candidate security and release-readiness review

- Status: **In progress — candidate not frozen; no release authorization.**
- Target commit/tag: Candidate baseline `592a0422917f249df800de751dd8e5bad14a430b`; final target commit/tag must be pinned after all remediation and reviews.
- Commit range: Pending final candidate selection; compare from the agreed release baseline and record exact range before sign-off.
- Release date/window: As soon as review passes; date not set.
- Supported platforms: Linux only; architecture and tested environment to be recorded against the final candidate.
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
- Evidence: Work tracked by #124 and #125. Prior integration evidence includes a finite preparation-memory budget, per-child deadlines, bounded subprocess output, per-preparation record caps, and URL destination checks; exact current/final commit evidence and remaining storage/runtime/cancellation/workload criteria are pending.
- Findings / rationale: Temporary/durable storage bounds, total refresh runtime, explicit cancellation, and measured workload/resource budget remain open under #124. Do not infer complete resource safety from memory/record limits alone.

### Correctness, security tests and data integrity
- [ ] Status: In progress
- Scope / owner: Source-byte/range mapping, deterministic refresh/search/history behavior, imported content and metadata, transaction/publication invariants / emfrom.
- Evidence: #126 and #127. Prior SQL publication-failure and attempt-state evidence exists on earlier commits; final-candidate coverage and remaining transaction/fault-injection matrix are pending.
- Findings / rationale: No final-candidate integrity sign-off is claimed.

### Failure recovery
- [ ] Status: In progress
- Scope / owner: Refresh, subprocess, persistence, migration, cancellation, rollback, and concurrency failures / emfrom.
- Evidence: #124, #126, and #127; exact commands and fault-injection outcomes to be recorded after implementation.
- Findings / rationale: Until each required invariant is tied to executable evidence, this gate remains blocked.

### Interface, maintainability and performance
- [ ] Status: In progress
- Scope / owner: CLI/API contracts, diagnostics, maintainability, large-input workload and resource budgets / emfrom.
- Evidence: #131; no candidate-specific contract matrix or representative measured workload is recorded yet.
- Findings / rationale: A defined workload, budget, environment, latency and memory/disk observations, repeated-work analysis, and compatibility results remain required. Historical v0.3.1 baseline waivers are not a pass of this security/release gate.

### Build, artifacts, release hygiene and operations
- [ ] Status: In progress
- Scope / owner: Linux release build, artifacts, secrets, CI/publishing controls, data flows, reporting and rollback readiness / emfrom.
- Evidence: #130; candidate-specific build provenance, artifact checksums/contents, secret scan, permissions, incident/rollback evidence, and supported-version plan remain to be recorded.
- Findings / rationale: No release, publish, or deployment action is authorized by this review record.

### Conditional reviews
- Network/URL fetching: **Triggered**; destination policy, redirects, DNS/address changes, proxies, schemes, credentials, limits, and tests tracked in #125.
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
