#!/usr/bin/env python3
"""Reproducible RepoDB issue #83 retrieval benchmark and common scorer."""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

BUDGET_BYTES = 5000
PIN_BY_CORPUS = {
    "repodb": "13639d6dbfed410bfa513f95861556acdf32cb5d",
    "protonsync_ts": "7c77aab91aff586591a00092a19760d5dc15e90a",
}


def _path(value: Any) -> str | None:
    if isinstance(value, str):
        return value.replace("\\", "/").lstrip("./")
    return None


def _references(result: dict[str, Any]) -> list[tuple[str, int | None, int | None]]:
    refs: list[tuple[str, int | None, int | None]] = []
    values = [result]
    extra = result.get("references")
    if isinstance(extra, list):
        values.extend(item for item in extra if isinstance(item, dict))
    for item in values:
        path = _path(item.get("path") or item.get("file_path"))
        if path is None:
            continue
        start = item.get("start_line", item.get("line_start", item.get("line")))
        end = item.get("end_line", item.get("line_end", start))
        refs.append((path, start if isinstance(start, int) else None,
                     end if isinstance(end, int) else None))
    return refs


def _result_text(result: dict[str, Any]) -> str:
    fields = ("text", "snippet", "content", "message", "matched_text")
    return " ".join(str(result.get(key, "")) for key in fields).casefold()


def _matches_reference(result: dict[str, Any], expected: dict[str, Any]) -> bool:
    for path, start, end in _references(result):
        wanted = expected["path"].replace("\\", "/")
        if path != wanted and not path.endswith("/" + wanted):
            continue
        if start is None or end is None:
            continue
        if start <= expected["end_line"] and end >= expected["start_line"]:
            return True
    return False


def score_task(task: dict[str, Any], results: list[dict[str, Any]], k: int) -> dict[str, Any]:
    top = results[:k]
    refs = task["expected_references"]
    retrieved = [any(_matches_reference(result, ref) for result in top) for ref in refs]
    answer_terms = task["answer_terms"]
    joined = " ".join(_result_text(result) for result in top)
    return {
        "id": task["id"],
        "category": task["category"],
        "recall_at_k": sum(retrieved) / len(refs) if refs else 0.0,
        "source_reference_correct": bool(retrieved and all(retrieved)),
        "answer_correct": bool(answer_terms) and all(term.casefold() in joined for term in answer_terms),
        "returned_results": len(top),
    }


def _git_head(repo: Path) -> str:
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(f"not a usable Git repository: {repo}: {result.stderr.strip()}")
    return result.stdout.strip()


def verify_task_source_spans(repo: Path, corpus: str) -> None:
    task_data = json.loads((Path(__file__).with_name("tasks.json")).read_text())
    if task_data.get("corpus_pins", {}).get(corpus) != PIN_BY_CORPUS[corpus]:
        raise RuntimeError(f"task corpus pin metadata does not match {corpus} pin")
    for task in (row for row in task_data["tasks"] if row["corpus"] == corpus):
        for reference in task["expected_references"]:
            source = (repo / reference["path"]).resolve()
            if not source.is_relative_to(repo.resolve()):
                raise RuntimeError(f"task {task['id']} has a source path outside its corpus")
            pinned = subprocess.run(["git", "show", f"{PIN_BY_CORPUS[corpus]}:{reference['path']}"],
                                    cwd=repo, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if pinned.returncode:
                raise RuntimeError(f"task {task['id']} source is absent at pin: {pinned.stderr.strip()}")
            lines = pinned.stdout.splitlines()
            start, end = reference["start_line"], reference["end_line"]
            if start < 1 or end < start or end > len(lines) or not any(line.strip() for line in lines[start - 1:end]):
                raise RuntimeError(f"task {task['id']} has an invalid/empty source span {reference}")


def _run(command: list[str], cwd: Path, env: dict[str, str], timeout: int) -> tuple[subprocess.CompletedProcess[str], float, int | None]:
    time_bin = shutil.which("/usr/bin/time")
    wrapped = [time_bin, "-f", "\nISSUE83_PEAK_RSS_KB=%M", "--", *command] if time_bin else command
    start = time.perf_counter()
    result = subprocess.run(wrapped, cwd=cwd, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
    elapsed_ms = (time.perf_counter() - start) * 1000
    match = re.search(r"ISSUE83_PEAK_RSS_KB=(\d+)", result.stderr)
    stderr = re.sub(r"\nISSUE83_PEAK_RSS_KB=\d+\s*$", "", result.stderr)
    result.stderr = stderr
    return result, elapsed_ms, int(match.group(1)) if match else None


def configure_embedding_environment(output_dir: Path, embedder: Path, semgrep_core: Path | None) -> dict[str, str]:
    """Isolate RepoDB config, but retain HOME so built-in embedder detection stays intact."""
    env = os.environ.copy()
    home = env.get("HOME")
    if not home:
        raise RuntimeError("HOME must be set for the direct built-in embedder benchmark")
    config = output_dir / ".config" / "repodb"
    config.mkdir(parents=True, exist_ok=True)
    tools = f'embedding_command = "{embedder}"\n'
    if semgrep_core:
        tools += f'semgrep_core = "{semgrep_core}"\n'
    (config / "config.toml").write_text(f'[tools]\n{tools}', encoding="utf-8")
    env.update({"XDG_CONFIG_HOME": str(output_dir / ".config"),
                "HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
                "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
    return env


def _parse_results(stdout: str, context: str) -> list[dict[str, Any]]:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{context}: CLI did not return JSON: {error}") from error
    if isinstance(payload, list):
        results = payload
    elif isinstance(payload, dict) and isinstance(payload.get("results"), list):
        results = payload["results"]
    else:
        raise RuntimeError(f"{context}: unexpected JSON result shape")
    return [row for row in results if isinstance(row, dict)]


def _apply_common_budget(results: list[dict[str, Any]], limit: int) -> list[dict[str, Any]]:
    """Common evaluator truncates only between complete serialized result objects."""
    selected: list[dict[str, Any]] = []
    for result in results[:limit]:
        proposed = selected + [result]
        envelope = {"results": proposed}
        if len(json.dumps(envelope, ensure_ascii=False, separators=(",", ":")).encode()) > BUDGET_BYTES:
            if not selected:
                raise RuntimeError("first complete result exceeds the common 5,000-byte response budget")
            break
        selected = proposed
    return selected


def _check_binary_semantics(binary: Path, repo: Path, db: Path | None, env: dict[str, str], candidate: bool) -> None:
    if candidate:
        command = [str(binary), "--database", str(db), "status", "--json"]
    else:
        # v0.2.0 has no global --database or status command; its database is
        # isolated by the separate Git directory and its semantic probe below.
        command = [str(binary), "search", "__issue83_embedding_capability_probe__", "--semantic", "--json", "--semantic-limit", "1"]
    result = subprocess.run(command, cwd=repo, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if candidate:
        if result.returncode:
            raise RuntimeError(f"candidate status failed: {result.stderr.strip()}")
        try:
            status = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError("candidate status did not return JSON") from error
        semantic = status.get("capabilities", {}).get("semantic", {})
        if not semantic.get("ready", False):
            raise RuntimeError(f"candidate index/embeddings unavailable: {semantic}")
    elif result.returncode and re.search(r"no embeddings|missing embedding|run (init|refresh)", result.stderr, re.I):
        raise RuntimeError(f"baseline index/embeddings unavailable: {result.stderr.strip()}")
    # A no-hit semantic probe is valid; unsupported mode/errors are not.
    elif result.returncode:
        raise RuntimeError(f"baseline semantic capability probe failed: {result.stderr.strip()}")


def run_variant(name: str, binary: Path, root: Path, embedder: Path, semgrep_core: Path | None, output_dir: Path,
                refresh: bool, repeat: int, limit: int, timeout: int) -> dict[str, Any]:
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError(f"{name} binary is missing or not executable: {binary}")
    if not root.is_dir():
        raise RuntimeError(f"{name} corpus root is missing: {root}")
    env = configure_embedding_environment(output_dir / name, embedder, semgrep_core)
    if name == "candidate" and embedder.resolve() != (Path(env["HOME"]) / "bin/generate_embeddings").resolve():
        raise RuntimeError("candidate batch path requires the real HOME/bin/generate_embeddings executable")
    corpus_runs: dict[str, Any] = {}
    for corpus, pin in PIN_BY_CORPUS.items():
        repo = root / corpus
        if _git_head(repo) != pin:
            raise RuntimeError(f"{name}/{corpus} pin mismatch: expected {pin}, got {_git_head(repo)}")
        verify_task_source_spans(repo, corpus)
        git_dir = subprocess.run(["git", "rev-parse", "--absolute-git-dir"], cwd=repo,
                                 check=True, text=True, stdout=subprocess.PIPE).stdout.strip()
        db = Path(git_dir) / ("issue83-benchmark.sqlite" if name == "candidate" else "repodb.sqlite")
        if refresh:
            command = ([str(binary), "--database", str(db), "init"] if name == "candidate"
                       else [str(binary), "init"])
            refreshed, refresh_ms, refresh_rss = _run(command, repo, env, timeout)
            if refreshed.returncode:
                raise RuntimeError(f"{name}/{corpus} refresh failed ({refreshed.returncode}): {refreshed.stderr.strip()}\n{refreshed.stdout[-2000:]}")
        else:
            refresh_ms = None
            refresh_rss = None
        if name == "candidate" and not db.is_file():
            raise RuntimeError(f"candidate index missing at {db}; run with --refresh")
        if name == "baseline" and not (Path(git_dir) / "repodb.sqlite").is_file() and not db.is_file():
            raise RuntimeError(f"baseline index missing under {git_dir}; run with --refresh")
        _check_binary_semantics(binary, repo, db if name == "candidate" else None, env, name == "candidate")
        task_results = []
        tasks = json.loads((Path(__file__).with_name("tasks.json")).read_text())["tasks"]
        for task in (task for task in tasks if task["corpus"] == corpus):
            repetitions = []
            for run_number in range(repeat):
                if name == "candidate":
                    command = [str(binary), "--database", str(db), "search", task["query"],
                               "--json", "--limit", str(limit), "--byte-budget", str(BUDGET_BYTES)]
                else:
                    command = [str(binary), "search", task["query"], "--combined", "--json", "--semantic-limit", "20"]
                completed, elapsed_ms, peak_rss = _run(command, repo, env, timeout)
                if completed.returncode:
                    message = completed.stderr.strip()
                    if re.search(r"no embeddings|missing embedding", message, re.I):
                        raise RuntimeError(f"{name} index exists but embeddings are missing for task {task['id']}: {message}")
                    raise RuntimeError(f"{name} search failed for {task['id']} ({completed.returncode}): {message}")
                raw = _parse_results(completed.stdout, f"{name} task {task['id']}")
                results = _apply_common_budget(raw, limit)
                repetitions.append({"condition": "first-process-run" if run_number == 0 else "warm-process-run",
                                    "elapsed_ms": elapsed_ms, "peak_rss_kb": peak_rss,
                                    "raw_response_bytes": len(completed.stdout.encode()),
                                    "evaluator_response_bytes": len(json.dumps({"results": results}, ensure_ascii=False, separators=(",", ":")).encode()),
                                    "score": score_task(task, results, limit),
                                    "results": results})
            task_results.append({"id": task["id"], "runs": repetitions})
        corpus_runs[corpus] = {
            "pin": pin,
            "index_bytes": db.stat().st_size if db.exists() else (Path(git_dir) / "repodb.sqlite").stat().st_size,
            "refresh_ms": refresh_ms,
            "refresh_peak_rss_kb": refresh_rss,
            "refresh_embedding": None,
            "query_embedding": None,
            "tasks": task_results,
        }
    return {"binary": str(binary), "corpora": corpus_runs,
            "conditions": {"first_run": "first invocation in a fresh process; OS page cache was not dropped",
                           "warm_run": "subsequent fresh-process invocation; OS cache may be warm; no cache dropping claimed",
                           "response_budget_bytes": BUDGET_BYTES, "result_limit": limit}}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-bin", type=Path, required=True)
    parser.add_argument("--candidate-bin", type=Path, required=True)
    parser.add_argument("--baseline-root", type=Path, required=True,
                        help="directory containing separate repodb/ and protonsync_ts/ checkouts at the pinned commits")
    parser.add_argument("--candidate-root", type=Path, required=True,
                        help="separate directory containing the same two pinned checkouts")
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--semgrep-core", type=Path, help="installed semgrep-core executable required by pinned RepoDB builds")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--refresh", action="store_true", help="initialize each isolated index with offline real embeddings before querying")
    parser.add_argument("--repeat", type=int, default=2)
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=3600)
    args = parser.parse_args()
    if args.repeat < 1 or args.limit < 1:
        parser.error("--repeat and --limit must be positive")
    if not args.embedder.is_file() or not os.access(args.embedder, os.X_OK):
        parser.error(f"real embedder missing or not executable: {args.embedder}")
    if args.semgrep_core and (not args.semgrep_core.is_file() or not os.access(args.semgrep_core, os.X_OK)):
        parser.error(f"semgrep-core missing or not executable: {args.semgrep_core}")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    try:
        result = {
            "schema_version": 2,
            "pins": PIN_BY_CORPUS,
            "baseline": run_variant("baseline", args.baseline_bin.resolve(), args.baseline_root.resolve(), args.embedder.resolve(), args.semgrep_core.resolve() if args.semgrep_core else None, output.parent, args.refresh, args.repeat, args.limit, args.timeout),
            "candidate": run_variant("candidate", args.candidate_bin.resolve(), args.candidate_root.resolve(), args.embedder.resolve(), args.semgrep_core.resolve() if args.semgrep_core else None, output.parent, args.refresh, args.repeat, args.limit, args.timeout),
            "limitations": [
                "The v0.2.0 baseline has no --database option; it is isolated by distinct Git directories and the common evaluator layer.",
                "Baseline has no native response-byte budget; both outputs are capped by the same evaluator at complete result boundaries after CLI execution.",
                "Cold means first fresh-process query only; operating-system caches were not dropped.",
                "Stale worktree coverage is not exercised by this source-retrieval suite; stale behavior is represented by a source-fact task only.",
                "The embedder is invoked directly with its original HOME; no wrapper, input rewrite or process logging changes its batch path. Embedding subprocess call count and stdin/stdout bytes are unmeasured, not zero.",
                "The v0.2.0 baseline may fail on whitespace-only stdin units; do not rewrite inputs or report incomplete initialization as a comparable baseline."
            ],
        }
        output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    except (RuntimeError, subprocess.TimeoutExpired, OSError, json.JSONDecodeError) as error:
        print(f"issue-83 benchmark blocked: {error}", file=sys.stderr)
        return 2
    print(f"wrote benchmark report: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
