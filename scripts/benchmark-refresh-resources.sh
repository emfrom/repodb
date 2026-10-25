#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null && pwd -P)
FILE_COUNT=${FILE_COUNT:-5000}
UPDATED_COUNT=${UPDATED_COUNT:-100}
IMPORT_PARAGRAPHS=${IMPORT_PARAGRAPHS:-2048}
BENCH_ROOT=${REPODB_BENCH_ROOT:-$(mktemp -d /tmp/repodb-resource-bench.XXXXXX)}
BIN=

if (( UPDATED_COUNT > FILE_COUNT )); then
  printf 'UPDATED_COUNT must not exceed FILE_COUNT\n' >&2
  exit 2
fi

if [[ -n "${REPODB_BIN:-}" ]]; then
  BIN=$REPODB_BIN
else
  CARGO_NET_OFFLINE=true cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml"
  TARGET_DIR=$(CARGO_NET_OFFLINE=true cargo metadata --no-deps --format-version 1 --manifest-path "$REPO_ROOT/Cargo.toml" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
  BIN="$TARGET_DIR/release/repodb"
fi
if [[ ! -x "$BIN" ]]; then
  printf 'RepoDB binary is not executable: %s\n' "$BIN" >&2
  exit 2
fi
mkdir -p "$BENCH_ROOT/repo" "$BENCH_ROOT/home/bin" "$BENCH_ROOT/config/repodb"
cat > "$BENCH_ROOT/home/bin/generate_embeddings" <<'PY'
#!/usr/bin/env python3
import sys
payload = sys.stdin.buffer.read()
units = payload.split(b"\0") if "-0" in sys.argv else [payload]
for _ in units:
    sys.stdout.write("[1.0,0.0]\n")
PY
chmod +x "$BENCH_ROOT/home/bin/generate_embeddings"
printf '[tools]\nembedding_command = "%s"\n' \
  "$BENCH_ROOT/home/bin/generate_embeddings" \
  > "$BENCH_ROOT/config/repodb/config.toml"

python3 - "$BENCH_ROOT/repo" "$BENCH_ROOT/imported.txt" "$FILE_COUNT" "$IMPORT_PARAGRAPHS" <<'PY'
from pathlib import Path
import random, string, sys
repo = Path(sys.argv[1])
file_count = int(sys.argv[3])
paragraphs = int(sys.argv[4])
rng = random.Random(124031)
alphabet = string.ascii_letters + string.digits + " _-.,:;"
for i in range(file_count):
    text = f"fixture {i:05d} " + "".join(rng.choices(alphabet, k=2000)) + "\n"
    (repo / f"file-{i:05d}.txt").write_text(text)
with Path(sys.argv[2]).open("w") as output:
    for i in range(paragraphs):
        output.write(f"Document section {i:04d}: " + "".join(rng.choices(alphabet, k=480)) + "\n\n")
PY

cd "$BENCH_ROOT/repo"
git init -q
git config user.email benchmark@example.invalid
git config user.name 'RepoDB Resource Benchmark'
git add .
git commit -qm 'initial file corpus'
python3 - "$BENCH_ROOT/repo" "$UPDATED_COUNT" <<'PY'
from pathlib import Path
import random, string, sys
repo = Path(sys.argv[1])
updated_count = int(sys.argv[2])
rng = random.Random(124032)
alphabet = string.ascii_letters + string.digits + " _-.,:;"
for i in range(updated_count):
    text = f"updated {i:05d} " + "".join(rng.choices(alphabet, k=2000)) + "\n"
    (repo / f"file-{i:05d}.txt").write_text(text)
PY
git add .
git commit -qm "update $UPDATED_COUNT files"

export HOME="$BENCH_ROOT/home"
export XDG_CONFIG_HOME="$BENCH_ROOT/config"
"$BIN" import "$BENCH_ROOT/imported.txt" >/dev/null
/usr/bin/time -v -o "$BENCH_ROOT/init.time" "$BIN" init --size-limit=warn
/usr/bin/time -v -o "$BENCH_ROOT/refresh.time" "$BIN" refresh --size-limit=warn

printf 'benchmark_root=%s\n' "$BENCH_ROOT"
printf 'environment=%s\n' "$(uname -a)"
printf 'tracked_files=%s\n' "$(find "$BENCH_ROOT/repo" -maxdepth 1 -name 'file-*.txt' -type f | wc -l)"
printf 'updated_files=%s\n' "$UPDATED_COUNT"
printf 'import_paragraphs=%s\n' "$IMPORT_PARAGRAPHS"
printf 'repository_source_bytes=%s\n' "$(git ls-tree -r -l HEAD | awk '{sum += $4} END {print sum}')"
printf 'import_bytes=%s\n' "$(stat -c %s "$BENCH_ROOT/imported.txt")"
printf 'database_bytes=%s\n' "$(stat -c %s "$BENCH_ROOT/repo/.git/repodb.sqlite")"
printf 'git_dir_bytes=%s\n' "$(du -sb "$BENCH_ROOT/repo/.git" | cut -f1)"
printf '\nINIT MEASUREMENT\n'
cat "$BENCH_ROOT/init.time"
printf '\nREFRESH MEASUREMENT\n'
cat "$BENCH_ROOT/refresh.time"
printf '\nSTATUS\n'
"$BIN" status --json
python3 - "$BENCH_ROOT/repo/.git/repodb.sqlite" <<'PY'
import sqlite3, sys
connection = sqlite3.connect(sys.argv[1])
for table in ("git_commits", "indexed_units", "unit_embeddings", "imported_sources", "imported_units"):
    count = connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
    print(f"{table}={count}")
PY
