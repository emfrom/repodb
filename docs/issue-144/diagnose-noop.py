#!/usr/bin/env python3
"""Investigate short changed/no-op refresh outliers without replacing gate samples."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--before-root", type=Path, required=True)
    parser.add_argument("--after-root", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=20)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    baseline, candidate, embedder = map(Path.resolve, (args.baseline, args.candidate, args.embedder))
    roots = {"before": args.before_root.resolve(), "after": args.after_root.resolve()}
    measurements: list[dict[str, Any]] = []
    for dataset in ("repodb", "combined_26"):
        for database_owner, root in roots.items():
            run = root / dataset / "run-01"
            repo, database = run / "repo", run / "repodb.sqlite"
            env = os.environ.copy()
            env.update({"XDG_CONFIG_HOME": str(root / "config"), "HF_HUB_OFFLINE": "1",
                        "TRANSFORMERS_OFFLINE": "1", "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
            binaries = {"baseline": baseline, "candidate": candidate}
            for repeat in range(1, args.repeat + 1):
                # Balance order within this database while keeping each pair adjacent.
                order = ("baseline", "candidate") if repeat % 2 else ("candidate", "baseline")
                for label in order:
                    command = [str(binaries[label]), "--database", str(database), "refresh", "--changed"]
                    started = time.perf_counter()
                    completed = subprocess.run(command, cwd=repo, env=env, text=True,
                                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
                    elapsed = time.perf_counter() - started
                    if completed.returncode:
                        raise RuntimeError(f"{dataset}/{database_owner}/{label} failed: {completed.stderr}")
                    measurements.append({"dataset": dataset, "database_owner": database_owner,
                                         "repeat": repeat, "order": list(order), "binary": label,
                                         "elapsed_seconds": elapsed, "returncode": completed.returncode,
                                         "database_bytes": database.stat().st_size,
                                         "stdout_tail": completed.stdout[-500:],
                                         "stderr_tail": completed.stderr[-500:]})
            with __import__("sqlite3").connect(database) as connection:
                integrity = connection.execute("PRAGMA integrity_check").fetchone()[0]
                if integrity != "ok":
                    raise RuntimeError(f"{dataset}/{database_owner}: integrity check failed: {integrity}")

    grouped: dict[str, Any] = {}
    for dataset in ("repodb", "combined_26"):
        grouped[dataset] = {}
        for owner in roots:
            rows = [row for row in measurements if row["dataset"] == dataset and row["database_owner"] == owner]
            baseline_times = [row["elapsed_seconds"] for row in rows if row["binary"] == "baseline"]
            candidate_times = [row["elapsed_seconds"] for row in rows if row["binary"] == "candidate"]
            grouped[dataset][owner] = {
                "baseline_median_seconds": statistics.median(baseline_times),
                "candidate_median_seconds": statistics.median(candidate_times),
                "candidate_over_baseline_median_ratio": statistics.median(candidate_times) / statistics.median(baseline_times),
                "baseline_range_seconds": [min(baseline_times), max(baseline_times)],
                "candidate_range_seconds": [min(candidate_times), max(candidate_times)],
            }
    result = {
        "schema_version": 1,
        "purpose": "Diagnostic only; these repetitions do not replace, filter, or augment the five accepted gate pairs.",
        "repeat_per_binary_per_database": args.repeat,
        "binary_sha256": {"baseline": sha256(baseline), "candidate": sha256(candidate)},
        "balanced_adjacent_order": True,
        "database_crossing": "Run both binaries against each of pair-02's unchanged before and after databases to distinguish binary from database-owner effects.",
        "summary": grouped,
        "measurements": measurements,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(args.output.resolve())
    print(json.dumps(grouped, indent=2))


if __name__ == "__main__":
    main()
