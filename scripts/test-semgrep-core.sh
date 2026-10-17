#!/usr/bin/env bash
set -euo pipefail

# Required real-core gate. Keep ordinary cargo test portable when Semgrep is
# absent, but never let this dedicated command silently skip Semgrep coverage.
export REPODB_REQUIRE_SEMGREP_CORE=1

cargo test --test cli semgrep_corpus::
cargo test --test cli optional_semgrep_core_uses_real_ast_and_falls_back_when_unavailable -- --exact
