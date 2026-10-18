#!/usr/bin/env bash
set -euo pipefail

# Resolve the core from the Semgrep Python package explicitly. uv's temporary
# environment puts its console script on PATH, but its package layout is not
# reliably discoverable from that script's location by the Rust test helper.
if [[ -z "${REPODB_SEMGREP_CORE:-}" ]]; then
    core=$(python -c 'from pathlib import Path; import semgrep; print(Path(semgrep.__file__).parent / "bin" / "semgrep-core")' 2>/dev/null || true)
    if [[ -x "$core" ]]; then
        export REPODB_SEMGREP_CORE="$core"
    fi
fi

# Required real-core gate. The corpus and adapter tests fail closed if the
# pinned Semgrep core is unavailable or has the wrong version.
cargo test --test cli semgrep_corpus::
cargo test --test cli semgrep_core_is_required_and_uses_real_ast -- --exact
