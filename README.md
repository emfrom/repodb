# RepoDB v0.2.1

A repository search CLI with exact and embedding-based search.

This is something I cooked up to give agents faster understanding of codebases and codebase operations. Usable, but integrations are not finished. Depends a script generate-embeddings, ask you local llm to adapt it to your circumstances.

**Alpha-yolo software, use at your own risk.**

See [CHANGELOG.md](CHANGELOG.md) for version history.

Custom embedding commands receive each text on stdin and must print a JSON numeric vector on stdout. RepoDB invokes them as `COMMAND --json -` and sets `REPODB_EMBEDDING_MODEL` to the configured model ID for every indexing and query invocation. Commands must produce vectors consistent with that model ID. The built-in `~/bin/generate_embeddings` supports only `all-MiniLM-L6-v2`.

## Git history and scopes

```sh
repodb init                          # build the first history snapshot
repodb refresh                       # full snapshot from current refs + HEAD
repodb refresh --changed              # incrementally index current branch HEAD vs first parent
repodb search 'literal text'          # checked-out HEAD only (the default)
repodb search 'literal text' --all-history --json
repodb search 'literal text' --commit COMMIT_ID
repodb search 'question' --semantic --include-commit ID1 --include-commit ID2
repodb search 'question' --combined --all-history --exclude-commit ID3
```

Search does not refresh the index. Run `repodb refresh` on demand after ref
changes or when indexed content may be stale. `repodb refresh --changed` only
indexes the current branch's HEAD relative to its first parent: unchanged paths
reuse their indexed blob units and vectors, while changed paths are re-read and
re-segmented only when their blob/segmentation variant is not already indexed.
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

Choose exactly one scope family:

- No selector, or `--current-worktree`: checked-out HEAD.
- `--all-history`: every commit in the captured snapshot.
- `--commit ID`: exactly one captured commit, not its ancestors.
- Repeatable `--include-commit ID`: the union of those exact captured commits.

These families conflict; `--commit` cannot be repeated. Repeatable
`--exclude-commit ID` subtracts commits from **any** selected scope, including the
default. Repeating an include/exclude ID is harmless; exclusion wins if an ID is
both included and excluded. Selecting an empty set returns no matches.

IDs must be full hexadecimal commit IDs or unambiguous hexadecimal prefixes of
at least four characters, resolved against **indexed commits**, not live refs.
Unknown and ambiguous IDs are errors. Branch/tag names, revision expressions,
and ranges are not accepted. A deleted ref's captured commit remains selectable
until a successful explicit refresh removes it.

All scopes and filters apply equally to exact, semantic, and combined search.
Exact search remains a case-insensitive literal substring search. `init` and a
full `refresh` prepare searchable units and embeddings for the captured history
in one atomic snapshot. `refresh --changed` atomically indexes the new commit, embeds changed or
previously unindexed content, and reuses unchanged units and vectors. Semantic search works after either mode. The first scan uses
`[tools].embedding_command` from the XDG RepoDB config, or defaults to
`~/bin/generate_embeddings` with `all-MiniLM-L6-v2`; the selected command/model
are stored per database and reused on later scans. Use
`reembed [--command PATH] [--model ID]` to change them. `--semantic-limit N`
(default 20) limits semantic candidates, not exact matches in combined search.

### Imported sources

Register one local file, HTTP(S) URL, local Git repository, or remote Git URL at a time with `repodb import PATH_OR_URL`. Git remotes can use SSH, Git, `file://`, or HTTP(S) URLs ending in `.git`; ordinary directories are rejected. `repodb retention latest` (the default) keeps only the newest fetched version per source, while `repodb retention all` retains prior versions. `repodb refresh` explicitly reads registered files, fetches registered URLs, and updates registered Git repositories; searches never fetch. Imported Git results include the repository locator, commit ID, repository-relative path, and one-based line reference. Semantic imported search uses the repository's configured embedding command.

Refresh does not impose fixed per-file, URL, PDF-text, page-count, unit-count, or aggregate preparation-byte ceilings. Subprocess stderr is drained while retaining only a bounded diagnostic excerpt; large stderr streams do not reject otherwise valid output. Git tree records are processed incrementally, and failed reads/conversions still leave the prior searchable snapshot intact. PDF conversion caches are reused when available. Importing PDF files requires the `pdftotext` executable (from Poppler) to be installed and available on `PATH` when `repodb refresh` converts a PDF; a missing converter is reported with an installation requirement. Extracted PDF text is indexed as bounded paragraph units within each page; JSON search results include the one-based `page` number, and human-readable results include the page reference. Imported sources are processed sequentially, with PDF pages and content units handled one at a time; this does not promise a fixed maximum memory footprint.

### References, duplicates, and ordering

JSON results contain `commit_id`, `blob_id`, `path`, `line_start`, `line_end`,
`text`, and (for semantic/combined searches) cosine `distance`. Human output also
includes full commit and blob IDs. Line ranges are one-based and inclusive;
each result returns the complete matching indexed unit, not just a matching line.

Commit filtering happens **before** duplicate collapse. Each indexed unit of an
identical blob appears once, at the newest eligible occurrence: committer timestamp
descending, then full commit ID descending, then path ascending (byte order).
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
