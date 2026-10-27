#!/usr/bin/env python3
"""Measure incremental no-change `refresh --changed` on the completed baseline indexes."""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("roots", nargs="+", type=Path,
                        help="baseline run roots (pilot and two-repeat corpus outputs)")
    args = parser.parse_args()
    binary, embedder = args.binary.resolve(), args.embedder.resolve()
    results = {"command": [str(binary), "--database", "<per-run-db>", "refresh", "--changed"],
               "repeat": args.repeat, "measurements": []}
    for run_root in args.roots:
        run_root = run_root.resolve()
        for dataset in ("repodb", "combined_26"):
            dataset_root = run_root / dataset
            if not dataset_root.is_dir():
                continue
            for run_dir in sorted(dataset_root.glob("run-*")):
                repo, database = run_dir / "repo", run_dir / "repodb.sqlite"
                config_root = run_root / "config"
                env = os.environ.copy()
                env.update({"XDG_CONFIG_HOME": str(config_root), "HF_HUB_OFFLINE": "1",
                            "TRANSFORMERS_OFFLINE": "1", "REPODB_REFRESH_TIMEOUT_SECS": "900",
                            "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
                for repeat in range(1, args.repeat + 1):
                    print(f"{dataset}/{run_dir.name} changed-noop {repeat}/{args.repeat}", flush=True)
                    command = [str(binary), "--database", str(database), "refresh", "--changed"]
                    started = time.perf_counter()
                    completed = subprocess.run(command, cwd=repo, env=env, text=True,
                                               capture_output=True, timeout=3600)
                    elapsed = time.perf_counter() - started
                    if completed.returncode:
                        raise RuntimeError(f"no-change refresh failed: {completed.stderr}")
                    measurement = {"command": command, "elapsed_seconds": elapsed,
                                   "returncode": completed.returncode,
                                   "database_bytes": database.stat().st_size,
                                   "dataset": dataset, "run": run_dir.name, "repeat": repeat,
                                   "stdout_tail": completed.stdout[-1000:],
                                   "stderr_tail": completed.stderr[-1000:]}
                    results["measurements"].append(measurement)
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
