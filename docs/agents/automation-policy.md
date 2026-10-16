# Automation Policy

- **Scope:** implement accepted RepoDB spec #1 and its tickets only, plus necessary tests and docs.
- **Filesystem:** modify this repository; keep scratch, logs, and temporary Git worktrees in `/tmp`.
- **Preservation:** preserve existing changes and isolate ticket work on separate branches/worktrees.
- **External effects:** use configured GitHub issue workflow only; do not publish, deploy, release, or alter credentials.
- **Execution:** complete every ticket whose dependencies and evidence permit; continue independent work around blockers; close only verified tickets.
- **Parallelism:** use available defaults and modest concurrency; use independent review for difficult decisions.
- **Stopping:** record evidence-based blockers and continue; stop when no useful in-scope work remains or a hard boundary prevents progress.
- **Implementation default:** Rust, as requested; all other unspecified choices follow the accepted spec and conservative repository-compatible defaults.
