# Automation Policy

- **Scope:** implement accepted RepoDB spec #1 and its tickets only, plus necessary tests and docs.
- **Filesystem:** modify this repository; keep scratch, logs, and temporary Git worktrees in `/tmp`. For in-scope dependency installations, allow only the install ledger required by the `un-install` skill under `~/.local/uninstall/` and a repository pointer to it.
- **Preservation:** preserve existing changes and isolate ticket work on separate branches/worktrees.
- **External effects:** use configured GitHub issue workflow only; do not publish, deploy, release, or alter credentials.
- **Execution:** complete every ticket whose dependencies and evidence permit; continue independent work around blockers; close only verified tickets.
- **Parallelism and models:** use modest concurrency; implementation workers use local `polystrata/gemma-4-26b-a4b-ud-q2_k_xl`; oracle consultations use `openai-codex/gpt-6.1-sol`, reserving `openai-codex/gpt-6-astra` for unusually difficult cases.
- **Stopping:** record evidence-based blockers and continue; stop when no useful in-scope work remains or a hard boundary prevents progress.
- **Implementation default:** Rust, as requested; all other unspecified choices follow the accepted spec and conservative repository-compatible defaults.
