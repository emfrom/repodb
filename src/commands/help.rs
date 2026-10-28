use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const GENERAL_HELP: &str = r#"RepoDB — local repository and source search

Usage:
  repodb --version
  repodb [--database PATH] <COMMAND> [OPTIONS]
  repodb [--database PATH] help [COMMAND]

Commands:
  init                 Initialize the index and capture the checked-out repository
  refresh              Refresh indexed repository and registered imports
  import SOURCE        Register one local file, URL page, or Git repository
  retention latest|N   Set imported-source version retention (default: latest)
  redirect-limit N     Set the maximum followed URL redirects (default: 5; max: 20)
  reembed              Rebuild vectors with an embedding command/model
  search QUERY         Search indexed content, diffs, and commit messages
  status [--json]      Report index freshness and capability readiness

Global options:
  --database PATH      Use a separate database (default: <git-dir>/repodb.sqlite)
  --version            Print the application version and exit
  -h, --help           Show this help or help for a command

Search defaults to checked-out HEAD and combined matching. Coarse options include:
  --history=off|all|N|PATH   Select the committed view (default: off; bare --history means all)
  --exact | --semantic | --combined  Choose exact-only, semantic-only, or combined matching (default: combined)
  --type and --scope          Select evidence components and source origins
  --worktree                 Opt into live inspection at the active indexed HEAD
  --current-worktree         Compatibility name; remains HEAD-only
  --explore                  Page through all eligible references without text
  --limit N                  Results per page (default: 5; 50 with --explore)
  --byte-budget N            Serialized response budget (default: 5000)
  --cursor CURSOR            Continue a snapshot; --fetch-reference ID retrieves omitted text
See `repodb search --help` for full option details.

Refresh size policy is `--size-limit=error|warn|ignore` (default: error) and
applies to local/URL document imports, not Git content. See `repodb refresh --help`.

Imports are explicit and single-source: a URL import fetches only that page and
never crawls linked pages. Search never fetches or refreshes; refreshing
registered URLs is explicit. See `repodb import --help` and `repodb refresh --help`.

Examples:
  repodb init
  repodb search 'literal text' --json
  repodb search 'literal text' --exact --json
  repodb search 'why is this implemented?' --semantic --history=all
  repodb search 'literal text' --explore --history=all --json
  repodb import --preview https://example.com/page
  repodb status --json
"#;

const SEARCH_HELP: &str = r#"Search indexed content, historical diffs, commit messages, and imported sources.

Usage:
  repodb [--database PATH] search QUERY [OPTIONS]

Matching and evidence (default: combined):
  --exact                    Exact-only matching
  --semantic                 Semantic-only matching by embedding similarity
  --combined                 Combine semantic candidates with exact matches
  --semantic-limit N         Semantic candidate limit (default: 20)
  --type SET                 all|content|cmdiffs|diffs|cmcommits|commits (default: all)
                             all=content+diffs+messages; content=content+diffs;
                             cmdiffs=messages+diffs; cmcommits=messages+content;
                             diffs=diffs only; commits=messages only
  --scope SCOPE              source|repo-local-source|repo-local-docs|all-docs|all
                             (default: all; source includes imported Git sources;
                             repo-local-* restricts to this repository; all-docs
                             includes indexed docs from all origins)

History selection:
  --history=off|all|N|PATH   Select HEAD, all indexed history, N topological commits,
                             or one path and its detected rename history (default: off)
  --history                  Alias for --history=all
  --commit ID                Starting commit for history traversal
  --include-commit ID        Add one exact commit ID
  --exclude-commit ID        Exclude one exact commit ID (exclusion wins)
  --all-history              Compatibility alias for --history=all
  --current-worktree         Compatibility name for HEAD-only; it does not inspect
                             staged or live file contents

Worktree and result budgets:
  --worktree                 Opt into best-effort exact/semantic inspection against
                             the active indexed HEAD; no uncommitted embeddings
  --explore                  Enumerate every eligible occurrence as references only
  --limit N                  Results per page (default: 5, or 50 with --explore)
  --byte-budget N            Maximum serialized response bytes (default: 5000)
  --cursor CURSOR            Continue the same result snapshot
  --fetch-reference ID       Fetch one full result by its returned reference_id;
                             repeat the same query/options and raise byte budget if needed
  --json                     Emit machine-readable results and pagination metadata
  -h, --help                 Show this help

A complete untruncated ordinary JSON response retains the legacy results-array shape.
Paginated, explore, and worktree responses include results, snapshot_id, cursor,
truncated, total_count when known, and worktree coverage when applicable. Text is
never cut mid-result: an oversized result returns its reference with
text_omitted_for_size; use --fetch-reference ID to retrieve it separately. If a
minimal reference cannot fit, search returns a result_limit error.

The default history view is HEAD. Matching defaults to combined exact and
semantic evidence; `--exact` and `--semantic` select one mode explicitly, while
`--combined` may be used to state the default. `--history=all` searches all indexed
history; `--history=N` uses deterministic topological order. `--current-worktree`
remains HEAD-only, while `--worktree` is opt-in and rejects stale/alternate HEADs. Semantic
worktree similarity is based on indexed HEAD text; live text is not embedded.
Search does not refresh, fetch URLs, or crawl links.

Examples:
  repodb search 'literal text' --history=all --json
  repodb search 'why does this retry?' --semantic --scope source
  repodb search 'literal text' --explore --history=all --limit 50 --json
  repodb search 'literal text' --json --cursor SNAPSHOT:OFFSET
"#;

const REFRESH_HELP: &str = r#"Initialize or refresh the local index.

Usage:
  repodb init [--semgrep-workers N] [--size-limit=error|warn|ignore]
  repodb refresh [--imports | --changed | --update COMMIT]
                 [--semgrep-workers N] [--size-limit=error|warn|ignore]

`init` creates the index and captures the checked-out repository plus registered imports.
`refresh` performs a full refresh. `--imports` refreshes registered external sources
only; `--changed` indexes commits since the previous snapshot; `--update COMMIT`
adds the selected commit. `--changed` and `--update` cannot be combined with
--imports or --size-limit.

`--semgrep-workers N` sets the positive native Git Semgrep worker count. The default
is half of the process-available logical parallelism, rounded down and at least one.
`[refresh].semgrep_workers` sets a persistent value; the CLI option overrides it.
This option does not apply to `refresh --imports`.

`--size-limit` applies to local and URL document imports during `init`, a full
`refresh`, or `refresh --imports`: `error` (default) aborts oversized documents,
`warn` continues and reports them, and `ignore` skips the policy. `--changed` and
`--update` do not accept this policy. Git repositories and Git history are not
size-gated. `import SOURCE` only registers a source; refreshing it is explicit.
Search never refreshes or fetches.

Refresh uses a one-hour total deadline (`REPODB_MAX_REFRESH_DURATION_SECS`, 1–86400),
a 10 GiB durable database cap (`REPODB_MAX_DATABASE_BYTES`), a 2 GiB temporary
staging cap (`REPODB_MAX_REFRESH_TEMP_BYTES`), a 10 GiB Git mirror cache cap
(`REPODB_MAX_GIT_CACHE_BYTES`), and a 1 GiB PDF conversion cache cap
(`REPODB_MAX_PDF_CACHE_BYTES`). Storage overrides accept 4096 bytes through 1 TiB.
On Unix, SIGINT cancels refresh and terminates its active child process group.

Examples:
  repodb init --semgrep-workers 1 --size-limit=warn
  repodb refresh --semgrep-workers=4 --size-limit=warn
  repodb refresh --imports --size-limit=ignore
  repodb refresh --changed
  repodb refresh --update COMMIT_ID
"#;

const IMPORT_HELP: &str = r#"Register one source, previewing it without side effects, or refresh registered sources.

Usage:
  repodb [--database PATH] import SOURCE
  repodb [--database PATH] import --revision REF GIT_REPOSITORY
  repodb [--database PATH] import --preview FILE_OR_DIRECTORY_OR_URL

SOURCE is one local file, one HTTP(S) page URL, or one Git repository. A URL
import fetches only the selected page; it does not crawl links. Search never
fetches. `repodb refresh` or `repodb refresh --imports` explicitly refreshes
registered URLs and other imports.

`--preview` reports local source selection or fetch size/warnings without registering
or indexing; it accepts one local file/directory or HTTP(S) page URL, not a Git repo.
For directory previews, register each selected local file separately.
`--revision` pins a Git import to one resolved branch, tag, or commit. Size policy is
selected at refresh time with `--size-limit`, not at registration.

Examples:
  repodb import ./docs/guide.md
  repodb import https://example.com/guide
  repodb import --preview https://example.com/guide
  repodb import --revision v1.2.0 https://example.com/project.git
  repodb refresh --imports
"#;

const STATUS_HELP: &str = r#"Report database, indexed/live HEAD, freshness, and readiness by capability.

Usage:
  repodb [--database PATH] status [--json]

Status does not initialize, refresh, fetch, or mutate the index. JSON distinguishes
missing database/index state from an empty searchable snapshot.
"#;

const RETENTION_HELP: &str = r#"Choose a bounded history for imported source versions.

Usage:
  repodb [--database PATH] retention latest|N

`latest` is the default and keeps only the newest distinct version per source. A
number from 2 to 100 keeps that many newest distinct versions per source; `all`
is a compatibility alias for 2. Setting a lower limit prunes excess versions and
reports the count. This applies to imported documents, URLs, and pinned Git
snapshots, not native repository history. It does not fetch sources; use an
explicit refresh.
"#;

const REDIRECT_LIMIT_HELP: &str = r#"Set the finite maximum number of HTTP(S) redirects followed when fetching URLs.

Usage:
  repodb [--database PATH] redirect-limit N

The default is 5 and the hard maximum is 20. A limit of 0 disables redirects.
This affects explicit URL refreshes and does not crawl page links.
"#;

const REEMBED_HELP: &str = r#"Rebuild stored vectors using the configured or supplied embedding command/model.

Usage:
  repodb [--database PATH] reembed [--command PATH] [--model ID]

Reembedding does not fetch or refresh external sources.
"#;

pub(crate) fn print_help(command: Option<&str>) -> Result<()> {
    let help = match command {
        None => GENERAL_HELP,
        Some("search") => SEARCH_HELP,
        Some("init" | "refresh") => REFRESH_HELP,
        Some("import") => IMPORT_HELP,
        Some("status") => STATUS_HELP,
        Some("retention") => RETENTION_HELP,
        Some("redirect-limit") => REDIRECT_LIMIT_HELP,
        Some("reembed") => REEMBED_HELP,
        Some(command) => return Err(format!("unknown help topic: {command}").into()),
    };
    print!("{help}");
    Ok(())
}
