#!/usr/bin/env python3
"""Diagnostic only: sample memory at explicit native-parser worker counts."""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import time
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
RUNNER_PATH = HERE.parent / "issue-134" / "run-baseline.py"
SPEC = importlib.util.spec_from_file_location("issue144_refresh_runner", RUNNER_PATH)
assert SPEC and SPEC.loader
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--workers", type=int, nargs="+", default=(1, 4))
    parser.add_argument("--datasets", nargs="+", choices=("repodb", "combined_26"), default=("repodb", "combined_26"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary, root, embedder = map(Path.resolve, (args.binary, args.root, args.embedder))
    daemon_pid_path = Path.home() / ".cache/generate_embeddings/daemon.pid"
    daemon_pid = int(daemon_pid_path.read_text().strip()) if daemon_pid_path.exists() else None
    env = os.environ.copy()
    env.update({"XDG_CONFIG_HOME": str(root / "config"), "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1", "REPODB_REFRESH_TIMEOUT_SECS": "900",
                "RUST_BACKTRACE": "1", "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
    rows: list[dict[str, Any]] = []
    for dataset in args.datasets:
        run = root / dataset / "run-01"
        repo, database = run / "repo", run / "repodb.sqlite"
        for workers in args.workers:
            warm_started = time.perf_counter()
            warm = subprocess.run([str(embedder), "--json", "-0", "-"],
                                  input="issue144 parser worker diagnostic alpha\0issue144 parser worker diagnostic beta",
                                  cwd=repo, env=env, text=True, capture_output=True, timeout=300)
            warmup_seconds = time.perf_counter() - warm_started
            if warm.returncode:
                raise RuntimeError(f"embedder warmup failed: {warm.stderr}")
            command = [str(binary), "--database", str(database), "refresh", "--semgrep-workers", str(workers)]
            measured = runner.run_measured(command, repo, env, daemon_pid, 900)
            rows.append({"dataset": dataset, "workers": workers, "warmup_seconds_excluded": warmup_seconds,
                         **measured})
            print(f"{dataset} workers={workers}: {measured['elapsed_seconds']:.2f}s, "
                  f"peak={measured['sampled_process_tree_peak_rss_kib']} KiB", flush=True)
    result = {
        "schema_version": 1,
        "purpose": "Resource diagnosis only; default-worker gate measurements remain unchanged.",
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "default_workers": max(1, os.cpu_count() // 2) if os.cpu_count() else 1,
        "sample_interval_seconds": 0.05,
        "includes_persistent_embedder_daemon": bool(daemon_pid),
        "measurements": rows,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
