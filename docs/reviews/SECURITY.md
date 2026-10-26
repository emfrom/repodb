# Security release review

Security reviews are mandatory on the cadence defined in [`README.md`](README.md): before RepoDB 1.0.0, for every major- or minor-version increase release (including 1.0.0), and after 1.0.0, before every release. For each release subject to review, ensure the matching master security-review issue and review-area sub-issues exist, creating only those that are missing; follow [`README.md`](README.md) for ticket structure and deduplication. Reviewers append the plan, results, evidence, findings, and decision to `docs/reviews/security/repodb-<version>.md`. Do not create additional tickets for findings; record findings and dispositions in that versioned output file.

Create one master review record per version and maintain it in place as review work proceeds. Start from the template in **Versioned review record**. Record the exact version, commit range, target commit/tag, release date, supported platforms, scope, owners, and findings. Include every required review area; note conditionals that do not apply and why. The master record must make status and evidence easy to audit. Do not represent an unrun check as passed.

## Audit, remediation, and re-audit

For each release subject to security review, read and update [`THREAT_MODEL.md`](THREAT_MODEL.md) before assessing findings. Record the model's reviewed revision in the versioned review record. The threat model defines assumptions and boundaries; the versioned record contains release-specific evidence, findings, and the decision. If a boundary or assumption is unknown, record it as unknown and assess the resulting exposure rather than assuming a protective control exists.

Use a repeatable loop: assess the candidate and record findings; define testable acceptance criteria; implement and test fixes; independently verify each disposition; then re-audit the final candidate. A fix on another branch does not close a finding against an immutable tag. Keep historical results visible, and distinguish tests run before a fix from tests run afterward.

Assign a separate adversarial review agent, outside the implementation agent's context, to verify material security findings and the final candidate's key safety and integrity claims. Use a capable frontier model; it may be the same model as the implementer, but it must start from an independent context and evidence path. Give it the frozen candidate, threat model, acceptance criteria, and evidence pointers, not only the implementer's account. Direct it to try to falsify each assigned claim through source inspection and safe, bounded reproductions or targeted test reruns. Record the agent's model and role, candidate commit, claims challenged, commands/results, counterexamples, and unverified claims in the versioned record. Endorsement of the implementer's summary alone is not verification.

Freeze the final candidate's commit, lockfile, toolchain, supported-platform scope, and intended artifacts before final verification. Record commands, versions, environment, outcomes, and durable evidence locations against that candidate. Re-run affected checks when the candidate changes; run the final security review after other release reviews and remediations, as required by [`README.md`](README.md). The release owner signs off only on the candidate actually reviewed.

The release owner makes the final go/no-go decision from the evidence in the versioned record. Release is blocked until required checks have evidence and a clear pass, or an authorized, explicit, time-bounded exception and mitigation. A scanner's clean output alone is not a security review. If a finding requires remediation, document it in the record with a reproducible scenario, severity, affected invariant/component, proposed fix, and acceptance test; do not create a separate issue for the finding.

## Audit model

The goal is not abstract code perfection. Establish that the release is correct, predictable, safe with untrusted inputs, recoverable after failure, and maintainable. These are audit categories, not eight independent passes: a single test or review may provide evidence for several categories. Every applicable category must nevertheless be covered in the versioned record with evidence.

| Area | Release question | Priority |
| --- | --- | --- |
| Security | Are command execution, path access, malicious inputs, persistence, and network boundaries safe? | Critical |
| Correctness | Are search, Git history, indexing, deduplication, and embedding results correct? | Critical |
| Data flow | Are input-to-output paths, trust boundaries, transformations, ownership, and failure propagation understood? | Critical |
| Failure recovery | Do interrupted work, subprocess failures, corruption, rollback, and concurrent writes leave recoverable state? | Critical |
| Interface contracts | Are CLI arguments, JSON, exit codes, errors, and compatibility predictable? | High |
| Code structure | Are call paths, coupling, state, invariants, complexity, and dead code understood? | High |
| Performance | Are memory, large repositories, query latency, repeated work, and resource exhaustion acceptable? | High |
| Release hygiene | Are builds, dependencies, platform tests, documentation, and installation ready? | High |

Priority guides attention; it does not substitute for severity assessment. A failure of a critical data-integrity invariant blocks release until fixed or explicitly excepted under the release gate.

## Required review areas

For each release subject to the review cadence, review and record these areas. Collectively they must cover all eight audit areas above. Combine adjacent sections in the versioned record only if each remains independently verifiable.

### 1. Release scope, threat changes, and security-sensitive diff

**Acceptance criteria**
- Record the exact commit range and enumerate changed components, dependencies, build/release workflows, configuration, and public interfaces.
- Identify trust boundaries and attacker-controlled inputs affected by the change; consider local files, Git repositories/objects, imported URLs/content, CLI arguments, environment/configuration, and persisted database data as applicable.
- Compare the release against `THREAT_MODEL.md`: record changed assumptions, entry points, assets, attacker capabilities, deployment constraints, and controls. Cite evidence for controls relied on to reduce severity or close a finding.
- Review security-sensitive changes for authorization, validation, path/URL handling, subprocess use, resource limits, unsafe code, and error behavior. State why each area is in or out of scope.
- Record new or changed threats, mitigations, residual risks, and links to fixes/findings. Confirm the release notes do not overstate security properties.
- Map each major operation end-to-end from input through read/validation/decoding, transformations (including segmentation/embedding where applicable), persistence, and observable output. At each boundary record preconditions, postconditions, ownership, and failure propagation; identify any untraced path.

### 2. Dependency and supply-chain review

**Acceptance criteria**
- Audit direct and transitive production dependencies and development/build dependencies used in the release; include the lockfile and platform-specific dependencies.
- Run the repository's available dependency/advisory and license checks, recording tool/version, command, date, commit, and complete result. Verify significant advisories against authoritative sources and assess actual reachability/impact.
- Review dependency additions, upgrades, removals, abandoned/compromised package warnings, and unexpected lockfile changes. Confirm dependency sources and release artifacts are the intended ones.
- For each actionable vulnerability, record severity, affected versions, proposed fix or mitigation, and release decision in the versioned review file. Document false positives and accepted residual risk with rationale.

### 3. Static analysis and unsafe/security-sensitive code

**Acceptance criteria**
- Run the repository's available static analysis, compiler/linter security checks, and secret scanning over the release commit; record commands, versions, and results. Include changed code and relevant surrounding code in manual review.
- Inspect unsafe code, deserialization/parsing, cryptographic use, permissions, path traversal, injection, integer/size handling, and error/logging paths touched by the release.
- Triage every finding: link a fix, justify a false positive, or document an accepted risk and approver. No unexplained high-confidence security finding remains.

### 4. Input validation, parser robustness, and resource-exhaustion review

**Acceptance criteria**
- Verify validation and limits for untrusted or potentially large inputs, including malformed/hostile repositories, file contents, URLs/responses, and persisted data where applicable.
- Check bounds on bytes, records, recursion/depth, pagination, timeouts, concurrency, disk/database growth, and subprocess/runtime costs; verify failures are controlled and do not expose secrets or corrupt state.
- Run relevant malformed-input, boundary, regression, and resource-limit tests. Record test commands and outcomes; document uncovered gaps as findings in the versioned review file.
- Explicitly exercise malicious Git object data, unusual filenames, large histories, symlinks/submodules and pathological trees; imported URLs for SSRF where applicable, redirects, unsupported protocols, streaming, byte/time limits; and malformed PDF/source inputs, invalid byte offsets, oversized output, crashes, and hangs for external parsers/converters.
- Where the application has no fixed overall indexing-size ceiling, test and document resource-exhaustion behavior and operator-visible failure/recovery rather than assuming memory safety prevents exhaustion. Record practical tested workload/budget and any intentional unbounded behavior as residual risk.

### 5. Security tests and regression coverage

**Acceptance criteria**
- Map security-relevant changes and identified threats to automated tests; cover both expected rejection and legitimate use.
- Run the full relevant test suite on the release commit and record exact commands, environment/platform, and results. Include integration/CLI tests and supported-platform checks when applicable.
- Verify applicable data invariants with executable tests: intended source bytes are represented except documented exclusions; ranges map to original bytes/lines; unchanged refreshes are equivalent; interrupted refresh does not publish partial state; vectors cannot mix incompatible models; search respects requested history/scope; failed migration preserves the previous usable database.
- Use property-based tests for unusual source content, ranges, branch histories, and state transitions, and fault injection for failures at persistence, subprocess, and refresh/migration boundaries where practical. Record which invariants are covered and gaps.
- Confirm fixes for security findings have regression tests. Record gaps and why they do not block, or block release until resolved.

### 6. Build, artifact, and release-channel integrity

**Acceptance criteria**
- Verify the release is built from the reviewed commit using the documented/reproducible release process and intended toolchain; record provenance and artifact identifiers/checksums.
- Inspect packaging contents for unintended files, credentials, test fixtures, debug data, or stale binaries. Confirm permissions and platform-specific artifacts are appropriate.
- Verify signing, checksums, publishing permissions, and release-channel configuration where supported. Confirm only authorized maintainers/workflows can publish and that credentials are appropriately scoped and protected.
- Record any unavailable provenance/signing controls as explicit release risks; a successful build alone does not prove artifact authenticity.

### 7. Secrets, access, and operational readiness

**Acceptance criteria**
- Scan the release diff and packaged artifacts for credentials, tokens, private keys, and sensitive data; investigate matches and rotate/revoke any exposed credential.
- Review relevant CI, repository, publishing, and service permissions/configuration changed for this release using least privilege. Confirm no secrets are printed in logs or error output.
- For security fixes or incidents, confirm disclosure/coordination and user-facing advisories are handled appropriately; identify supported versions needing backports.
- Record security contact/reporting route and operational response/rollback considerations for the release.
- Confirm imported private source content is not unintentionally logged, transmitted to remote embedding services, or exposed through diagnostics; document intentional external data flows and their controls.

### 8. Release security sign-off

**Acceptance criteria**
- Summarize the status and evidence for all review areas; verify required checks are tied to the exact release commit.
- List unresolved findings by severity, affected component, mitigation, owner, due date, and approved exception (if any). Verify exceptions have an expiry/revisit date and are not represented as fixes.
- Confirm no Critical/High finding is unresolved without an explicit authorized exception; identify who authorized it and why release is acceptable.
- Record the release owner's go/no-go decision and date. If evidence is missing, a check failed, or the reviewed commit changed, mark blocked and repeat affected checks against the final commit.

### 9. Correctness, data integrity, and recovery

**Acceptance criteria**
- Trace critical indexing, refresh, search, history, import, and persistence paths as applicable; record invariants, ownership, and errors at transitions.
- Verify repeated operations are deterministic/equivalent where promised; search results match requested scope/history; deduplication and embedding/model identity remain consistent.
- Exercise interruption, subprocess failure, database corruption, transaction rollback, migration failure, and concurrent writes where applicable. Verify no partial snapshot is published and previous usable data remains recoverable after failed migration.
- Link each critical invariant to executable tests. Unproven data-integrity or rollback invariants block release absent an explicit authorized exception.

### 10. Interface, maintainability, and performance contracts

**Acceptance criteria**
- Test successful and unsuccessful public CLI/API interactions: argument validation, stdout/stderr separation, valid/stable JSON, exit status, path handling, cancellation, and predictable diagnostics. Check backward compatibility against documented contracts and supported clients.
- Review changed code and critical paths for excessive branching/fan-out, coupling/cycles, duplicated or conflicting state, hidden assumptions, incomplete state machines, swallowed errors, unchecked conversions, panic paths, dead code, unnecessary allocations, and repeated transformations. Record concrete findings; avoid subjective refactoring-only issues.
- Measure representative workloads including a documented large-repository/input case. Record workload, environment, memory/disk use, latency, repeated work, and any performance budget; compare to a baseline when available. Document deviations and resource-exhaustion behavior.
- Confirm installation, upgrade, rollback/uninstall instructions and user/agent-facing documentation match actual behavior.

## Automated checks and verification gates

Select checks supported by the repository/toolchain and run them against the exact release commit. Do not add tools or dependencies just to satisfy this checklist without following the repository's installation policy. At minimum record the applicable formatting, lint, full test, dependency/advisory, and secret-scan commands with tool versions and outcomes. For Rust projects, consider `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets --all-features`, `cargo audit`, and `cargo deny check`; report unavailable tools as not run, not as passed. Add fuzzing for parsers/range validators/malformed database inputs, Miri for suitable unsafe-sensitive tests, and sanitizers for applicable native/FFI boundaries when practical. These checks complement manual review and tests; none proves safety alone.

The release owner verifies these evidence gates:

| Gate | Pass evidence |
| --- | --- |
| Functional | Specified behavior is covered by passing tests. |
| Security | No unresolved Critical/High vulnerabilities in the stated threat model, except an explicitly authorized, time-bounded exception. |
| Data integrity | Persistence, snapshot, migration, and rollback invariants pass executable tests. |
| Failure handling | Injected/relevant failures leave consistent, recoverable state. |
| Interface | Contract tests pass, including invalid input and structured output. |
| Structure | Material complexity/coupling findings are fixed or specifically justified. |
| Performance | A defined workload and resource budget are measured and met, or deviation is explicitly accepted. |
| Reproducibility | A clean checkout builds and tests; artifacts are tied to the reviewed commit. |

Not every finding must be fixed: low-priority performance or maintainability work may be tracked and deferred with rationale. A failed or unproven critical data-integrity invariant blocks release unless explicitly excepted by the authorized release owner.

## Conditional review areas

Add a section to the versioned review record when the release changes the relevant surface; document the trigger and scope.

- **Authentication, authorization, or access control changed:** test access boundaries, privilege escalation, default-deny behavior, and negative cases.
- **Network-facing behavior or URL fetching changed:** review SSRF, redirects, DNS/IP ranges, TLS validation, schemes, credentials, timeouts, response-size limits, and proxy behavior.
- **Subprocesses, shell commands, or executable discovery changed:** review injection, argument boundaries, environment inheritance, executable/path trust, timeouts, and output handling.
- **Filesystem or Git object access changed:** review traversal/symlinks, repository trust, object/path validation, permission handling, race conditions, and safe write boundaries.
- **Database schema, migrations, or persistence changed:** test untrusted/corrupt/legacy data, migration atomicity and rollback, access controls, integrity, and sensitive-data retention.
- **Cryptography, signing, or secret handling changed:** review algorithms/protocols, key generation/storage/rotation, randomness, verification failures, and secret exposure with an appropriate specialist.
- **Web UI, API, or browser-facing surface changed:** review injection/XSS, CSRF, CORS, content security, authentication/session handling, and API authorization as applicable.
- **New platform, installer, package manager, or distribution channel:** verify platform-specific permissions, artifact provenance, install/upgrade behavior, and uninstall/rollback paths.
- **A security vulnerability was fixed:** verify exploit conditions, regression coverage, affected versions, backports, disclosure timing, and advisory/release-note accuracy.
- **No code change (documentation-only or rebuild):** retain scope, dependency/supply-chain, artifact integrity, and sign-off reviews; justify any check marked not applicable.

## Versioned review record

Create `docs/reviews/security/repodb-<version>.md` from this structure and amend that file throughout the review. Checkboxes represent review work, not GitHub tickets. Keep evidence, findings, and dispositions in the record; the master record is the source of truth for the review.

```markdown
# RepoDB <version> security and release-readiness review

- Status: Not started | In progress | Blocked | Passed with exception | Passed
- Target commit/tag:
- Commit range:
- Release date:
- Supported platforms:
- Release owner:

## Scope and threat model
<changed components, trust boundaries, inputs, exclusions and rationale; THREAT_MODEL.md revision reviewed, changes made, unknown assumptions, and affected threat IDs>

## Review areas
### Security, scope and data flow
- [ ] Status: Not started | In progress | Pass | Fail | N/A
- Scope / owner:
- Evidence (commit, commands/tools/versions, tests, links):
- Findings / rationale:

### Dependencies and supply chain
<same fields>

### Static analysis and security-sensitive code
<same fields>

### Input, parser robustness and resource exhaustion
<same fields>

### Correctness, security tests and data integrity
<same fields>

### Failure recovery
<same fields>

### Interface, maintainability and performance
<same fields>

### Build, artifacts, release hygiene and operations
<same fields>

### Conditional reviews
<repeat a section for each applicable trigger; record N/A and rationale for others>

## Adversarial verification
- Adversarial review agent (model, role, independent context) and target commit:
- Claims challenged and falsification method (source paths, reproductions, commands and results):
- Counterexamples or newly opened findings; fixes and retest evidence:
- Claims not reproduced or verified, with reason and release-gate disposition:

## Findings and exceptions
<finding, severity, reproducible scenario, affected invariant/component, disposition, mitigation, owner, due date; exceptions include approver and expiry>

## Final release gate
- Functional:
- Security:
- Data integrity:
- Failure handling:
- Interface:
- Structure:
- Performance:
- Reproducibility:
- Decision and rationale:
- Release owner / date:
```

When humans choose to transfer this record to GitHub, use one master issue for the release review and make review areas and actionable findings its sub-issues. That issue creation is outside the agent workflow specified here.

## Optional review organization advice

For a large or sensitive release, the release owner may assign independent reviewers for architecture/data-flow and invariants, adversarial security, correctness/testing and fault injection, and interface/compatibility. A separate verifier can re-run checks and confirm fixes. This is advice for dividing work, not a required agent lineup. Keep review independent where possible: reviewers should challenge assumptions rather than merely endorse implementation. Record each actionable finding in the versioned review file with a reproducible scenario, severity, affected invariant, and acceptance test.

## Severity and exceptions

Use the repository's normal triage labels and severity policy if present. Otherwise, classify by plausible impact and exploitability: Critical (catastrophic compromise or broad data/system exposure), High (serious compromise or meaningful privilege/security boundary bypass), Medium (material but constrained impact), Low (limited impact). Record reasoning rather than inferring severity from scanner labels alone.

An exception is not a pass. It must name the unresolved risk, affected release/components, compensating controls, approver, rationale, expiry/revisit date, and tracking record. Do not waive a check silently. Changes after sign-off that affect security scope require re-review of the affected areas and renewed sign-off.
