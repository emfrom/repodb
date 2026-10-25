# Issue #120 — v0.3.1 verification evidence

- Target: clean `v0.3.1` tag checkout, `/tmp/repodb-030`, commit `f7cc6bc5451204c5696f1bbef7e5575772820f47` (verified exact tag; checkout remained clean).
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

For this v0.3.1 verification, the supported platform scope is Linux x86_64, verified on Ubuntu 26.04.1 LTS. Windows, macOS, and other architectures are explicitly out of scope (N/A), not unverified support targets. `.github/workflows/semgrep-core.yml` selects Ubuntu 24.04 for a Semgrep-only job; that is not full RepoDB build/test evidence. This is the support scope for this evidence record, not a claim about release-era platform validation. The initial missing deny policy/configuration was a dependency/license verification blocker; see the follow-up below.

## Addendum — cargo-deny policy and exact-tag verification

After the initial check, `deny.toml` was added to the integration branch using the repository's published license and dependency graph, and the full standard's requirements for configured dependency checks, no silent suppressions, and traceable exceptions. The config is limited to the declared `x86_64-unknown-linux-gnu` verification target and enables all features.

- **License policy:** allow `GPL-2.0-only` (the project license in `Cargo.toml` and `LICENSE`), `MIT`, and `Unicode-3.0`. The current locked graph's OR expressions have an MIT choice; an observed AND expression also requires Unicode-3.0. Apache-2.0, BSD-2-Clause, and Unlicense are not blanket-allowed. No per-crate license exceptions or clarifications are configured. This is cargo-deny's allowlist, not a legal opinion on compatibility of combined works.
- **Advisories:** no ignored advisory IDs (`ignore = []`); default cargo-deny advisory handling remains in force.
- **Dependency policy:** unknown registries and Git sources are denied; crates.io is the only allowed registry. Wildcard version requirements are denied. Duplicate versions warn rather than fail, so maintainers see them without mechanically rejecting semver-major coexistence.
- **Exact-tag command/result:** on clean `/tmp/repodb-030` at `f7cc6bc5451204c5696f1bbef7e5575772820f47`, the config was copied in temporarily and `/home/emfrom/opt/rust/cargo/bin/cargo-deny check` (equivalent `cargo deny check`) exited **0**: advisories, bans, licenses, and sources all reported `ok`. It emitted the configured duplicate-version warning for `hashbrown 0.14.5` and `0.17.1`, with dependency paths shown; no warning was suppressed. The temporary config was removed afterward and the checkout was confirmed clean. Current integration `Cargo.toml` and `Cargo.lock` are identical to the v0.3.1 tag, and the same command also passed on the current branch.

This supersedes the initial “not configured” result for the proposed checked-in policy. The check establishes conformance to this configured allowlist; it does not establish legal compatibility, broaden platform support, or itself constitute release sign-off.
