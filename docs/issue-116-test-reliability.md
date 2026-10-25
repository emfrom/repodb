# Issue #116 — All-target test reliability

## Current-tip verification

- Code commit: `c90f4b5481847842d9fa6701a71670e6dd75fff8`; the evidence record was prepared at `781dee493ba48e7d452c56224bab624dc3b6c79c` (documentation-only commits follow the tested code).
- Platform/toolchain: Ubuntu 26.04.1 LTS, Linux x86_64; stable `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)`, Clippy `0.1.99 (b940084d7e 2026-09-28)`.
- A clean per-run Cargo target directory was used: `CARGO_TARGET_DIR=/tmp/repodb-116/target`.

| Command | Result |
|---|---|
| `cargo test --all-targets --all-features` | PASS; 262 passed, 1 ignored. Raw output: `/tmp/repodb-116/default.log`. |
| `cargo test --all-targets --all-features -- --test-threads=1` | PASS; 262 passed, 1 ignored. Raw output: `/tmp/repodb-116/serial.log`. |

## Historical failure and supported fallback

The v0.3.1 review recorded one default-parallel failure in `git_objects::tests::oversized_batch_response_kills_child_before_waiting_for_stdout`: spawning its executable fixture returned `ExecutableFileBusy`. The full evidence and exact tagged-checkout command are in `docs/reviews/standards/repodb-0.3.1.md` under TB-97-2. Subsequent default-parallel runs, including the current clean-target run above, passed; the original failure has not been reproduced and its cause is not established.

The supported conservative fallback is:

```sh
cargo test --all-targets --all-features -- --test-threads=1
```

This disables parallel test threads in each test binary, avoiding concurrent test-thread fixture activity while retaining the complete all-target suite. Both default and serialized full-suite commands pass on the current code. This record does not claim the historical parallel failure was fixed or that its cause is known; it provides the ticket's accepted serialized-command alternative and retains the intermittent failure as a documented risk.
