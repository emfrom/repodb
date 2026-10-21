# Coding-standards release review

This specification is for the release-review suite. The short [`docs/CODING_STANDARDS.md`](../CODING_STANDARDS.md) is an operational checklist for implementation agents; the normative full standard is [`docs/CODING_STANDARDS_FULL.md`](../CODING_STANDARDS_FULL.md). For every release subject to the cadence in [`README.md`](README.md)—before 1.0.0, each major/minor version increase (including 1.0.0), and after 1.0.0, every release—review the full standard end-to-end and create or amend `docs/reviews/standards/repodb-<version>.md` (for example, `docs/reviews/standards/repodb-0.4.0.md`). Ensure the matching master standards-review issue and review-area sub-issues exist, creating only missing tickets; follow [`README.md`](README.md) for structure and deduplication. The versioned file is the authoritative review output and must represent each applicable check below as a distinct checklist section beneath one master standards review record. Reviewers append their results to that file and do not create separate tickets for findings.

Record evidence, not conclusions alone. Tie commands, code review, tests, and findings to the exact release commit. Mark checks `PASS`, `FAIL`, `N/A` (with reason), or `NOT RUN`; never mark unrun checks passed. A check may cover multiple requirements, but each distinct check below needs its own recorded disposition and evidence pointer. The full standard remains authoritative if this summary and the standard differ.

## Review procedure

1. Identify release version, target commit/tag, commit range, date, supported platforms, owner, and changed components. Record these in the master report.
2. Read the complete full standard and inspect the release diff, relevant source/tests/configuration, and existing contracts independently; do not rely solely on the implementer's summary.
3. Copy the template below to `docs/reviews/standards/repodb-<version>.md`. Retain every review area. Mark conditional checks N/A only with a specific rationale.
4. Run applicable repository-configured tooling and tests. Record exact commands, versions, environment, commit, and outcomes. Tool unavailability is `NOT RUN`, not a pass.
5. Record findings with location, reproducible evidence, impact/severity, violated contract/invariant, and testable acceptance criterion. Do not make source changes as part of this review unless separately tasked.
6. The release owner verifies the gates and records the verdict. Hard-gate failures block release unless an authorized, explicit, time-bounded exception is recorded with mitigation and approver.

## Master report and sub-ticket template

Use one master review record and these sections as its review-area work items. They are checklist sections in the Markdown output; the corresponding GitHub review-area sub-issues are tracked separately, as described in [`README.md`](README.md).

```markdown
# RepoDB <version> coding-standards release review

- Status: NOT STARTED | IN PROGRESS | BLOCKED | PASS WITH EXCEPTION | PASS
- Target commit/tag:
- Commit range:
- Release date:
- Supported platforms:
- Review owner:
- Release owner:

## Scope and evidence index
<changed components, exclusions with rationale, environment, links to evidence>

## Sub-review 1 — Precedence and fundamental principles
- Status: PASS | FAIL | N/A | NOT RUN
- Evidence:
- Findings / rationale:

## Sub-review 2 — Structure and complexity
<same fields>

## Sub-review 3 — Interfaces and contracts
<same fields>

## Sub-review 4 — State, data flow, and persistence
<same fields>

## Sub-review 5 — Security and resource safety
<same fields>

## Sub-review 6 — Behavioural verification
<same fields>

## Sub-review 7 — Architecture
<same fields>

## Sub-review 8 — Agent comprehensibility and reasoning cost
<same fields>

## Sub-review 9 — Tooling baselines
<same fields>

## Sub-review 10 — Quality gates
<same fields>

## Sub-review 11 — Independent comprehension review
<same fields>

## Sub-review 12 — Agent change workflow and reporting
<same fields>

## Sub-review 13 — Independent release audit and findings
<same fields>

## Sub-review 14 — Definition of done / release readiness
<same fields>

## Sub-review 15 — Prohibited practices and final principle
<same fields>

## Findings
<finding ID, location, evidence/reproduction, severity/impact, violated contract/invariant, acceptance test, disposition, owner and due date>

## Hard release gates
- Supported builds:
- Configured formatting/static checks:
- Relevant tests:
- Security/data invariants and public contracts:
- Release-blocking findings:
- Unsafe/undefined behavior:
- Clean-checkout reproducibility:
- Supported platforms:
- Dependency/security review:
- Recovery, interface, performance, limitations:

## Final verdict
<PASS | BLOCKED | PASS WITH EXCEPTION, rationale, approver, date, exceptions and expiry>
```

## Review criteria

### Sub-review 1 — Precedence and fundamental principles

- Verify correctness, testability, safe failure, and comprehensibility are addressed, and that priorities are respected: correctness/integrity, security/safe failure, contract compatibility, maintainability, testability, performance, style.
- Verify established behavior is preserved unless the release intentionally changes it. Any exception to a MUST is explicit and approved; SHOULD deviations have reasons; soft numerical limits trigger review rather than automatic rewrites.

### Sub-review 2 — Structure and complexity

Review changed code and relevant dependency graphs, including indirect calls, recursion, traits/callbacks/function pointers. Record metrics or reasoned assessments for:

- Cyclomatic complexity ≤10/function; cognitive complexity ≤15/function.
- Function length ≤60 logical lines; call-chain depth ≤3 within a subsystem; direct fan-out ≤5 distinct callees.
- Parameters ≤5; control-flow nesting ≤3 levels.
- Significant duplication, unjustified dead code, cohesion, external dependencies, public API size, dependency cycles, state ownership, and change locality.
- Ensure thresholds have not been gamed with wrappers, mechanical splitting, or flattened useful abstractions. Explain material deviations; unrelated cross-module edits receive architectural review.

### Sub-review 3 — Interfaces and contracts

- Verify each changed public operation defines/encodes valid inputs, outputs, side effects, error semantics, pre/postconditions, and ownership/lifetime constraints.
- For CLI/machine interfaces, test argument behavior, stdout/stderr separation, structured-output schema, exit statuses, error behavior, and compatibility. Ensure human diagnostics do not pollute machine-readable stdout.
- Verify contracts are preserved unless breaking changes are intentional and documented.
- Check that errors preserve actionable context, distinguish recoverable errors from invariant violations, avoid panic/exit on normal recoverable errors, and do not expose secrets.

### Sub-review 4 — State, data flow, and persistence

- Trace major operations end-to-end: input → validation → transformations → state changes → persistent writes → observable output. At each boundary check representation, ownership, invariants, errors, resource use, and consistency.
- Identify the authoritative owner of mutable state; review global/shared mutation, duplicate representations, and transitions for inconsistent or partial states.
- Verify persistent operations use appropriate atomic writes/transactions, define cancellation/interruption/rollback, detect incompatible schema/data forms, and preserve prior usable state on failure.
- Check idempotence where promised and confirm failed operations do not publish a partial logical result unless explicitly documented.

### Sub-review 5 — Security and resource safety

- Treat external files, Git data, URLs, subprocess output, and imported content as untrusted unless a real trust boundary guarantees otherwise.
- Review path traversal, symlinks, command argument separation/shell evaluation, redirects/SSRF where applicable, malicious parsers, malformed data, database inputs, and unintended private-content leakage. Prefer argument arrays over shell-composed commands.
- Document or bound memory, CPU, execution time, file size, concurrency, temporary storage, and cancellation behavior; establish that unbounded operations are deliberate and tested.
- For unsafe Rust, C pointer manipulation, and FFI, verify explicit lifetime, bounds, alignment, aliasing, ownership, cleanup, and concurrency invariants; justify each unsafe use and remove unnecessary unsafe code.

### Sub-review 6 — Behavioural verification

- Confirm every substantive behavior change has relevant tests; tests should be deterministic, isolated where possible, and focused on observable behavior. Every confirmed bug has a regression test.
- Select applicable unit, integration, regression, boundary, property-based, failure-injection, persistence/recovery, interface-contract, concurrency, and performance/regression tests.
- Review line and branch coverage without treating coverage as correctness; critical security/integrity paths have direct behavioral verification. Coverage targets are risk- and baseline-based.
- For key invariants, use mutation testing and review surviving mutations. Fuzz complex parsers/untrusted input; generate ranges, offsets, transformations, idempotence, serialization, and state-transition cases. Avoid assertions that only inflate scores.

### Sub-review 7 — Architecture

Assess cohesion, coupling, dependency direction, authoritative representations, explicit state ownership, failure semantics, abstraction usefulness, public API size, change locality, and test isolation. Confirm architectural changes solve a concrete problem rather than hypothetical future needs and exclude unrelated refactoring.

### Sub-review 8 — Agent comprehensibility and reasoning cost

For complex areas, determine whether a fresh agent can locate an operation, explain contracts/invariants, trace outputs and failures, and identify the correct change point. Record relevant evidence: files/functions read, dependency-subgraph size, dynamic-dispatch edges, hidden assumptions, and unrelated modules modified per logical change. Where measured, note context tokens, searches, tool calls, correction attempts, and regression rate as experimental signals, not standalone quality measures.

### Sub-review 9 — Tooling baselines

Run applicable configured checks against the release commit and record exact commands, tool versions, platform, and results. Rust baseline to consider:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo audit
cargo deny check
```

Where appropriate and available, consider `cargo-llvm-cov` (coverage), `cargo-mutants` (mutation), `proptest`, `cargo-fuzz`, `cargo-semver-checks`, Miri, and sanitizers. For C, consider strong compiler warnings, clang-tidy/cppcheck, relevant sanitizers, coverage, and static complexity/call-graph analysis. Tool configuration should be committed. Record unavailable checks as skipped/not run, not passed. Do not silence warnings merely to pass CI or install tools without following project installation policy.

### Sub-review 10 — Quality gates

Verify hard gates: supported builds; configured formatting/static checks; relevant tests; preservation of security/data invariants and public contracts; no unresolved release-blocking finding; no unexplained unsafe/undefined behavior in changed code.

Review soft gates: function length, complexity, depth/fan-out, nesting, parameter count, coupling, duplication, coverage changes, and performance regressions. Deviations require reasoning, not mechanical edits. Consider trends in complexity/dependencies, mutation score, coverage, change amplification, agent comprehension/tool cost, and defect frequency.

### Sub-review 11 — Independent comprehension review

For major subsystem changes, obtain an independent review where justified. The reviewer should not rely on the author's description; have them explain behavior/invariants, trace an operation end-to-end, identify error states, locate a hypothetical change point, and predict regressions. Validate conclusions against tests or execution where possible. Treat poor comprehension as a design/documentation concern, not automatic rejection.

### Sub-review 12 — Agent change workflow and reporting

Verify the work followed the standard's phases:

- **Understand:** task/source read; contracts, state, data flow, tests and minimal scope identified.
- **Implement:** smallest coherent change; interfaces preserved; invariants/errors encoded; tests updated; intentional changes documented.
- **Verify:** relevant tests/static checks run; boundaries/failures exercised; call graph, coupling and diff reviewed; no unrun check claimed passed.
- **Review:** unrelated edits, hidden effects, swallowed errors, unsafe assumptions, excess abstractions, missing tests and compatibility breaks checked.
- **Report:** changes, invariants, exact commands/results, skipped checks, deviations and residual risks reported; evidence distinguished from assumptions.

### Sub-review 13 — Independent release audit and findings

For significant changes, consider separate architecture/data-flow, security, correctness/adversarial-testing, interface/compatibility, and release-verification reviews. Every finding should identify location, evidence/reproduction, impact/severity, violated contract/invariant, and testable acceptance criteria. Verify an implementer does not self-certify a previously disputed finding without independent evidence.

### Sub-review 14 — Definition of done and release readiness

Confirm requirements/contracts are met, critical invariants remain valid, relevant checks pass, the diff is reviewed, and risks/deviations are documented. Release review additionally covers clean-checkout reproducibility, supported platforms, security/dependencies, data recovery, public interfaces, relevant performance, and known limitations. Automated green status is necessary where configured but is insufficient alone.

### Sub-review 15 — Prohibited practices and final principle

Check that no agent claimed unexecuted tests passed; deleted/weakened tests to hide failures; suppressed diagnostics without cause; gamed metrics with wrappers/superficial assertions; changed unrelated modules without reason; ignored failed subprocesses/writes; silently changed public formats; equated coverage with correctness; or closed findings without evidence. Apply the final principle: prefer the simplest implementation whose correctness, invariants, failure handling, and maintainability can be independently verified.

## Verdict and exceptions

Unresolved hard-gate failures block release. Findings do not all require fixes: lower-priority items can be documented and deferred when justified. Exceptions must identify the specific unmet requirement, affected release/code, rationale, compensating controls, approver, owner, and expiry/revisit date. An exception is not a pass and must not conceal an unrun test or unverified invariant.
