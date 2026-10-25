# RepoDB v0.3.1

A repository search CLI with exact and embedding-based search.

I built RepoDB to help agents understand codebases and work with them more quickly. It is usable, but integrations are not finished. It depends on the [`generate-embeddings` script](https://github.com/emfrom/generate_embeddings); ask your local LLM to adapt the script to your needs.

**Alpha-yolo software; use at your own risk.**

See [CHANGELOG.md](CHANGELOG.md) for version history. Release-review cadence: before 1.0.0, reviews are mandatory for major/minor version increases; from 1.0.0 onward, they are mandatory before every release. See [docs/reviews](docs/reviews/README.md).

Custom embedding commands receive each text on stdin and must print a JSON numeric vector on stdout. RepoDB invokes them as `COMMAND --json -` and sets `REPODB_EMBEDDING_MODEL` to the configured model ID for every indexing and query invocation. Commands must produce vectors consistent with that model ID. The built-in [`~/bin/generate_embeddings`](https://github.com/emfrom/generate_embeddings) supports only `all-MiniLM-L6-v2`.

## Git history and scopes

```sh
repodb init                          # build the first history snapshot
repodb refresh                       # full snapshot from current refs + HEAD
repodb refresh --changed              # incrementally index current branch HEAD vs first parent
repodb refresh --update COMMIT         # add one historical commit without pruning
repodb search 'literal text'          # combined exact + semantic matching (default)
repodb search 'literal text' --exact   # exact-only matching
repodb search 'literal text' --exact --worktree --json  # exact live changes
repodb search 'question' --semantic --worktree --json  # map indexed matches to live text
repodb search 'literal text' --history=all --json
repodb search 'literal text' --history=20 --commit COMMIT_ID
repodb search 'literal text' --history=src/
repodb search 'literal text' --all-history --json  # legacy alias
repodb search 'literal text' --explore --history=all --json # all refs, reference-only pages
repodb search 'question' --semantic --include-commit ID1 --include-commit ID2
repodb search 'question' --combined --all-history --exclude-commit ID3
repodb search 'question' --scope repo-local-docs
repodb search 'question' --semantic --scope source
```

By default, the SQLite database is `repodb.sqlite` in the repository's Git
directory. To use a separate database, prefix any command with
`--database PATH`, for example `repodb --database /tmp/corpus.sqlite init` and
`repodb --database /tmp/corpus.sqlite search 'literal text'`. Relative paths
are resolved against the caller's current working directory. Selecting a
file does not change which Git repository is indexed.

Search does not refresh the index. Run `repodb refresh` on demand after ref
changes or when indexed content may be stale. `repodb refresh --update COMMIT`
adds only the specified commit's complete tree, without indexing ancestors,
refreshing imported sources, or pruning prior history. It accepts a full
hexadecimal commit ID or a unique hexadecimal prefix and stores the canonical
full ID. The commit object must already exist locally, but need not be reachable
from a ref; RepoDB does not fetch. Merge commits use their complete tree. An
already-indexed commit is a no-op. Invalid IDs, missing objects, embedding
failures, and concurrent index changes leave the published snapshot unchanged.
Use regular `repodb refresh` when you explicitly want to rebuild and prune the
snapshot.

`repodb refresh --changed` only indexes the current branch's HEAD relative to
its first parent: unchanged paths reuse their indexed blob units and vectors,
while changed paths are re-read and re-segmented only when their
blob/segmentation variant is not already indexed.
Merge commits use their first parent; an initial commit indexes its tracked tree.
When a first parent exists, it must already be indexed; otherwise this mode
fails with guidance to run a full refresh. An initial commit has no baseline
requirement. It does not refresh imported sources or prune
other refs. The optional post-commit hook uses this scoped mode; enable it only
by explicitly configuring this repository's `core.hooksPath` to `hooks`.

`--current-worktree` explicitly selects the default scope: the checked-out
**commit**, never staged, unstaged, or untracked content. Checking out an already
indexed commit changes that scope without rebuilding. A HEAD not in the snapshot
returns no matches; run `refresh` to index it. Search never refreshes, prunes, or
migrates the index, including after refs are deleted.

`--worktree` opts into best-effort inspection relative to the active indexed
HEAD and rejects alternate or stale HEADs. In exact mode, changed tracked files
and Git-visible untracked supported text are searched live; staged/live
provenance is labeled, deleted HEAD content matches are removed, and no
embeddings are generated. In semantic mode, existing indexed HEAD embeddings
rank candidates; staged/live diffs map their references forward, and output
labels the score as similarity to indexed HEAD text. Live text is shown only
when mapping is unambiguous; deleted or ambiguous mappings retain the matched
HEAD lines. Newly staged/untracked additions and excluded files are disclosed
as coverage gaps. Neither mode refreshes or persists worktree text. Concurrent
edits may produce a non-atomic observation, which is labeled in the output.
With `--json`, opted-in output is an object containing `results` and a
`worktree` observation/coverage summary. Responses that need pagination include
`results`, `cursor`, `snapshot_id`, `truncated`, `total_count` (null when not
known), and any worktree summary; an untruncated ordinary JSON response retains
the existing array shape. `--current-worktree` remains HEAD-only.

Use one coarse history selector:

- No selector or `--history=off`: checked-out HEAD only (the default). Bare
  `--history` means `--history=all`.
- `--history=all`: every captured commit in the indexed snapshot.
- `--history=N`: the first N commits in deterministic topological traversal
  reachable from HEAD, including merged ancestry; N must be positive. A numeric
  path can be written as `./123` to distinguish it from a count.
- `--history=PATH`: content and changes for a repository-relative file or
  directory (including descendants), following detected renames as explicitly
  heuristic links. Results show the path at each commit and exclude unrelated
  files from the same commit.

For `--history=off`, `--history=N`, or `--history=PATH`, `--commit ID` (or
`--commit=ID`) chooses the starting commit instead of HEAD. With `off`, this
selects that single committed view; with N or PATH, it changes the traversal
start. It conflicts with `--history=all`; `--commit` without `--history` keeps
its legacy meaning of selecting exactly one captured commit. `--all-history` remains a legacy alias for the all-history scope and
conflicts with `--history`, `--current-worktree`, `--commit`, and
`--include-commit`; prefer `--history=all`. `--current-worktree` remains HEAD-only
and conflicts with explicit `--history`.

In `--history` mode, repeatable `--include-commit ID` adds exact captured commits
and repeatable `--exclude-commit ID` removes them; exclusion wins. With legacy
selectors, `--include-commit` retains its existing meaning of selecting the union
of those exact commits, and conflicts with `--all-history`, `--current-worktree`,
or `--commit`. Repeating an ID is harmless. Selecting an empty set returns no
local history matches. IDs must be full hexadecimal commit IDs or unambiguous
hexadecimal prefixes of at least four characters, resolved against indexed
commits. Unknown and ambiguous IDs are errors; branch/tag names, revision
expressions, and ranges are not accepted.

History selectors apply only to this repository's indexed Git history. They do
not filter imported files, websites, or imported Git repositories; imported
sources are searched independently and may still appear in results.

A deleted ref's captured commit remains selectable until a successful explicit
refresh removes it.

Search and `search --explore` default to combined exact and semantic matching when
semantic search is available. `--exact` selects exact-only matching; `--semantic`
selects semantic-only matching; `--combined` explicitly states the default. The
same selectors, filters, and ranking rules apply to ordinary search and explore.
Exact matching remains a case-insensitive literal substring search.

Search results can also be filtered with `--scope` (default `all`):

- `source`: recognized source-code files from this repository and imported Git repositories.
- `repo-local-source`: recognized source-code files from this repository's indexed Git history.
- `repo-local-docs`: recognized documentation from this repository's indexed Git history.
- `all-docs`: recognized documentation from any indexed origin.
- `all`: every matching result, including files whose type is unknown.

Scopes are orthogonal to commit selectors: repository-local results still obey
`--history`, the legacy `--all-history`, `--commit`, `--include-commit`, and
`--exclude-commit`.
Documentation is recognized from supported file extensions (`.md`, `.markdown`,
`.txt`, `.pdf`, `.html`, `.htm`, `.org`), known documentation basenames such as
`README` and `LICENSE`, or PDF page metadata. This is a metadata-based
classification, not a guarantee that every documentation format or unusual
filename is recognized. Unknown or unclassifiable results are included only by
`--scope all`. Source files are recognized using RepoDB's supported source
segmentation types; ambiguous or unsupported file types are not treated as
source. `source` does not include imported standalone files or websites. `init` and a
full `refresh` prepare searchable units and embeddings for the captured history
in one atomic snapshot. `refresh --changed` atomically indexes the new commit, embeds changed or
previously unindexed content, and reuses unchanged units and vectors. Both paths
segment and submit units in batches of at most 256 while scanning continues; one
embedding worker processes batches sequentially with at most one queued batch.
The built-in command receives each batch through its NUL-delimited protocol.
Configured custom commands retain one process invocation per unit, sequentially
within each batch; RepoDB does not run custom commands concurrently. Semantic
search works after either mode. The first scan uses
`[tools].embedding_command` from the XDG RepoDB config, or defaults to
[`~/bin/generate_embeddings`](https://github.com/emfrom/generate_embeddings) with `all-MiniLM-L6-v2`; the selected command/model
are stored per database and reused on later scans. Use
`reembed [--command PATH] [--model ID]` to change them. `--semantic-limit N`
(default 20) limits semantic candidates, not exact matches in combined search.

### Bounded search and exploration

Ordinary search returns at most 5 results per page; `--explore` returns at most
50 reference-only results per page and enumerates every eligible historical
occurrence, including duplicate blobs at different commits or paths. Both use a
5,000-byte serialized-response budget by default. Configure the page count with
`--limit N` and the byte budget with `--byte-budget N`. JSON pagination responses
include a stable `snapshot_id`, `total_count` when known, `truncated`, and a
`cursor` when another page is available; pass that cursor back with `--cursor`.
The cursor is valid only while the selected result snapshot remains the same.

If a complete result cannot fit, RepoDB emits its reference and sets
`text_omitted_for_size` rather than cutting text. Fetch that result separately
by repeating the same search with `--fetch-reference REFERENCE_ID`; increase
`--byte-budget` enough to fit the complete result. `--fetch-reference` cannot be
combined with `--explore` or `--cursor`. If even the minimal reference cannot
fit, search returns an explicit `result_limit` error. `--explore` removes text
recursively, including indexed/live semantic text, but preserves references and
ranking/provenance metadata.

### Imported sources

Register one local file, HTTP(S) URL, local Git repository, or remote Git URL at a time with `repodb import PATH_OR_URL`. Git remotes can use SSH, Git, `file://`, or HTTP(S) URLs ending in `.git`; ordinary directories are rejected. `repodb import --preview URL` performs one GET to the selected HTTP(S) page without registering, indexing, or updating retrieval timestamps; it reports response-body bytes and warns at 50,000,000 bytes or when bytes exceed twice the current database size. URL growth is unknown, so source bytes are the warning basis. Preview has no fixed byte ceiling and is advisory: the later import fetches again and may observe different content. It follows neither links nor redirects. `repodb retention latest` (the default) keeps only the newest distinct version per source; `repodb retention 2` keeps the two newest distinct versions, and `all` is a compatibility alias for 2. Limits from 2 to 100 are configurable; lowering a limit prunes excess versions and reports the count. For URL pages, SHA-256 of the extracted searchable text is the version identity and JSON `blob_id`; unchanged extracted text deduplicates, while changed text creates a version. URL JSON provenance includes the requested URL, final URL, redirect `Location` chain, fetch status/error, and UTC RFC3339 `first_retrieved_at` and `last_retrieved_at`. Failed-fetch status, error, and attempt time are durable telemetry and may change without replacing the last successfully published content; successful URL provenance is published atomically with its version. Redirects are followed with a finite configurable limit (`repodb redirect-limit N`, default 5, maximum 20); legacy unknown first timestamps remain null even after a later refresh, which updates `last_retrieved_at` without inventing historical provenance. With `latest`, A→B→A makes A latest again, but a pruned hash's first timestamp starts a new retained-row lifetime because its discarded metadata cannot be reconstructed; with a bounded history limit, retained distinct hashes keep their original first timestamps. `repodb refresh` and `repodb refresh --imports` explicitly observe registered URLs; search never fetches. Imported Git results include the repository locator, commit ID, repository-relative path, and one-based line reference. Semantic imported search uses the repository's configured embedding command.

Refresh accounts for retained preparation allocations with a **512 MiB default aggregate byte budget**. Set `REPODB_MAX_REFRESH_BYTES` to a decimal byte count from 1 through 4,294,967,296 to override it. Exceeding the budget rejects the refresh before publication, leaving the previous searchable snapshot intact. Each history and import preparation phase also has a 10,000,000-record default cap, configurable with `REPODB_MAX_REFRESH_RECORDS` from 1 through 100,000,000; the counters are separate, and exceeding either cap aborts before publication. Refresh subprocesses (history Git commands and persistent Git object readers, Git clone, URL fetch, PDF conversion, and embedding) have a five-minute default per-process deadline, configurable from 1 to 3600 seconds with `REPODB_REFRESH_TIMEOUT_SECS`, bounded captured output, and descendant cleanup on Unix; URL fetching also has a 30-second total deadline that includes DNS resolution. DNS runs on one bounded worker; a resolver that stalls causes later hostname requests in the same process to fail closed. URL fetching rejects loopback, private, link-local, multicast, reserved, and other non-global IPv4/IPv6 destinations on every redirect, pins DNS answers with curl `--resolve`, and disables proxy use. `REPODB_URL_PRIVATE_ALLOWLIST` can explicitly trust a comma-separated list of IP literals for controlled environments; do not set it when importing untrusted URLs. These limits do not cap cloned repository disk growth, durable database/cache growth, total refresh wall-clock time across many child processes, or allocations not included in preparation accounting. Subprocess stderr is drained while retaining only a bounded diagnostic excerpt; large stderr streams do not reject otherwise valid output. Git tree records are processed incrementally, and failed reads/conversions still leave the prior searchable snapshot intact. PDF conversion caches are reused when available. Importing PDF files requires the `pdftotext` executable (from Poppler) to be installed and available on `PATH` when `repodb refresh` converts a PDF; a missing converter is reported with an installation requirement. Extracted PDF text is indexed as bounded paragraph units within each page; JSON search results include the one-based `page` number, and human-readable results include the page reference. For each PDF version RepoDB retains the original-byte SHA-256 as its version ID and stores the ordered extracted page strings, the SHA-256 of their canonical JSON, and a SHA-256 of the stored representation in the database. Original PDF bytes are not retained. This costs one additional copy of the extracted pages (in addition to indexed units); cache deletion does not remove that durable snapshot. Local document versions retain decoded text and its representation hash; their version ID remains the SHA-256 of source bytes. URL versions use the SHA-256 of extracted searchable text as their version ID. These hashes are checked when the database schema is initialized, so tampered durable content fails explicitly instead of being accepted. Older databases with PDF versions have no recoverable full page snapshot in `imported_versions`: migration leaves the representation hash null rather than inventing one from incomplete indexed paragraphs or claiming recovery from the source hash. Existing indexed units remain searchable and reembeddable offline. An explicit refresh can build a complete snapshot for the current source; it does not restore historical URL versions and migration never fetches URLs. Imported sources are processed sequentially, with PDF pages and content units handled one at a time; this does not promise a fixed maximum memory footprint.

### References, duplicates, and ordering

JSON results contain a stable-within-snapshot `reference_id`, `commit_id`,
`blob_id`, `path`, `line_start`, `line_end`, `text`, and (for semantic/combined
searches) cosine `distance`. Human output also
includes full commit and blob IDs. Line ranges are one-based and inclusive;
each result returns the complete matching indexed unit, not just a matching line.

Commit filtering happens **before** ordinary-search duplicate collapse. Each
indexed unit of an identical blob appears once, at the newest eligible occurrence:
committer timestamp descending, then full commit ID descending, then path ascending
(byte order). `--explore` bypasses this collapse and pages every eligible
commit/path reference.
This is a deterministic timestamp policy, not ancestry ordering. The chosen
occurrence's path determines the segmentation kind; only that representation of
the blob is returned, even when identical content occurs as both `.md` and `.rs`.
Excluding the chosen commit can reveal an older occurrence and its segmentation.
Different blobs, or distinct units within one blob, are not collapsed merely
because their text is equal.

Semantic top-N is applied **after** filtering and duplicate collapse. Combined
search unions those candidates with all exact matching units, deduplicates by
unit identity, and orders the entire union by actual cosine distance. Distance
ties use path, starting line, then blob ID ascending. Exact results use path,
starting line, then blob ID ascending.

### Indexed units and source ranges

All three search modes use the same units, including the full text sent to the
configured embedding command:

- Supported documentation formats (`.md`, `.markdown`, `.txt`, and extracted PDF
  pages): paragraphs delimited by blank or whitespace-only lines (including CRLF
  input). PDF page boundaries always remain separate, even when adjacent pages
  contain text that would otherwise form one paragraph; indexed PDF results carry
  page metadata. Separating blank lines are excluded, while internal whitespace
  and line endings are retained.
- Source extensions with a dedicated Semgrep v1.181.0 parser: top-level syntax
  units plus all source gaps, validated against exact UTF-8 token offsets. Rust
  is parsed through Semgrep's `rust` target, not a separate parser.
- Unsupported or ambiguous extensions, and sources Semgrep cannot parse
  consistently: a bounded blank-line paragraph heuristic. Extension matching is
  case-sensitive.

A Semgrep core v1.181.0 executable is required when refreshing any supported
source-code file. Configure it in `$XDG_CONFIG_HOME/repodb/config.toml`, or
`~/.config/repodb/config.toml` when `XDG_CONFIG_HOME` is unset:

```toml
[tools]
semgrep_core = "/path/to/semgrep-core"
embedding_command = "/path/to/generate_embeddings"
# Optional override for source byte ranges:
# segmenter = "/path/to/segmenter"
```

`REPODB_SEMGREP_CORE` and `REPODB_SEGMENTER`, when set, override their config
values. Missing or wrong-version Semgrep cores fail refresh instead of silently
switching the source index to heuristic chunks. Unsupported extensions and
parser failures still use the fallback above.

Every unit is at most **4096 UTF-8 bytes**. Oversized paragraphs, Semgrep syntax
ranges, and fallback units split at the last complete newline within that bound,
otherwise at a UTF-8 character boundary (never between CR and LF). Split pieces
retain all text, including internal line endings. Ranges refer to the original
one-based, inclusive source lines; pieces of a long single line share its line
range. Whitespace-only separators between document paragraphs are not standalone
units; source gaps are preserved as returned by Semgrep.

Exact matching is still literal and case-insensitive, including punctuation and
newlines within a unit. A query spanning separate units (including size-imposed
boundaries) cannot match. Distinct units with the same starting line retain source
order when other ordering keys tie. Binary/invalid UTF-8 blobs have no text units.

An explicit external range segmenter can override the source partition with
`REPODB_SEGMENTER=/path/to/tool` or `tools.segmenter` in the config file, but the
required Semgrep runtime is still validated first. RepoDB invokes the segmenter
directly with the kind as its sole argument and sends the exact UTF-8 source bytes
on stdin. It must print a JSON array of `[start,end]` half-open byte-offset pairs
on stdout. The ranges must form an ordered, non-overlapping, UTF-8-aligned full
partition of the input. Output is capped at 8 MiB and execution at 10 seconds;
missing tools, nonzero exits, timeouts, oversized output, or invalid ranges use the
built-in segmentation instead. The environment variable is unset by default, and
RepoDB does not install or bundle an external segmenter.

### Refresh and storage safety

Refresh captures ref object IDs plus HEAD (including detached HEAD), traverses
their reachable commits, reads each distinct blob once, and retains every
commit/path occurrence. Reflog-only commits are not retained. Metadata is kept
for binary and unsupported blobs; only NUL-free UTF-8 text is searchable.
Paths are stored as Git bytes; non-UTF-8 paths retain metadata but are not emitted
as search results. Submodule commits are not file blobs and are not traversed.

Storage version 4 separates Git commits, blob IDs/status, indexed-unit byte and line ranges,
occurrences, and per-unit vectors. Indexed Git text is hydrated from the referenced Git object
at search time; the database does not retain verbatim Git blob or unit text. Imported Git
sources retain their own object-store locator and blob/range references, separate from local
history objects. Search therefore requires the corresponding Git objects to remain available;
missing objects or invalid byte ranges are errors. Refresh and reembed read source text from
those objects, while temporary refresh staging may contain text before atomic publication.
Explicit refresh rebuilds legacy Git text snapshots from their recorded Git repositories into
range-only storage; legacy rows remain searchable until that successful refresh, and search
never initiates migration or refresh. Non-Git imported documents continue to store their text.
Occurrences record their segmentation kind, and unit mappings are keyed by blob and kind.
Unit content/vectors do not require fabricated Git commits for future imported sources.
Repeated historical
occurrences of the same blob/kind do not cause repeated embeddings within one
refresh/reembed. Different kinds may require
different units and embeddings for the same blob. A full refresh traverses
every reachable commit's tree and generates vectors for distinct units when a
model is configured. Refresh no longer rejects reachable history based on fixed commit, occurrence,
blob-size, unit-count, vector-count, or aggregate byte ceilings. Git tree records
are consumed incrementally rather than captured as a complete tree. All Git reads
and embedding generation finish before publication, and failures leave the
previous snapshot searchable.

Existing #3 databases (without blob IDs), version 2/#4 databases (line units),
and version 3 databases (verbatim Git text) remain searchable in their original
representation until a successful explicit `init` or full `refresh` rebuilds them
as version 4. All Git reads and embedding generation
finish before publication; tables, vectors, model dimension, and schema version
are published in one SQLite
transaction. Git/embedding/migration failures preserve the previous usable
snapshot and configuration. Refresh retains the configured command/model and
requires its established vector dimension; changing models requires `reembed`.
`init` also works before a repository's first commit.

If another writer publishes during refresh/reembed preparation, the stale command
fails with a retry message instead of mixing snapshots or embedding models.
Search reads scope, model configuration, and results from one database snapshot.

## License

RepoDB is licensed under the GNU General Public License v2 only (`GPL-2.0-only`).
See [LICENSE](LICENSE) for the full text.
