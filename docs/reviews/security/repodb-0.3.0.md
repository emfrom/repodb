# RepoDB 0.3.0 security and release-readiness review

- Status: **BLOCKED — retrospective review; not release authorization.** Unresolved security findings and missing adversarial/limit evidence prevent a security pass under this review rubric. No release owner go/no-go is issued here.
- Target commit/tag: `v0.3.0`, `f7cc6bc5451204c5696f1bbef7e5575772820f47`
- Commit range: `v0.2.2..v0.3.0` for the implementation reviewed; the final tag commit's parent is `099f4ccd7a1814df4111f66fc09d51add055b587`. Final commit is release/documentation changes, but substantive release additions occur earlier in the range.
- Release date: 2026-10-21 (tag/release records)
- Supported platforms: Linux x86_64, verified on Ubuntu 26.04.1 LTS. Windows, macOS, and other architectures are explicitly out of scope (N/A), not asserted supported or tested.
- Release owner: not identified in the reviewed evidence.
- Review type: retrospective, evidence-bounded assessment of the immutable release tag against `docs/reviews/SECURITY.md` and the cited review records. The standard and parts of the rubric postdate v0.3.0; their use here does not imply they governed release publication. This record neither authorizes nor retrospectively reauthorizes release. Later integration changes are not fixes in the tagged source and do not close tag findings.

## Scope and threat model

The release range includes substantial changes to CLI/search, history, imports, worktree search, ranking, pagination, persistence, tests, and dependency/build behavior. Relevant trust boundaries include local CLI arguments/config/environment; Git repositories, objects, paths and subprocess output; imported local files/directories and URLs/responses; PDF conversion and embedding subprocesses; SQLite persisted state and refresh publication; and serialized CLI/API output. Users may supply malicious or unusually large repositories/files/URLs. A deployment that accepts import requests from less-trusted users has a stronger network threat than a purely local trusted-user invocation; no service-side caller trust boundary was established in the reviewed evidence.

High-level flow: CLI selection and source registration feed local/Git/URL import readers; content may be decoded, segmented, converted (PDF), or embedded; temporary staging is validated and published into SQLite; history/search/worktree logic retrieves data and ranks/pages it; results/errors are emitted through CLI text/JSON. Some writes and side effects occur before publication (see S-92-1), and not every parser/converter or failure transition was dynamically traced. The security review focused on resource exhaustion, URL redirect destinations, subprocess boundaries, file/link handling, and persistence observations; it was not an exhaustive code audit.

## Review areas

### Security, scope and data flow
- [x] Status: **Fail / blocked**
- Scope / owner: retrospective reviewer; import, URL fetch, Git/process, file access, refresh and output paths.
- Evidence: direct tagged-source inspection recorded in `docs/reviews/standards/repodb-0.3.0.md` (security/resource section); tag-specific test/build evidence in `docs/reviews/security/issue-120-v0.3.0-verification.md`; target identity verified as above.
- Findings / rationale: subprocesses in inspected paths use argument arrays, not shell-composed commands. URL fetching uses curl with a 30-second timeout, redirect cap, HTTP(S)-only redirect protocols and effective-status checks; this does not enforce destination-address policy. Temporary URL headers use unique files, Unix mode 0600, and cleanup. Directory previews skip symlinks and `.git`; worktree live reads have an 8 MiB per-file cap. These controls do not resolve the unbounded aggregate import/resource risk or redirect SSRF finding below. Reviewed `gmtime_r` unsafe use is narrow; SQLite vector registration also uses FFI/transmute and a complete safety invariant was not established. Output/log secrecy and all error paths were not exhaustively audited.

### Dependencies and supply chain
- [x] Status: **Partial pass; policy scope limited**
- Scope / owner: release lockfile and production/development dependency graph on declared Linux x86_64 target.
- Evidence: `issue-120-v0.3.0-verification.md` records Rust/Cargo 1.99.0, cargo-audit 0.22.2, cargo-deny 0.20.2. On the exact tag, cargo-audit scanned 52 locked crate dependencies with no vulnerability findings; configured cargo-deny passed advisories, bans, licenses and sources. Its configured license allowlist is not a legal compatibility opinion; the duplicate hashbrown 0.14.5/0.17.1 warning is visible. The initial default cargo-deny run was unconfigured and failed; the documented follow-up used a temporary policy for exact-tag verification. No dependency tools were installed for this review.
- Findings / rationale: those checks provide bounded dependency evidence only, not supply-chain provenance/signing, artifact identity, legal advice, or checks for other platforms. The record is not evidence of a signed/reproducible published artifact.

### Static analysis and security-sensitive code
- [x] Status: **Fail / incomplete**
- Scope / owner: tagged Rust source and security-relevant paths.
- Evidence: on 2026-10-21 UTC, `cargo fmt --all -- --check` passed on the clean immutable `/tmp/repodb-030` release-tag checkout using Cargo 1.99.0 and rustfmt 1.10.0-stable (b940084d7e 2026-09-28). With `clippy 0.1.99 (b940084d7e 2026-09-28)` / `rustc 1.99.0 (b940084d7 2026-09-28)`, this session also ran `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/tmp/repodb-120-target cargo clippy --all-targets --all-features -- -D warnings`; it failed (exit 101) on five diagnostics: `redundant_closure` in `src/pagination.rs:146`, `unnecessary_fold` at `:241`, and `collapsible_if` in `src/ranking.rs:255,264,265`. This independently confirms the previously recorded Rust/Clippy 1.99.0 TB-97-1 result; later integration Clippy fixes do not change this tag. No comprehensive unsafe, secret, static security analyzer, Miri or sanitizer audit is evidenced.
- Findings / rationale: static/security review is incomplete. FFI vector registration lacks established complete safety invariants in reviewed evidence; no concrete exploit is asserted. No dedicated secret scan result is available.

### Input, parser robustness and resource exhaustion
- [x] Status: **Fail — SEC-93-1 and SEC-93-2 unresolved at tag**
- Scope / owner: imported files/repositories/URLs, child processes, PDF/source inputs and resource budgets.
- Evidence: `src/budget.rs` describes `usize::MAX` accounting rather than a configured finite ceiling. Local files are read into memory; Git output/import paths can accumulate very large data. Git clone and other non-curl subprocesses have no established overall deadline. URL curl has a bounded timeout, but that is not a refresh-wide bound. URL fetching follows HTTP(S) redirects without validating destination IP ranges. Existing tests cover some size modes, subprocess output handling, and redirect protocol/provenance, but no end-to-end adversarial SSRF destination test, aggregate memory ceiling, or hung-child timeout test was run/evidenced.
- Findings / rationale: see SEC-93-1 and SEC-93-2 below. The release review does not claim bounded refresh memory/CPU/disk/time or safe redirect destinations.

### Correctness, security tests and data integrity
- [x] Status: **Partial pass; security gaps remain**
- Scope / owner: full test suite and selected persistence/security regressions.
- Evidence: in this session, from clean `/tmp/repodb-030` at exact immutable release tag `f7cc6bc5451204c5696f1bbef7e5575772820f47`, ran `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/tmp/repodb-120-target cargo test --all-targets --all-features`; passed, with one ignored test. Also ran `cargo fmt --all -- --check`; passed. The test command was run here on the release-tag checkout, not inferred from later integration. Historical #120 verification records a complete all-target test pass and one ignored worktree race-child test. Tests provide executable evidence but do not demonstrate untested hostile-input, resource-limit, or every data invariant.
- Findings / rationale: tests for failed import refresh and publication exist, but the tagged flow can mutate some source metadata outside the content publication transaction (S-92-1), so preservation of all durable metadata on failure is not established. Follow-up integration evidence says Git pin state was later staged transactionally and URL fetch status/error/attempt-time is explicitly attempt-state; these later semantics are not changes to v0.3.0 and do not close the tag finding. No property-based/fuzz/mutation/security regression run is evidenced.

### Failure recovery
- [x] Status: **Fail / partial evidence**
- Scope / owner: failed import/refresh, staging/publication, subprocess and cache side effects.
- Evidence: full test pass above; source review in standards review state/history paths indicate staged content publication uses a transaction. However unresolved Git pin/source fetch status updates can be written before that transaction; URL fetch attempt status is durable attempt-state and may remain changed on failed refresh. PDF cache uses temporary file, sync, rename and is described as an optimization. No crash/power-loss, cancellation, broad concurrent writer, or full subprocess failure-injection matrix was executed here.
- Findings / rationale: S-92-1 records the tag's partial durable side-effect behavior. Recovery is not certified across all interruption boundaries.

### Interface, maintainability and performance
- [x] Status: **Partial / not passed as a gate**
- Scope / owner: CLI/API contracts, complex paths, resource and performance behavior.
- Evidence: all-target tests pass here; review records identify focused CLI/pagination coverage, but comprehensive compatibility checks, fresh-consumer API checks, complexity metrics on the tag, and representative resource/performance measurements were not run in this security review. The release spec explicitly says its pinned real-model baseline and numeric quality/performance thresholds were waived/not completed, and disclaims quality, accuracy/recall, latency, memory, cost, refresh-time and storage-performance claims. Later integration #118 selected CLI comparisons and #121 complexity metrics are bounded later evidence, not tag-wide security validation.
- Findings / rationale: resource ceilings remain unproven (SEC-93-1). The explicit baseline waiver is not a pass or security exception for SEC-93 findings. No claim is made that all interfaces or performance are correct.

### Build, artifacts, release hygiene and operations
- [x] Status: **Partial; artifact integrity not established**
- Scope / owner: supported target, available build/test/dependency evidence, provenance and operational recovery.
- Evidence: #120 records exact-tag Linux x86_64 / Ubuntu 26.04.1 release build and all-target test pass, cargo-audit pass, and configured cargo-deny pass. Windows/macOS/other architectures are explicitly out of scope (N/A). The Semgrep-only Ubuntu 24.04 workflow is not full-platform test evidence. No artifact checksum/signature, reproducible build provenance, packaging inspection, publishing permission review, or rollback/incident exercise is evidenced here.
- Findings / rationale: build/tests do not establish published artifact authenticity. Availability of a reporting route/operational response was not verified.

### Conditional reviews

- **Network/URL fetching — triggered.** HTTP(S) redirects are supported, curl has timeout/redirect/protocol controls, but no restricted destination validation across redirects is established; SEC-93-2 remains unresolved. DNS rebinding, proxy behavior and private/link-local/loopback targets were not adversarially tested.
- **Subprocesses/executable use — triggered.** Inspected commands use argument arrays; URL curl timeout is bounded, but Git clone and other subprocess total time/output/resource behavior do not have established finite limits. No exhaustive executable discovery/environment/timeout audit or hung-process test.
- **Filesystem/Git object access — triggered.** Symlink and `.git` exclusions, worktree file cap, and argument-array Git usage are positive controls. Malicious object graphs, unusual path races, symlink/submodule adversarial cases, huge histories and aggregate cost were not comprehensively exercised. SEC-93-1 remains.
- **Persistence/database — triggered.** SQLite staging/publication and tests are present; S-92-1 identifies pre-publication metadata writes at tag. Corrupt database, migration rollback and crash-boundary testing are not comprehensively evidenced here.
- **Cryptography/secrets — not triggered by reviewed release surface / not assessed.** No cryptographic protocol/key-management change was identified in this scoped evidence. No secret scan was run; this N/A does not imply a secret scan passed.
- **Web UI/API/browser — N/A.** No browser-facing UI or web service surface was identified in scope; the CLI and public Rust API remain in scope above.
- **New platform/distribution — N/A beyond declared platform.** Sole verified support target is Linux x86_64 / Ubuntu 26.04.1; other OS/architectures explicitly out of scope.
- **Security vulnerability remediation — not established.** No evidence in these records establishes that SEC-93-1 or SEC-93-2 was fixed on the exact tag. Later integration work or dispositions must not be represented as a tag fix.

## Findings and exceptions

### SEC-93-1 — Unbounded import/refresh resource consumption
- Severity: **High potential availability impact**, particularly where less-trusted users can initiate imports; exploitability depends on caller trust and resource isolation. This severity is retrospective and does not assert a demonstrated production exploit.
- Affected component/invariant: `src/budget.rs`, `src/imports.rs` local file/Git/import refresh and subprocess handling. Intended invariant: untrusted input and work must have finite, operator-understood byte, CPU, disk and wall-time bounds, with controlled cancellation/failure.
- Reproducible scenario: submit/import an extremely large local file or repository, or a Git clone/child operation that does not terminate. The accounting ceiling is effectively `usize::MAX`; file reading/output accumulation can consume unbounded practical memory/resources, and clone has no established deadline. The review did not execute a resource-exhaustion exploit.
- Evidence/disposition: found in tagged source; unresolved at `f7cc6bc`. Tests run here do not test finite aggregate ceilings or bounded child duration. Later integration fixes, if any, are not evidence of a fix in this release.
- Proposed remediation and acceptance tests: define finite per-input and aggregate byte/record limits and an overall/per-child deadline covering Git, embedding and converters; ensure limits include output/stderr and temporary storage where relevant; abort cleanly without publishing partial state and preserve recoverable prior state. Add tests for oversized local/Git inputs, cumulative multi-source budget, a hung child, output floods, cancellation, and failed-publication state; assert bounded termination/resource behavior and operator-visible diagnostics.
- Owner/due date/exception: not established. No authorized, time-bounded exception for this finding was found.

### SEC-93-2 — Redirect-based SSRF destination policy not established
- Severity: **High potential confidentiality/security impact in service or delegated-import deployments**; lower in a strictly local, trusted-user-only model. Deployment trigger is conditional; no deployment architecture was verified.
- Affected component/invariant: `src/imports.rs` URL fetch/redirect handling. Intended invariant: URL import must not let an untrusted request reach destinations outside its authorized network boundary.
- Reproducible scenario: configure a publicly reachable URL to redirect to loopback, private, link-local or otherwise sensitive service; fetch follows HTTP(S) redirects, and code does not establish restricted-address validation of each destination. This is a code-path assessment; no SSRF request was executed.
- Evidence/disposition: tagged code limits redirect count and protocols but does not establish destination-address policy. No trusted-user-only deployment constraint was found; finding remains unresolved for a threat model that accepts less-trusted URL submissions.
- Proposed remediation and acceptance tests: define/document caller trust and intended egress policy. If callers/URLs are not fully trusted, validate every redirect destination against restricted ranges, account for DNS rebinding and connection-time address changes, and test loopback/private/link-local destinations, redirect chains, DNS changes, allowed public destinations and proxy behavior. If unrestricted fetch is intentional, make that trust requirement and exposure explicit and enforce it at the deployment boundary.
- Owner/due date/exception: not established. No authorized, time-bounded exception was found.

### Other precise evidenced issues / limitations

- **S-92-1 (medium, tag data-state failure boundary):** tagged refresh may update Git pin metadata during preparation (`src/imports.rs:771–787`) and URL fetch status/error/attempt time before content publication (`:1221–1229`). A later failure can leave durable metadata changed while staged content is not published. Acceptance: make publication metadata transactional or specify attempt-state semantics and add failure-injection assertions. The integration addendum says Git pin writes were later staged transactionally and URL fetch status is attempt-state; this is not a fix in the immutable tag.
- **TB-97-1 (tooling):** strict Clippy invocation failed on the tag under Rust/Clippy 1.99.0; later integration pass does not change tag result. This is not itself a security vulnerability but is a recorded static-check failure.
- SQLite vector registration uses FFI/transmute; the scoped review did not establish its complete safety invariant. No concrete issue is claimed; dedicated unsafe audit remains unperformed.
- Several large orchestration/import/history functions were identified in standards review as maintainability/context risks, but approximate line spans are not proof of a complexity threshold breach or vulnerability.

No exception in this record converts an unresolved finding or unrun check into a pass. The historical baseline quality/performance override documented in the release spec concerns that baseline gate only and is not an exception for the security findings. Approval authority/expiry for a security exception was not evidenced.

## Final release gate

- Functional: all-target/all-feature suite passed in this session at the immutable release tag, with one ignored test; this is bounded test evidence, not proof of all behaviors.
- Security: **FAIL / BLOCKED** — SEC-93-1 and SEC-93-2 unresolved; no authorized time-bounded exceptions evidenced.
- Data integrity: **PARTIAL / NOT PASSED** — transaction and test evidence exists, but S-92-1 and incomplete failure-injection/corruption/crash evidence remain.
- Failure handling: **NOT PASSED** — relevant tests pass, but resource limits, child deadlines and interruption recovery are not established.
- Interface: partial; no broad compatibility or exhaustive CLI/API contract assessment here.
- Structure: concerns documented; no release-tag complexity analysis sufficient to pass this gate.
- Performance: **NOT ESTABLISHED**; release baseline/threshold work explicitly waived and not equivalent to a measurement pass.
- Reproducibility: exact-tag Linux x86_64 build evidence is recorded in #120; formatting and all-target tests rerun here. Artifact provenance/signing/reproducible-build properties are not established. Other platforms are N/A/out of scope.
- Decision and rationale: **BLOCKED; retrospective record only, no release authorization.** The two security findings have concrete acceptance criteria and no evidenced exception. Additional recovery, artifact, and adversarial verification gaps remain. Later integration fixes do not alter v0.3.0.
- Release owner / date: no release-owner decision is made; review record prepared retrospectively.

## Evidence and limitations

- On 2026-10-21 UTC, this session ran `cargo fmt --all -- --check` (pass; Cargo 1.99.0, rustfmt 1.10.0-stable), `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/tmp/repodb-120-target cargo test --all-targets --all-features` (pass; one ignored test; Cargo/Rust 1.99.0), and `CARGO_NET_OFFLINE=true CARGO_TARGET_DIR=/tmp/repodb-120-target cargo clippy --all-targets --all-features -- -D warnings` (fail, exit 101; five diagnostics listed above; Clippy 0.1.99, Rust 1.99.0) from clean `/tmp/repodb-030`, the immutable `v0.3.0` tag checkout at the exact SHA above. These results are from the release-tag checkout, not later integration.
- Additional exact-tag evidence (build, audit, configured deny) is reported in `issue-120-v0.3.0-verification.md`; it is attributed to that record, not claimed as rerun here. Tool versions/environment are given there (Ubuntu 26.04.1 LTS, Linux x86_64, Rust/Cargo 1.99.0, cargo-audit 0.22.2, cargo-deny 0.20.2).
- Security findings are based on the tagged-source review documented in `docs/reviews/standards/repodb-0.3.0.md`, not successful exploit reproduction. No source fixes, ticket closure, GitHub contact, network access, dependency installation, or adversarial exploit test is claimed.
- RepoDB search was attempted but unavailable (`repodb: no embeddings; run refresh`); no index refresh was performed. Source evidence was read directly.
- This review does not verify a particular deployment's caller permissions, egress controls, container/resource limits, release artifact provenance, or historical release authorization. Such controls could reduce exposure but are not credited without evidence.
