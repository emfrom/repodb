---
name: repodb-import
description: Import one explicitly selected URL page, local documentation file or directory, or Git repository into RepoDB.
---

# RepoDB import

Use this skill only when RepoDB is available for the current repository. Availability requires both an executable `repodb` command and an initialized database; do not initialize a database as part of this skill. If either is unavailable, explain what is missing and stop.

Import exactly one selected source per import call: one HTTP(S) URL, one local documentation file or explicitly selected directory, or one Git repository. Do not combine source types or multiple sources in a call. A supplied HTTP(S) URL imports exactly that page. It is one source/call. Do not crawl links or follow site navigation. Bounded crawling is not supported by this workflow. Make another import call only for another independently selected source.

## Local documentation preview and import

The preview flow applies only to local files and directories; `repodb import --preview` does not preview URLs or Git repositories. `--preview` is local-file/directory only.

1. Check applicable repository instructions, including `AGENTS.md`, for import/approval policy. Do not infer an approval requirement if none is stated.
2. Run `repodb import --preview PATH` for the selected local file or directory. Preview is read-only. No imports have been performed until the preview has been shown and the policy decision to proceed has been made.
3. Parse the JSON preview and use it as the sole source of truth for scope and support. Do not independently walk, filter, or reinterpret the path. Respect all preview boundaries, including excluded symlinks and `.git` directories; never traverse outside the selected scope.
4. Before any import, show the complete preview: every entry in `selected_supported_local_files` and every entry in `unsupported_regular_files` with its reason. Report `total_selected_source_bytes`, `current_db_bytes`, `estimated_net_db_growth_bytes` (including null/unknown), `warning_basis_bytes`, and `warn_large`, using the CLI-reported byte units. These totals describe all preview-selected supported files, including preview-supported formats outside this skill's scope; do not present them as an exact size estimate for the documentation files you will import. Unsupported files are excluded from imports. Where useful, offer alternatives such as converting to a supported format or selecting a supported file explicitly.
5. If `warn_large` is true, clearly flag the large-import warning and show `warning_basis_bytes` plus available source, database, and growth sizes. The CLI's warning thresholds are at least 50 MB (50,000,000 bytes) or estimated net growth greater than twice the current database size; when growth is unknown, the warning basis falls back to total selected source bytes. Do not block solely on size and do not require human approval solely because of this warning. Apply any approval required by repository policy; otherwise the agent may decide whether to proceed. Ask the user only for an approval policy requirement or a genuine user-owned decision.
6. If policy requires approval, obtain it before proceeding. If proceeding is approved by policy or the agent's decision where no approval is required, make that decision explicit. If not proceeding, stop without importing or refreshing.

## URL import

After checking applicable policy and deciding to proceed, run `repodb import URL` with exactly the explicitly selected HTTP(S) URL. This imports only that supplied page; do not crawl links, follow site navigation, or add URLs discovered in its content. URL imports do not have a local preview step. Do not promise URL preview size fields or a URL size-warning workflow that the CLI does not provide.

## Git repository import

A Git repository is one source per invocation. Use `repodb import [--revision REV] REPOSITORY`; default is `HEAD`. `repodb import REPOSITORY` selects HEAD, or `repodb import --revision REV REPOSITORY` selects one branch, tag, or commit. The `--revision` option precedes the repository argument. RepoDB resolves the selected revision to a resolved commit SHA and pins the imported snapshot to that commit; report the SHA as its version/provenance. Do not import full history. A later refresh must continue using the resolved commit for that source, even if a branch or tag moves. Do not combine a Git repository with another source in one invocation.

## Local documentation import scope

Local imports accept an explicitly selected file or directory; directory recursion is limited to eligible files within the selected directory according to the preview. Supported documentation formats are Markdown, plain-text, PDF, HTML, or Org formats, plus extensionless files: Markdown (`.md`, `.markdown`), plain text (`.txt`), PDF, HTML, and Org mode (`.org`). The CLI supports `.rs` files for backward compatibility, but source-code files are outside this documentation skill's scope. Use the preview as the source of truth for enumeration and CLI support; when it lists `.rs`, report it as preview-supported but out of skill scope and do not import it. Never expand selection to a parent, sibling, workspace, or another directory.

## Import, refresh, and report

For local documentation, after the complete preview and proceed decision, import only selected paths returned by the preview that are within this skill's documentation scope. Invoke `repodb import FILE` separately once for each in-scope path; never pass a batch or directory as an import, and never invoke imports for unsupported or out-of-scope paths. For URL or Git sources, use the corresponding single-source command above; these flows do not use local preview.

After successful import registration, run one explicit `repodb refresh --imports` to index the registered sources without refreshing repository history. Do not initialize or refresh before imports, and do not imply that an import succeeded when its command failed. Report which imports succeeded or failed, whether import refresh succeeded, and only the source provenance the CLI actually provides. For a URL, the URL is the source locator. JSON search results expose the imported version as `blob_id`, which is the SHA-256 of fetched response-body bytes, plus `first_retrieved_at` and `last_retrieved_at` in UTC RFC3339 (`Z`) format. Legacy snapshots whose observation time is unknown report null timestamps. Search is offline and never fetches; only explicit full or `refresh --imports` refresh observes registered URLs. `repodb retention latest` keeps the most recently successfully published version (including A→B→A returning A as latest); `all` keeps distinct hashes and deduplicates unchanged bytes. The default remains `latest`. URL preview and URL growth-warning are unavailable; local preview size fields apply only to local file/directory selections. If an import or import refresh fails, report the error honestly and any resulting partial completion.
