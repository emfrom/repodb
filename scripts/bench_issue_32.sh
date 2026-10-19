#!/usr/bin/env bash
# Measure Git-backed searches against already initialized before/after databases.
# See docs/issue-32-benchmark.md for fixture and database preparation.
set -euo pipefail
if [[ $# -ne 4 ]]; then
  echo "usage: $0 BEFORE_BINARY AFTER_BINARY BEFORE_REPO AFTER_REPO" >&2
  exit 2
fi
before_bin=$(realpath "$1")
after_bin=$(realpath "$2")
before_repo=$(realpath "$3")
after_repo=$(realpath "$4")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
git_path=$(command -v git)
cat > "$tmp/bin/git" <<'WRAPPER'
#!/bin/sh
case " $* " in
  *" cat-file "*) printf '%s\n' "$*" >> "$REPODB_GIT_CALL_LOG" ;;
esac
exec "$REPODB_BENCH_REAL_GIT" "$@"
WRAPPER
chmod +x "$tmp/bin/git"
export REPODB_BENCH_REAL_GIT="$git_path"

run_case() {
  local name=$1 binary=$2 repo=$3 sample=$4 query=$5; shift 5
  local log="$tmp/${name}-${sample}.gitlog"
  local timing="$tmp/${name}-${sample}.time"
  rm -f "$log"
  export REPODB_GIT_CALL_LOG="$log"
  (cd "$repo" && /usr/bin/time -f 'elapsed_seconds=%e peak_rss_kb=%M' -o "$timing" \
    env PATH="$tmp/bin:$PATH" "$binary" search "$query" "$@" --json >/dev/null)
  local calls=0
  [[ -f "$log" ]] && calls=$(wc -l < "$log")
  printf '%-12s sample=%s git_cat_file_invocations=%s ' "$name" "$sample" "$calls"
  cat "$timing"
}

for side in before after; do
  if [[ $side == before ]]; then binary=$before_bin; repo=$before_repo; else binary=$after_bin; repo=$after_repo; fi
  export XDG_CONFIG_HOME="$repo/.xdg"
  for sample in 1 2 3; do
    run_case exact-common "$binary" "$repo" "$sample" use
    run_case exact-absent "$binary" "$repo" "$sample" unlikely_literal_abc987
    run_case semantic-topn "$binary" "$repo" "$sample" 'how does repository search work' --semantic --semantic-limit 10
    run_case combined "$binary" "$repo" "$sample" use --combined --semantic-limit 10
    run_case all-history "$binary" "$repo" "$sample" use --all-history
    # This query also hydrates Git units from the registered imported repository.
    run_case imported-git "$binary" "$repo" "$sample" use
  done
done
