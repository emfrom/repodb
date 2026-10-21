# Issue #120 — v0.3.0 verification evidence

- Target: clean `v0.3.0` tag checkout, `/tmp/repodb-030`, commit `f7cc6bc5451204c5696f1bbef7e5575772820f47` (verified exact tag; checkout remained clean).
- Run: 2026-10-21 UTC, Linux x86_64, Ubuntu 26.04.1 LTS; `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)`, cargo-audit 0.22.2, cargo-deny 0.20.2. Tool executables were already installed; no installations performed.
- Build/test target: fresh `/tmp/repodb-120-target` via `CARGO_TARGET_DIR` (absent before run).

## Results

| Command | Exit | Result |
|---|---:|---|
| `CARGO_TARGET_DIR=/tmp/repodb-120-target cargo build --release --all-targets` | 0 | Passed; optimized all-target release build completed. |
| `CARGO_TARGET_DIR=/tmp/repodb-120-target cargo test --all-targets --all-features` | 0 | Passed; all executed targets/tests passed; one worktree race-child test ignored. |
| `/home/emfrom/opt/rust/cargo/bin/cargo-audit audit` | 0 | Passed; scanned 52 locked crate dependencies; no vulnerability findings printed. Advisory DB updated during the run. |
| `/home/emfrom/opt/rust/cargo/bin/cargo-deny check` | 4 | **Blocked / not configured:** no deny config exists in the checkout. cargo-deny warned that it was falling back to its default configuration and rejected the graph for unallowed licenses (including project GPL-2.0-only and dependency licenses); advisories, bans, and sources reported `ok`. No policy was invented, and this result is not represented as a policy pass. |

Raw stdout/stderr and exit status are preserved outside the repository in `/tmp/issue120-{build,tests,audit,deny}.log` and corresponding `.status` files. An initial `cargo-audit audit --locked` invocation was invalid for cargo-audit 0.22.2 (`--locked` is unsupported; exit 2); the supported `cargo-audit audit` command was rerun and passed as recorded above.

## Platform declaration and limitation

For this v0.3.0 verification, the supported platform scope is declared as Linux x86_64, verified on Ubuntu 26.04.1 LTS. No Windows, macOS, or other-architecture support is claimed. `.github/workflows/semgrep-core.yml` selects Ubuntu 24.04 for a Semgrep-only job; that is not full RepoDB build/test evidence. This is the support scope for this evidence record, not a claim about release-era platform validation. The missing owner-approved deny policy/configuration remains a dependency/license verification blocker requiring owner disposition; this evidence does not close Issue #120 or authorize a release decision.
