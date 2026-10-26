# RepoDB threat model

Living model for releases subject to security review. Review and update this file for each such release; record the reviewed revision and release-specific evidence in `security/repodb-<version>.md`. This model describes assumptions, not proof that a control works.

## Review history

| Release | Candidate commit | Model changes or “no change” rationale | Reviewer/date |
| --- | --- | --- | --- |
| <version> | <commit> | <changed threats, assumptions, controls, or explicit no-change rationale> | <name/date> |

## Deployment and trust assumptions

- Invocation and caller: <local CLI, automation, delegated caller, service; who can initiate imports/search/refresh?>
- Privileges and isolation: <OS user, filesystem permissions, network egress, resource limits; evidence for controls relied upon>
- Supported environment: <platforms, installed external programs, embedding provider, distribution channel>
- Unknowns: <unverified deployment assumptions and their effect on risk>

## Assets and security objectives

| Asset | Required property | Consequence of failure |
| --- | --- | --- |
| Indexed source and search results | Integrity; requested history/scope respected | <impact> |
| Source files, Git objects, imported content, database | Confidentiality and intended access | <impact> |
| Published snapshot and prior usable database | Atomic publication and recovery | <impact> |
| Host resources and network access | Availability and authorized destinations | <impact> |
| Release artifacts and credentials | Authenticity and secrecy | <impact> |

## Boundaries and entry points

For each applicable operation, trace input → validation/read → transformation or subprocess → persistence → output. Identify who controls the input, the authority the operation gains, the limits, and the failure state.

| Entry point | Attacker-controlled input | Boundary crossed | Existing control and evidence | Unknown or residual risk |
| --- | --- | --- | --- | --- |
| CLI/config/environment | <...> | <...> | <...> | <...> |
| Local files and Git repositories/objects | <...> | <...> | <...> | <...> |
| URL imports and redirects | <...> | <...> | <...> | <...> |
| Converters, Git, and embedding subprocesses/services | <...> | <...> | <...> | <...> |
| SQLite data, migrations, and refresh publication | <...> | <...> | <...> | <...> |
| CLI/JSON output and release artifacts | <...> | <...> | <...> | <...> |

## Threat register

Keep stable IDs across releases. Link release-specific tests, findings, exceptions, and dispositions from the versioned review record rather than duplicating their results here.

| ID | Scenario and attacker capability | Affected asset/invariant | Expected control | Assumption or open question |
| --- | --- | --- | --- | --- |
| TM-1 | Oversized or hostile import exhausts memory, disk, CPU, or time | Availability; recoverable prior state | Documented limits, controlled failure, recovery | Who can initiate imports? What isolation exists? |
| TM-2 | URL or redirect reaches an unintended internal destination | Authorized network access; confidentiality | Defined egress policy enforced for each destination, or enforced trusted-caller boundary | Are URLs accepted from less-trusted callers? |
| TM-3 | Malformed Git/object/parser/subprocess output causes unsafe behavior or partial state | Integrity; availability | Input validation, bounded work, atomic publication | Which external inputs and programs are trusted? |
| TM-4 | Interrupted refresh or migration publishes inconsistent data | Prior usable database; search integrity | Tested transaction/recovery invariants | Which metadata is publication state versus attempt state? |

Add or retire scenarios as the product changes; retain the rationale and release in the review history. A scenario is not closed merely because a tool reports no findings.
