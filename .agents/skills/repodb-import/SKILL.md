---
name: repodb-import
description: Preview and import one explicitly selected local documentation file or directory into RepoDB.
---

# RepoDB local documentation import

Use this skill only when RepoDB is available for the current repository. Availability requires both an executable `repodb` command and an initialized database; do not initialize a database as part of this skill. If either is unavailable, explain what is missing and stop.

Import exactly one explicitly selected local file or one explicitly selected directory per skill invocation. A directory selection means its recursively eligible files according to RepoDB's preview; never expand selection to a parent, sibling, workspace, or other directory. Do not discover or add extra inputs yourself.

## Preview before mutation

1. Check applicable repository instructions, including `AGENTS.md`, for import/approval policy. Do not infer an approval requirement if none is stated.
2. Run `repodb import --preview PATH` for the selected file or directory. Preview is read-only. No imports have been performed until the preview has been shown and the policy decision to proceed has been made.
3. Parse the JSON preview and use it as the sole source of truth for scope and support. Do not independently walk, filter, or reinterpret the path. Respect all preview boundaries, including excluded symlinks and `.git` directories; never traverse outside the selected scope.
4. Before any import, show the complete preview: every selected supported file and every unsupported path with its reason. Show `source_bytes`, `database_bytes` (including that it is unknown when null/absent), `estimated_growth_bytes` (including unknown/null), `warning_basis_bytes`, and `warn_large`. Explain the byte units as reported by the CLI; do not invent missing estimates. Unsupported files are excluded from imports. Where useful, offer alternatives such as converting to a supported format or selecting a supported file explicitly.
5. If `warn_large` is true, clearly flag the large-import warning and show the reported basis and available source/database/growth sizes. The CLI's warning thresholds are at least 50 MiB or estimated growth greater than twice the current database size; when growth is unknown, the warning basis falls back to source bytes. Do not block solely on size and do not require human approval solely because of this warning. Apply any approval required by repository policy; otherwise the agent may decide whether to proceed. Ask the user only for an approval policy requirement or a genuine user-owned decision.
6. If policy requires approval, obtain it before proceeding. If proceeding is approved by policy or the agent's decision where no approval is required, make that decision explicit. If not proceeding, stop without importing or refreshing.

## Import and report

After the preview and proceed decision, import only the supported selected paths returned by that preview. Invoke `repodb import FILE` separately once for each path; never pass a batch or directory as an import, and never invoke imports for unsupported paths. Do not import anything before the complete preview and decision.

After the per-file import attempts, run exactly one explicit `repodb refresh` to index the newly registered sources. Do not initialize, refresh before imports, or imply that an import succeeded when its command failed. Report which selected paths succeeded or failed, whether refresh succeeded, and the source provenance the CLI actually provides. Do not invent provenance or claim unsupported files were indexed. If an import or refresh fails, report the error honestly and any resulting partial completion.

## Scope of this skill

This skill is only for local documentation files and selected directories. It does not authorize importing a URL, Git repository, or any other source. The preview may include Markdown, text, PDF, HTML/HTM, Org, extensionless files, or `.rs` files; rely on the CLI preview's supported/unsupported classification rather than applying an extension allowlist yourself. One explicitly selected source scope is the limit for each invocation; run another invocation only for another independently selected source.
