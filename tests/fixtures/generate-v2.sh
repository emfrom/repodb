#!/bin/sh
# Generate the migration fixture using only the public #4 CLI, never SQL.
set -eu
project=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/source" "$scratch/repository"
git archive 3c58c0e | tar -x -C "$scratch/source"
CARGO_TARGET_DIR=/tmp/repodb-ticket5-target/v2 cargo build --offline --locked --manifest-path "$scratch/source/Cargo.toml"
cd "$scratch/repository"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
git init -q --object-format=sha1
git config user.name Test
git config user.email test@example.com
printf 'v2 first\nv2 marker\nv2 last\n' > v2.txt
git add v2.txt
GIT_AUTHOR_DATE=2000-01-01T00:00:00Z GIT_COMMITTER_DATE=2000-01-01T00:00:00Z git commit -qm v2
printf '#!/bin/sh\ncat >/dev/null\nprintf "[1,0]"\n' > v2-embedder
chmod +x v2-embedder
/tmp/repodb-ticket5-target/v2/debug/repodb init
/tmp/repodb-ticket5-target/v2/debug/repodb reembed --command ./v2-embedder --model v2-model
cp .git/repodb.sqlite "$project/tests/fixtures/v2.sqlite"
git rev-parse HEAD
