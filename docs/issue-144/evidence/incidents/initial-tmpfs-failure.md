# Initial paired-run attempt: storage failure

This attempt is retained as an incident record and is **not part of the five-pair comparison**.

- Attempt order: pair 1, baseline v0.3.1 only (before candidate was started).
- Baseline binary: SHA-256 `a7fa5ee72e5ff83419918275c8eb813891a5e942997133ad99bcb2bffb395f9a`.
- RepoDB corpus: `f7cc6bc5451204c5696f1bbef7e5575772820f47`.
- Fresh `init` completed successfully in 400.2098 seconds; index size 107,868,160 bytes, 46,681 indexed units/embeddings.
- The subsequent unchanged full `refresh` exited with `repodb: disk I/O error`; the runner aborted and retained a partial JSON with `unchanged_refresh: null`. The database passes SQLite `PRAGMA integrity_check` and still contains 231 commits/46,681 units.
- The measurement database was under `/tmp` (tmpfs, 7.7 GiB total; 1.7 GiB free before and 1.6 GiB after the failed refresh). `TMPDIR` and `SQLITE_TMPDIR` were set to `/home/emfrom/opt/rust/cargo/target` (the larger local filesystem). The precise source of the SQLite I/O error is unproven; it is treated as a failed measurement, not as a pass or an unfavorable timing ratio.
- Partial raw evidence: [`raw/preliminary-tmpfs/pair-01-before.partial.json`](../raw/preliminary-tmpfs/pair-01-before.partial.json), SHA-256 `7003d6d0d0e95acd588a3669f785bdde55a92dd51b711b973f51fd3426f468f7`.

Per the accepted #134 comparison policy, this partial sample is not selectively replaced or silently dropped. A separate, complete five-pair alternating series is being run with measurement roots on the larger local filesystem, reachable through `/tmp` aliases. All five pairs in that rerun will be retained. If that complete series has another failed or timed-out measurement, the result is inconclusive/fails rather than a speedup claim.
