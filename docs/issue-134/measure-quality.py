#!/usr/bin/env python3
"""Run source-grounded exact, semantic, and combined retrieval tasks on v0.3.1."""
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
TASKS_PATH = HERE.parents[1] / "benchmarks/issue-83/tasks.json"


def load_harness() -> Any:
    path = HERE.parents[1] / "benchmarks/issue-83/bench.py"
    spec = importlib.util.spec_from_file_location("issue83_bench", path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def valid_reference(repo: Path, row: dict[str, Any]) -> bool:
    kind = row.get("kind")
    if kind == "content":
        commit, path = row.get("commit_id"), row.get("path")
        start, end = row.get("line_start"), row.get("line_end")
    elif kind == "diff":
        if row.get("side") == "added":
            commit, path = row.get("child_commit_id"), row.get("new_path")
            start, end = row.get("new_line_start"), row.get("new_line_end")
        elif row.get("side") == "removed":
            commit, path = row.get("parent_commit_id"), row.get("old_path")
            start, end = row.get("old_line_start"), row.get("old_line_end")
        else:
            return False
    else:
        return False
    if not isinstance(commit, str) or not isinstance(path, str) or not isinstance(start, int) or not isinstance(end, int):
        return False
    source = subprocess.run(["git", "show", f"{commit}:{path}"], cwd=repo, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if source.returncode:
        return False
    lines = source.stdout.splitlines()
    if not 1 <= start <= end <= len(lines):
        return False
    excerpt = "\n".join(lines[start - 1:end]).strip()
    text = str(row.get("text", "")).strip()
    if not text and any(row.get(flag) is True for flag in (
        "text_omitted_for_size", "head_text_omitted_for_size", "live_text_omitted_for_size"
    )):
        return True
    return bool(text) and (text == excerpt if kind == "content" else text in excerpt)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus-root", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--semgrep-core", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--skip-init", action="store_true", help="reuse the existing pinned quality indexes")
    args = parser.parse_args()
    harness = load_harness()
    tasks_doc = json.loads(TASKS_PATH.read_text(encoding="utf-8"))
    binary, embedder, semgrep = map(Path.resolve, (args.binary, args.embedder, args.semgrep_core))
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = harness.configure_embedding_environment(out / "candidate", embedder, semgrep)
    env.update({"HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
                "REPODB_REFRESH_TIMEOUT_SECS": "900", "RUST_BACKTRACE": "1"})
    warm_start = time.perf_counter()
    warm = subprocess.run([str(embedder), "--json", "-0", "-"], input="quality warmup alpha\0quality warmup beta",
                          cwd=args.corpus_root / "repodb", env=env, text=True,
                          capture_output=True, timeout=300)
    warm_s = time.perf_counter() - warm_start
    if warm.returncode:
        raise RuntimeError(f"embedder warmup failed: {warm.stderr}")
    vectors = [json.loads(line) for line in warm.stdout.splitlines()]
    if len(vectors) != 2 or any(len(vector) != 384 for vector in vectors):
        raise RuntimeError("embedder warmup did not yield two 384-dimensional vectors")
    result: dict[str, Any] = {
        "schema_version": 1,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "task_file_sha256": hashlib.sha256(TASKS_PATH.read_bytes()).hexdigest(),
        "task_corpus_pins": tasks_doc["corpus_pins"],
        "result_limit": 5,
        "response_budget_bytes": harness.BUDGET_BYTES,
        "repeat": args.repeat,
        "process_timeout_seconds": 900,
        "warmup_seconds_excluded": warm_s,
        "corpora": {},
    }
    for corpus, pin in tasks_doc["corpus_pins"].items():
        repo = args.corpus_root / corpus
        if harness._git_head(repo) != pin:
            raise RuntimeError(f"corpus pin mismatch: {corpus} expected {pin}")
        harness.verify_task_source_spans(repo, corpus)
        git_dir = Path(subprocess.run(["git", "rev-parse", "--absolute-git-dir"], cwd=repo,
                                      check=True, text=True, capture_output=True).stdout.strip())
        database = git_dir / "issue134-baseline.sqlite"
        init_s = None
        if not args.skip_init:
            init_cmd = [str(binary), "--database", str(database), "init"]
            started = time.perf_counter()
            init = subprocess.run(init_cmd, cwd=repo, env=env, text=True,
                                  capture_output=True, timeout=3600)
            init_s = time.perf_counter() - started
            if init.returncode:
                raise RuntimeError(f"{corpus} init failed: {init.stderr[-4000:]}\n{init.stdout[-1000:]}")
        elif not database.is_file():
            raise RuntimeError(f"pinned quality index missing: {database}")
        corpus_tasks = [task for task in tasks_doc["tasks"] if task["corpus"] == corpus]
        rows = []
        for mode in ("exact", "semantic", "combined"):
            for task in corpus_tasks:
                runs = []
                for repeat in range(args.repeat):
                    command = [str(binary), "--database", str(database), "search", task["query"],
                               f"--{mode}", "--json", "--limit", "5", "--byte-budget",
                               str(harness.BUDGET_BYTES)]
                    started = time.perf_counter()
                    completed = subprocess.run(command, cwd=repo, env=env, text=True,
                                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
                    elapsed = time.perf_counter() - started
                    if completed.returncode:
                        raise RuntimeError(f"{corpus}/{task['id']}/{mode} failed: {completed.stderr}")
                    raw = harness._parse_results(completed.stdout, task["id"])
                    bounded = harness._apply_common_budget(raw, 5)
                    references_valid = all(valid_reference(repo, row) for row in bounded)
                    if not references_valid:
                        raise RuntimeError(f"{corpus}/{task['id']}/{mode} returned an invalid source reference")
                    runs.append({"elapsed_seconds": elapsed,
                                 "raw_response_bytes": len(completed.stdout.encode()),
                                 "score": harness.score_task(task, bounded, 5),
                                 "all_returned_references_source_valid": references_valid,
                                 "results": bounded})
                rows.append({"task_id": task["id"], "category": task["category"],
                             "mode": mode, "runs": runs})
        result["corpora"][corpus] = {"pin": pin, "database_bytes": database.stat().st_size,
                                     "init_seconds": init_s, "tasks": rows}
        (out / "raw-quality.json").write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n",
                                               encoding="utf-8")
        init_label = "skipped" if init_s is None else f"{init_s:.2f}s"
        print(f"finished quality corpus {corpus}: init={init_label}, tasks={len(rows)}", flush=True)

    print(out / "raw-quality.json")


if __name__ == "__main__":
    main()
