#!/bin/sh
# Generate the migration fixture using only the public #3 CLI, never SQL.
set -eu
project=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/source" "$scratch/repository"
git archive a1a4e56 | tar -x -C "$scratch/source"
CARGO_TARGET_DIR=/tmp/repodb-ticket4-target/legacy cargo build --locked --manifest-path "$scratch/source/Cargo.toml"
cd "$scratch/repository"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
git init -q --object-format=sha1
git config user.name Test
git config user.email test@example.com
printf 'legacy marker\n' > legacy.txt
git add legacy.txt
GIT_AUTHOR_DATE=2000-01-01T00:00:00Z GIT_COMMITTER_DATE=2000-01-01T00:00:00Z git commit -qm legacy
printf '#!/bin/sh\ncat >/dev/null\nprintf "[1,0]"\n' > legacy-embedder
chmod +x legacy-embedder
/tmp/repodb-ticket4-target/legacy/debug/repodb init
/tmp/repodb-ticket4-target/legacy/debug/repodb reembed --command ./legacy-embedder --model legacy-model
cp .git/repodb.sqlite "$project/tests/fixtures/legacy.sqlite"
git rev-parse HEAD
