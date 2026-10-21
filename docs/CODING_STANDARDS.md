# AGENT_QUALITY_RULES.md

**For running implementers and reviewers.** Apply alongside the task specification and `CODING_STANDARDS.md`. This is an operational checklist, not permission to bypass the full standard.

## Non-negotiable rules

1. **Correctness first:** Preserve existing behaviour, public interfaces, persistent formats, and data invariants unless the task explicitly changes them.
2. **Minimal coherent changes:** No speculative architecture, unrelated refactors, unnecessary wrappers, duplicated state, or hidden side effects.
3. **Explicit contracts:** Know each changed operation's inputs, outputs, errors, ownership, side effects, and relevant invariants.
4. **Safe failure:** Never swallow errors, leak private data, or expose partially committed persistent state. Validate untrusted inputs and bound/resource-review external operations.
5. **Evidence over assertion:** Do not claim checks passed unless actually executed. Do not remove/weaken tests or suppress warnings to conceal problems.

## Complexity review triggers (soft limits)

| Metric | Trigger |
|---|---:|
| Cyclomatic / cognitive complexity | >10 / >15 |
| Function length | >60 logical lines |
| Call depth within subsystem | >3 |
| Distinct direct callees | >5 |
| Parameters / nesting | >5 / >3 |

Review *total dependency-graph complexity*, not just depth. Do not game thresholds by splitting functions, introducing forwarding wrappers, or flattening good abstractions. Document justified exceptions.

## Implementer procedure

1. **Before editing:** Read spec, affected code, contracts, data flows, existing tests; identify the smallest change and its risk boundaries.
2. **During editing:** Keep state ownership explicit, errors meaningful, interfaces stable, modules cohesive, and code locally understandable.
3. **Test:** Add regression/behaviour tests for changes; exercise boundaries and failure paths. For critical data/parser logic, consider property, fuzz, mutation, and fault-injection tests.
4. **Verify:** Run configured formatting, lint, build, and relevant test commands. Check persistence rollback, CLI/JSON/exit-status compatibility, security/resource boundaries as applicable.
5. **Self-review:** Inspect the diff for accidental edits, excessive call chains/fan-out, hidden state, broken invariants, and missing tests.
6. **Report:** State changes, invariants, actual commands/results, skipped checks, deviations, and residual risks.

## Reviewer procedure

1. **Independently reconstruct behaviour** from source and tests. Do not rely solely on the implementer's summary.
2. **Trace one representative end-to-end data path:** input → validation → transformation → state/persistence → output and failure handling.
3. **Check correctness:** Contracts, invariants, partial failure/rollback, error propagation, edge cases, races where relevant, and regressions.
4. **Check design:** Cohesion, coupling, ownership, duplication, change locality, call-graph complexity, and avoidable abstraction layers.
5. **Check attack/resource boundaries:** Paths, external data/URLs, subprocesses, parsers, secrets, unbounded CPU/memory/time, and unsafe/FFI code where relevant.
6. **Check test strength:** Would tests catch plausible defects? Seek evidence beyond coverage; use mutations/properties/fault injection for high-risk code.
7. **Report each finding** with location, concrete evidence/reproduction, impact, violated invariant/contract, severity, and acceptance criterion. Distinguish verified defects from hypotheses.

## Acceptance / stop conditions

**Block acceptance for:** failed required checks; unsatisfied task requirements; security or data-integrity defects; unsupported interface breaks; unexplained unsafe/undefined behaviour; unresolved critical/high findings under the project's threat model.

**Request justification for:** exceeded soft complexity thresholds, unusual coupling, broad diffs, reduced test strength, or performance regressions.

**Done means:** requirements fulfilled, changed invariants verified, relevant checks actually passed, no release blockers, and known exceptions/limitations recorded.

### Minimal completion report

```text
Change:
Contracts/invariants affected:
Checks executed (commands + results):
Checks skipped (reason):
Review findings / exceptions:
Remaining risks:
Verdict: PASS | BLOCKED | NEEDS REVIEW
```
