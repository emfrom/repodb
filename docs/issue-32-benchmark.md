# Issue #32: Git-backed search benchmark

## Compared versions and fixture

- Baseline: `7d183627c82f053717e9198b712764f6f6f1a95f` (`v0.2.0`, immediately before #31's blob-backed range storage).
- Updated implementation: `70235caff428419dfcfa93bb829e9905e5f81641` (batch hydration implementation).
- Both databases were initialized from the same checked-out project snapshot, `9c2f779374f6e68cf442d4deffece63f5543285d`, with the same Git refs available: 96 reachable commits, 14,924 indexed units, and 3,212 commit/path occurrences. Each database also registered the same local Git source and indexed 7,060 imported Git units.
- Embeddings used a deterministic two-dimensional stub (`[1,0]`); this isolates RepoDB query/hydration work and does not measure real-model embedding latency. The imported Git checkout was the same project snapshot, so the `use` literal returned local and imported Git results.
- The resulting primary SQLite files were 25,284,608 bytes before and 6,250,496 bytes after. This is a storage comparison, not a query-time memory measure.

## Results

Each query was run three times per binary. The table shows the first sample and the median of samples 2–3 (the warm-repeat estimate). Peak RSS is the median of the three `/usr/bin/time` peak-RSS readings. Git invocation counts were stable across all samples; they count `git cat-file` process starts, not all Git commands.

| Workload | Baseline first / warm median (s) | Updated first / warm median (s) | Peak RSS baseline / updated (MiB) | `cat-file` baseline / updated per query |
|---|---:|---:|---:|---:|
| Exact, common literal `use` | 0.03 / 0.030 | 0.09 / 0.070 | 12.4 / 10.8 | 0 / 2 |
| Exact, absent literal `unlikely_literal_abc987` | 0.03 / 0.030 | 0.07 / 0.065 | 12.3 / 11.0 | 0 / 2 |
| Semantic, top 10, `how does repository search work` | 0.06 / 0.055 | 0.07 / 0.065 | 12.5 / 12.4 | 0 / 2 |
| Combined, common literal `use`, top 10 | 0.05 / 0.055 | 0.10 / 0.110 | 12.4 / 13.5 | 0 / 2 |
| Exact, all history, `use` | 0.06 / 0.060 | 0.14 / 0.135 | 22.6 / 25.5 | 0 / 2 |
| Exact with imported Git enabled, `use` | 0.04 / 0.035 | 0.07 / 0.080 | 12.4 / 10.9 | 0 / 2 |

The updated version starts one batch reader for the local repository and one for the registered imported Git object store. The pre-#31 baseline retains unit text in SQLite and starts no `cat-file` process during search. The blob-backed version reduces the primary database file by about 75% in this fixture, while this small-repository query set shows higher elapsed time for exact and combined searches. Semantic top-10 remains close to baseline. The batch reader avoids one process per blob, but hydration and result-text materialization still have a cost.

A subsequent all-history rerun after releasing nonmatching hydrated result text recorded samples of 0.14/0.13/0.14 seconds and peak RSS of 25,712/25,524/25,572 KiB (two `git cat-file` starts per sample). The median RSS is about 25.5 MiB, improved from the prior 30.7 MiB, but remains above the pre-#31 baseline of 0.06 seconds and 22.6 MiB. This residual time and memory regression is expected in part because range-backed storage necessitates hydrating Git blobs and materializing output text; these measurements do not demonstrate parity with the text-retaining baseline.

No OS page-cache flush was performed (and the first sample was not guaranteed cold: indexing and earlier setup had already read the same object store). Therefore these results do **not** claim cold-cache measurements. Reported first samples are the first query of that case in a run, not a cold-cache result. Warm-repeat estimates are samples 2–3. The timer's 0.01-second resolution limits interpretation of the fastest cases. Use a larger repository and an explicit cache-control setup for cold-cache claims.

## Reproduction

The benchmark driver is `scripts/bench_issue_32.sh`. It expects two built binaries and two already-initialized repository roots. It runs the six workloads above three times, counts `git cat-file` starts via a temporary PATH wrapper, and reports elapsed time and peak RSS. Example:

```sh
scripts/bench_issue_32.sh \
  /tmp/repodb-bench-before /tmp/repodb-bench-after \
  /tmp/repodb-bench/before /tmp/repodb-bench/after
```

To prepare matching binaries and roots from a repository checkout:

```sh
REPO=$(git rev-parse --show-toplevel)
TMP=/tmp/repodb-bench
mkdir -p "$TMP"
git worktree add --detach "$TMP/baseline-src" 7d183627c82f053717e9198b712764f6f6f1a95f
CARGO_TARGET_DIR="$TMP/cargo-target" cargo build --release --manifest-path "$TMP/baseline-src/Cargo.toml" --bin repodb
cp "$TMP/cargo-target/release/repodb" "$TMP/repodb-bench-before"
CARGO_TARGET_DIR="$TMP/cargo-target" cargo build --release --bin repodb
cp "$TMP/cargo-target/release/repodb" "$TMP/repodb-bench-after"
for repo in before after import-source; do
  git clone --shared --quiet --branch main "$REPO" "$TMP/$repo"
done
cat > "$TMP/embedder" <<'EOF'
#!/bin/sh
cat >/dev/null
printf '[1,0]'
EOF
chmod +x "$TMP/embedder"
for side in before after; do
  mkdir -p "$TMP/$side/.xdg/repodb"
  cat > "$TMP/$side/.xdg/repodb/config.toml" <<EOF
[tools]
embedding_command = "$TMP/embedder"
semgrep_core = "${REPODB_SEMGREP_CORE:?set this to Semgrep core v1.181.0}"
EOF
  (
    cd "$TMP/$side"
    XDG_CONFIG_HOME="$TMP/$side/.xdg" "$TMP/repodb-bench-$side" init
    XDG_CONFIG_HOME="$TMP/$side/.xdg" "$TMP/repodb-bench-$side" import "$TMP/import-source"
    XDG_CONFIG_HOME="$TMP/$side/.xdg" "$TMP/repodb-bench-$side" refresh
  )
done
```

Run the preparation with the repository snapshot and refs held fixed if comparing new measurements to the table; cloning a later branch tip changes the indexed corpus. Run the measured-query script only after each root is initialized and has completed the imported-Git refresh. The initial index and import refresh are intentionally separate from query timing and take substantially longer than the searches.
