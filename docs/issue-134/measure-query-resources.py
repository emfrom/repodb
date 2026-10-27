#!/usr/bin/env python3
"""Sample RepoDB plus embedder-daemon peak RSS for the task-suite queries."""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNNER = HERE / "run-baseline.py"
SPEC = importlib.util.spec_from_file_location("issue134_refresh_runner", RUNNER)
assert SPEC and SPEC.loader
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)
TASKS = HERE.parents[1] / "benchmarks/issue-83/tasks.json"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--semgrep-core", type=Path, required=True)
    parser.add_argument("--corpus-root", type=Path, required=True)
    parser.add_argument("--config-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary, embedder = args.binary.resolve(), args.embedder.resolve()
    tasks_doc = json.loads(TASKS.read_text(encoding="utf-8"))
    pidfile = Path.home() / ".cache/generate_embeddings/daemon.pid"
    daemon_pid = int(pidfile.read_text().strip()) if pidfile.exists() else None
    env = os.environ.copy()
    env.update({"XDG_CONFIG_HOME": str(args.config_root.resolve()), "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1", "REPODB_REFRESH_TIMEOUT_SECS": "900",
                "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
    rows = []
    for corpus, pin in tasks_doc["corpus_pins"].items():
        repo = args.corpus_root.resolve() / corpus
        git_dir = Path(subprocess.run(["git", "rev-parse", "--absolute-git-dir"], cwd=repo,
                                      check=True, text=True, capture_output=True).stdout.strip())
        database = git_dir / "issue134-baseline.sqlite"
        for task in (item for item in tasks_doc["tasks"] if item["corpus"] == corpus):
            for mode in ("exact", "semantic", "combined"):
                command = [str(binary), "--database", str(database), "search", task["query"],
                           f"--{mode}", "--json", "--limit", "5", "--byte-budget", "5000"]
                measured = runner.run_measured(command, repo, env, daemon_pid, 300)
                rows.append({"corpus": corpus, "pin": pin, "task_id": task["id"], "mode": mode,
                             "elapsed_seconds": measured["elapsed_seconds"],
                             "sampled_process_tree_peak_rss_kib": measured["sampled_process_tree_peak_rss_kib"]})
        print(f"measured query resources: {corpus}", flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"sample_interval_seconds": 0.05,
                                       "includes_persistent_embedder_daemon": bool(daemon_pid),
                                       "measurements": rows}, indent=2) + "\n", encoding="utf-8")
    print(args.output.resolve())


if __name__ == "__main__":
    main()
