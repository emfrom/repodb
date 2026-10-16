# repodb

A repository search CLI with exact and embedding-based search.

Custom embedding commands receive each text on stdin and must print a JSON numeric vector on stdout. RepoDB invokes them as `COMMAND --json -` and sets `REPODB_EMBEDDING_MODEL` to the configured model ID for every indexing and query invocation. Commands must produce vectors consistent with that model ID. The built-in `~/bin/generate_embeddings` supports only `all-MiniLM-L6-v2`.

## Git history and scopes

```sh
repodb init                          # build the first history snapshot
repodb refresh                       # explicitly replace it from current refs + HEAD
repodb search 'literal text'          # checked-out HEAD only (the default)
repodb search 'literal text' --all-history --json
repodb search 'literal text' --commit COMMIT_ID
repodb search 'question' --semantic --include-commit ID1 --include-commit ID2
repodb search 'question' --combined --all-history --exclude-commit ID3
```

Search does not refresh the index. Run `repodb refresh` on demand after ref
changes or when indexed content may be stale. An optional post-commit hook is
provided in `hooks/post-commit`; enable it only by explicitly configuring
this repository's `core.hooksPath` to `hooks`.

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
Exact search remains a case-insensitive literal substring search. Embedding
search requires `reembed [--command PATH] [--model ID]`; `--semantic-limit N`
(default 20) limits semantic candidates, not exact matches in combined search.

### Imported sources

Register one local file or an HTTP(S) URL at a time with `repodb import PATH_OR_URL`; directories are rejected. `repodb retention latest` (the default) keeps only the newest fetched version per source, while `repodb retention all` retains prior versions. `repodb refresh` explicitly reads registered files and fetches registered URLs; searches never fetch. Imported results identify the original path or URL and a one-based line range. Semantic imported search uses the repository's configured embedding command.

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

- `.md`, `.markdown`, and `.txt`: paragraphs delimited by blank or whitespace-only
  lines (including CRLF input). Separating blank lines and the paragraph's final
  line terminator are excluded; internal whitespace and line endings are retained.
- `.rs`: Rust top-level items parsed by `syn`, including attributes and doc comments.
  Nested constructs stay within their top-level item. Units slice original source
  spans, not reformatted tokens; even two items on one line remain distinct.
  Comments, file attributes, imports, and text between/after items are retained;
  non-item text uses the paragraph heuristic rather than being silently dropped.
- All other extensions, extensionless files, and malformed Rust: a bounded
  blank-line paragraph heuristic, without guessing language syntax. Extension
  matching is case-sensitive. No PDF extraction/page references are added here.

Every unit is at most **4096 UTF-8 bytes**. Oversized paragraphs, Rust items, and
fallback units split at the last complete newline within that bound, otherwise
at a UTF-8 character boundary (never between CR and LF). Split pieces retain all
text, including internal line endings. Ranges refer to the original one-based,
inclusive source lines; pieces of a long single line share its line range.
Whitespace-only separators are not standalone units.

Exact matching is still literal and case-insensitive, including punctuation and
newlines within a unit. A query spanning separate units (including size-imposed
boundaries) cannot match. Distinct units with the same starting line retain source
order when other ordering keys tie. Binary/invalid UTF-8 blobs have no text units.

### Refresh and storage safety

Refresh captures ref object IDs plus HEAD (including detached HEAD), traverses
their reachable commits, reads each distinct blob once, and retains every
commit/path occurrence. Reflog-only commits are not retained. Metadata is kept
for binary and unsupported blobs; only NUL-free UTF-8 text is searchable.
Paths are stored as Git bytes; non-UTF-8 paths retain metadata but are not emitted
as search results. Submodule commits are not file blobs and are not traversed.

Storage version 3 separates Git commits, distinct blob contents, indexed units,
occurrences, and per-unit vectors. Occurrences record their segmentation kind,
and unit mappings are keyed by blob and kind. Unit content/vectors do not require
fabricated Git commits for future imported sources. Repeated historical
occurrences of the same blob/kind do not cause repeated embeddings within one
refresh/reembed. Different kinds may require
different units and embeddings for the same blob. Refresh currently traverses
every reachable commit's tree and regenerates vectors for distinct units when a
model is configured; very large histories can be expensive.

Existing #3 databases (without blob IDs) and version 2/#4 databases (line units)
remain searchable in their original representation until a successful explicit
`init`/`refresh` rebuilds them as version 3. All Git reads and embedding generation
finish before publication; tables, vectors, model dimension, and schema version
are published in one SQLite
transaction. Git/embedding/migration failures preserve the previous usable
snapshot and configuration. Refresh retains the configured command/model and
requires its established vector dimension; changing models requires `reembed`.
`init` also works before a repository's first commit.

If another writer publishes during refresh/reembed preparation, the stale command
fails with a retry message instead of mixing snapshots or embedding models.
Search reads scope, model configuration, and results from one database snapshot.
