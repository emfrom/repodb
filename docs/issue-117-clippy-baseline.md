# Issue #117 — Strict Clippy baseline

This records the strict Clippy gate for RepoDB 0.3.1-era code after addressing the findings in `TB-97-1`.

## Verified target

- Code commit: `c90f4b5481847842d9fa6701a71670e6dd75fff8` (`c90f4b5`), on `integration/repodb-spec`.
- Platform: Ubuntu 26.04.1 LTS, Linux x86_64 (`7.0.0-38-generic`).
- Toolchain: stable `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)`, Clippy `0.1.99 (b940084d7e 2026-09-28)`.
- **Supported-toolchain policy for this quality gate:** RepoDB supports the current stable Rust toolchain for build, test, and strict Clippy validation. This is not an MSRV promise; no lower-version compatibility claim is made. At the recorded validation date, `stable` resolved to Rust/Clippy 1.99.0 above. The repository's existing GitHub workflow also selects `dtolnay/rust-toolchain@stable`.
- Platform tested for this run: Ubuntu 26.04.1 LTS, Linux x86_64. This line records the execution environment, not a broader platform-support declaration.

## Results

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --all-targets --all-features -- -D warnings` | PASS; no diagnostics |
| `cargo test --all-targets --all-features` | PASS; 262 passed, 1 ignored |
| `git diff --check` | PASS |

No lint suppressions or exceptions were added. The three too-many-arguments findings in `src/history.rs` were addressed by grouping related state and range data into private structs; the remaining changes are behavior-preserving Clippy suggestions.

The test result is a warm-target run on the verified code commit, not evidence about the release-era toolchain. See the separate #116 record for clean/default and serialized test execution evidence.
