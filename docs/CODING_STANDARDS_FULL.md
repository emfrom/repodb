# CODING_STANDARDS.md

**Status:** Normative  
**Scope:** Local applications written in Rust or C  
**Audience:** Autonomous coding agents and reviewers  
**Purpose:** Correctness, security, architectural simplicity, testability, and low reasoning cost.

## 1. Principles and precedence

Code MUST be correct, testable, safe on failure, and understandable without excessive context traversal. Prefer explicit interfaces and state ownership; avoid unnecessary abstraction, indirection, shared state, and speculative generalization. Preserve established behaviour unless a change is required.

Priority: **correctness and integrity > security and safe failure > contract compatibility > maintainability > testability > performance > style**. Do not optimize a lower priority at the expense of a higher one.

**MUST** is mandatory (exceptions require explicit approval); **SHOULD** is the default (exceptions require a reason); **MAY** is optional. Soft numerical limits trigger review, not mechanical rewrites.

## 2. Structure and complexity

| Metric | Suggested threshold | Policy |
|---|---:|---|
| Cyclomatic complexity | ≤ 10/function | Soft |
| Cognitive complexity | ≤ 15/function | Soft |
| Function length | ≤ 60 logical lines | Soft |
| Call-chain depth | ≤ 3 within a subsystem | Soft |
| Direct fan-out | ≤ 5 distinct callees | Soft |
| Parameters | ≤ 5 | Soft |
| Control-flow nesting | ≤ 3 levels | Soft |
| Significant duplication | Avoid | Review |
| Dead code | None without justification | Required |

Do not split functions mechanically to satisfy metrics, hide complexity behind wrappers, or flatten useful abstractions just to reduce call depth. Judge the entire relevant dependency graph, including indirect calls, recursion, traits/callbacks/function pointers, and not merely its depth.

Modules SHOULD have cohesive responsibilities, few external dependencies, minimal public API, no inappropriate dependency cycles, and clear mutable-state ownership. A small behavioural change SHOULD be local. Unexpected edits across unrelated modules require architectural review.

## 3. Interfaces and contracts

Public interfaces MUST define or encode valid inputs, outputs, observable side effects, error semantics, preconditions/postconditions, and relevant ownership/lifetime requirements. Types, tests, and documentation can jointly establish contracts; avoid redundant boilerplate.

CLIs and machine-facing interfaces MUST specify argument behaviour, stdout/stderr separation, structured output schemas, exit statuses, error handling, and compatibility expectations. Do not mix unsolicited human diagnostics into machine-readable stdout. Preserve contracts unless a breaking change is intentional and documented.

Errors MUST not be silently swallowed. Preserve actionable context, distinguish recoverable failures from violated invariants, do not panic/exit for normal recoverable errors, and do not expose secrets in diagnostics.

## 4. State, data flow, and persistence

For major operations, be able to trace input → validation → transformations → state changes → persistent writes → observable output. At each boundary review validation, representation, ownership, invariants, errors, resource use, and consistency.

Mutable state MUST have a clearly identified authoritative owner. Minimize global/shared mutation and duplicate representations. Maintain invariants throughout state transitions and avoid exposing partially initialized or inconsistent state.

Persistent operations MUST maintain database/filesystem integrity, use atomic writes or transactions as appropriate, define cancellation/interruption/rollback behaviour, and detect incompatible schema/data representations. Repeated operations MUST be safe where idempotence is promised. Failed operations MUST NOT publish a partial logical result unless explicitly documented.

## 5. Security and resource safety

Treat external files, Git data, URLs, subprocess output, and imported content as untrusted unless an actual trust boundary guarantees otherwise. Review path traversal, symlinks, command argument separation and shell evaluation, network redirects/SSRF where applicable, malicious parsers, malformed data, database inputs, and accidental leakage of private content. Prefer argument arrays to shell-composed commands.

Document or bound potentially unbounded memory, CPU, execution time, file size, concurrency, temporary storage, and cancellation behaviour. Unbounded operation must be deliberate.

Unsafe Rust, C pointer manipulation, and FFI MUST have explicit safety invariants covering lifetime, bounds, alignment, aliases, ownership, cleanup, and concurrency. Remove unnecessary unsafe code.

## 6. Behavioural verification

Every substantive behaviour change MUST have relevant tests. Tests SHOULD verify externally observable behaviour, remain deterministic, and be isolated where possible. Every confirmed bug SHOULD gain a regression test.

Select relevant tests from: unit, integration, regression, boundaries, property-based, failure injection, persistence/recovery, interface-contract, concurrency, and performance/regression testing. Not every category applies to every change.

Measure line and branch coverage, but do not equate coverage with correctness. Critical security/integrity paths require direct behavioural verification. Project-specific coverage targets SHOULD be based on baselines and risk.

Mutation testing SHOULD be used for key invariants: examine surviving mutations for equivalent behaviour or weak tests. Fuzz complex parsers/untrusted inputs. Use generated tests for ranges, offsets, transformations, idempotence, serialization, and state transitions. Do not add meaningless assertions to inflate scores.

## 7. Architectural review

Evaluate cohesion, coupling, dependency direction, authoritative representations, explicit state ownership, failure semantics, abstraction usefulness, public API size, change locality, and test isolation. Architectural changes MUST solve a concrete problem, not anticipate hypothetical future needs. Avoid unrelated refactoring.

## 8. Agent comprehensibility and reasoning cost

Optimize for a bounded, coherent code context. A fresh agent SHOULD be able to locate an operation, explain its contracts and invariants, trace its output and failure modes, and identify the correct change point. When assessing complex areas, note the number of files/functions read, dependency-subgraph size, dynamic dispatch edges, hidden assumptions, and unrelated modules modified per logical change.

Where measured, track context tokens, searches, tool calls, attempts to correct completion, and regression rate. These are experimental signals, not independent definitions of code quality.

## 9. Tooling baselines

Typical Rust checks, adapted to repository configuration:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo audit
cargo deny check
```

Use `cargo-llvm-cov` (coverage), `cargo-mutants` (mutation), `proptest`, `cargo-fuzz`, `cargo-semver-checks`, Miri and sanitizers when appropriate and available. An unavailable check must be recorded as skipped, not passed. For C use strong compiler warnings, clang-tidy/cppcheck, relevant sanitizers, coverage tooling, and static complexity/call-graph analysis. Tool configuration SHOULD be committed. Do not silence warnings merely to pass CI.

## 10. Quality gates

**Hard gates:** supported builds; configured formatting/static checks; relevant tests; preservation of security/data invariants and public contracts; no unresolved release-blocking findings; no unexplained unsafe/undefined behaviour in changed code.

**Soft review gates:** function length, complexity, depth/fan-out, nesting, parameter count, coupling, duplication, coverage changes, and performance regressions. Deviations require reasoning, not mechanical edits.

**Trends:** complexity and dependency growth, mutation score, coverage, change amplification, agent comprehension/tool cost, and defect frequency.

## 11. Independent comprehension review

For major subsystem work, a separate reviewer SHOULD inspect code, contracts, and tests without relying on the author's description. It SHOULD explain the behaviour/invariants, trace an operation end-to-end, identify error states, locate a hypothetical change, and predict regressions. Validate conclusions against tests or execution where possible. Poor comprehension prompts design/documentation investigation, not automatic rejection.

## 12. Agent change workflow

**Understand:** read task and relevant source; identify contracts, affected state, data flow, and existing tests; select minimal scope.

**Implement:** make the smallest coherent change; preserve interfaces; encode invariants and errors; add/update tests; document intentional changes.

**Verify:** run relevant tests and available static checks; test boundaries/failure paths; review call graph, coupling, and diff. Do not claim unrun checks passed.

**Review:** look for unrelated edits, hidden effects, swallowed errors, unsafe assumptions, excess abstractions, missing tests, and compatibility breaks.

**Report:** summarize change, affected invariants, exact executed checks/results, skipped checks, deviations, and remaining risks. Separate evidence from assumptions.

## 13. Independent release audit

Use separate reviewers where justified: architecture/data flow, security, correctness/adversarial tests, interface/compatibility, and release verification. Every finding SHOULD identify location, evidence/reproduction, impact/severity, violated contract/invariant, and testable acceptance criteria. The implementer MUST NOT self-certify a previously disputed finding without independent evidence.

## 14. Definition of done

A task is done when requirements and contracts are met, critical invariants remain valid, relevant checks pass, the diff is reviewed, and remaining risks/deviations are documented.

A release also needs clean-checkout reproducibility, supported-platform verification, security/dependency review, data recovery checks, public interface checks, relevant performance testing, and known-limitation documentation.

Automated green status is necessary where checks are configured, but insufficient on its own.

## 15. Prohibited agent behaviour

Agents MUST NOT claim unexecuted tests pass; delete or weaken tests to hide failures; suppress diagnostics without cause; game metrics by wrappers or superficial assertions; modify unrelated modules without reason; ignore failed subprocesses or writes; silently change public formats; equate coverage with correctness; or close findings without evidence.

**Final principle:** Prefer the simplest implementation whose correctness, invariants, failure handling, and maintainability can be independently verified.
