#!/usr/bin/env bash
set -euo pipefail

# Required real-core gate. The corpus and adapter tests fail closed if the
# pinned Semgrep core is unavailable or has the wrong version.
cargo test --test cli semgrep_corpus::
cargo test --test cli semgrep_core_is_required_and_uses_real_ast -- --exact
