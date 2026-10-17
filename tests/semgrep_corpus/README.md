# Semgrep language corpus (provisional)

This corpus is a provisional 25-language candidate set for issue #27; the final language/ranking decision and shared program semantics are pending in issue #28. It currently contains 50 checked-in source files (two per language), each 200–229 lines. The fixtures sketch the same small in-memory entity registry, audit-event type, and create/find/archive API. The primary entity name has a `V1` suffix so the history integration test can rename it to `V2` in a second commit.

The language list is intentionally a candidate, not a claim that these are objectively the top 25 by a particular popularity ranking. It is drawn from the Semgrep v1.181.0 OSS parser set. Move is represented as one high-level language using both Sui and Aptos parser alternatives; Rust uses RepoDB's built-in `syn` path.

`generate_fixtures.py` deterministically regenerates `sources/<language>/registry_{alpha,beta}.<extension>`. The CLI tests consume these checked-in fixtures, use fresh repositories/databases and fake tools, and don't require network access or an installed Semgrep binary. The fake core tests RepoDB's routing/indexing/history seam, not Semgrep's own parsing; real-core v1.181.0 probes were run manually against all 50 fixture files.
