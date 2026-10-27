# Default Semgrep parallelism

Status: accepted

For independent source-blob parsing, default to `max(1, floor(available_parallelism / 2))` concurrent Semgrep workers, using the logical parallelism available to the process at runtime; allow a positive user override. We choose this portable heuristic instead of a fixed machine-specific cap or host-by-host tuning: testing the range of modern CPU, SMT, affinity and quota configurations is not practical. The isolated Semgrep measurements support parallelism but do **not** validate this default on every architecture or prove an end-to-end speedup. Keep work queues and captured output bounded, and preserve ordering, fallback and timeout behavior. Revise the heuristic if later evidence shows harm; do not make exhaustive architecture-scaling tests a prerequisite for this decision.
