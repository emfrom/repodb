#!/usr/bin/env python3
"""Run the released v0.3.0 candidate-only baseline with the issue-83 scorer.

This measures one pinned release against the source-derived task set. It is not
an old/new comparison and does not turn the v0.3.0 quality/performance waiver
into a pass.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import platform
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

RELEASE_TAG = "v0.3.0"
RELEASE_COMMIT = "f7cc6bc5451204c5696f1bbef7e5575772820f47"
EMBEDDER_SOURCE_COMMIT = "938d9ed73402249e5ac407afed6e2159bc73dc9e"
MODEL_REVISION = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41"
CORPUS_PINS = {
    "repodb": "13639d6dbfed410bfa513f95861556acdf32cb5d",
    "protonsync_ts": "7c77aab91aff586591a00092a19760d5dc15e90a",
}


def command(args: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, check=False)


def require_command(args: list[str], cwd: Path | None = None) -> str:
    result = command(args, cwd)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(args)}\n{result.stdout}")
    return result.stdout.strip()


def load_harness() -> Any:
    path = Path(__file__).resolve().parents[1] / "issue-83" / "bench.py"
    spec = importlib.util.spec_from_file_location("issue83_bench", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load shared benchmark harness: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_source(source: Path) -> dict[str, str]:
    head = require_command(["git", "rev-parse", "HEAD"], source)
    tagged = require_command(["git", "rev-parse", f"{RELEASE_TAG}^{{commit}}"], source)
    exact_tag = require_command(["git", "describe", "--tags", "--exact-match", "HEAD"], source)
    dirty = require_command(["git", "status", "--porcelain"], source)
    if head != RELEASE_COMMIT or tagged != RELEASE_COMMIT or exact_tag != RELEASE_TAG:
        raise RuntimeError(f"source checkout is not exact {RELEASE_TAG} ({RELEASE_COMMIT}): HEAD={head}, tag={tagged}, describe={exact_tag}")
    if dirty:
        raise RuntimeError("release source checkout is dirty")
    return {"tag": exact_tag, "commit": head, "worktree": "clean"}


def warm_embedder(embedder: Path, corpus_root: Path, config_home: Path) -> dict[str, Any]:
    config_home.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update({"HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
                "REPODB_EMBEDDING_MODEL": "all-MiniLM-L6-v2",
                "XDG_CONFIG_HOME": str(config_home),
                "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
    payload = b"issue119 baseline warmup one\0issue119 baseline warmup two"
    started = time.perf_counter()
    result = subprocess.run([str(embedder), "--json", "-0", "-"], cwd=corpus_root / "repodb",
                            env=env, input=payload, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=300, check=False)
    elapsed_ms = (time.perf_counter() - started) * 1000
    if result.returncode:
        raise RuntimeError(f"real embedder warmup failed ({result.returncode}): {result.stderr.decode(errors='replace')}")
    try:
        vectors = [json.loads(line) for line in result.stdout.splitlines()]
    except json.JSONDecodeError as error:
        raise RuntimeError(f"real embedder warmup returned invalid JSON: {error}") from error
    if len(vectors) != 2 or any(len(vector) != 384 or not all(math.isfinite(value) for value in vector)
                                or not any(vector) for vector in vectors):
        raise RuntimeError("real embedder warmup did not return two finite nonzero 384-dimensional vectors")
    return {"command": [str(embedder), "--json", "-0", "-"], "model": "all-MiniLM-L6-v2",
            "vector_count": len(vectors), "dimension": 384, "elapsed_ms_excluded_from_refresh": elapsed_ms,
            "purpose": "pre-warm the actual local model service; not part of refresh or query timings"}


def benchmark_assets() -> dict[str, str]:
    project_root = Path(__file__).resolve().parents[2]
    harness_path = Path(__file__).resolve().parents[1] / "issue-83" / "bench.py"
    tasks_path = harness_path.with_name("tasks.json")
    commit = require_command(["git", "rev-parse", "HEAD"], project_root)
    unchanged = command(["git", "diff", "--quiet", "HEAD", "--", str(harness_path.relative_to(project_root)),
                         str(tasks_path.relative_to(project_root))], project_root)
    if unchanged.returncode:
        raise RuntimeError("shared benchmark harness/task file differ from the pinned repository commit")
    return {"repository_commit": commit,
            "harness_sha256": hashlib.sha256(harness_path.read_bytes()).hexdigest(),
            "tasks_sha256": hashlib.sha256(tasks_path.read_bytes()).hexdigest(),
            "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}


def embedder_provenance(embedder: Path, home: Path) -> dict[str, Any]:
    source_root = Path(require_command(["git", "rev-parse", "--show-toplevel"], embedder.parent))
    source_commit = require_command(["git", "rev-parse", "HEAD"], source_root)
    if source_commit != EMBEDDER_SOURCE_COMMIT:
        raise RuntimeError(f"embedder source commit mismatch: expected {EMBEDDER_SOURCE_COMMIT}, got {source_commit}")
    status = require_command(["git", "status", "--porcelain"], source_root)
    daemon = embedder.with_name("generate_embeddings.daemon")
    if not daemon.is_file():
        raise RuntimeError(f"embedder daemon script missing: {daemon}")
    model_cache = Path(os.environ.get("HF_HOME", str(home / ".cache/huggingface")))
    model_ref = model_cache / "hub/models--sentence-transformers--all-MiniLM-L6-v2/refs/main"
    if not model_ref.is_file():
        raise RuntimeError(f"pinned model reference missing: {model_ref}")
    model_revision = model_ref.read_text(encoding="utf-8").strip()
    if model_revision != MODEL_REVISION:
        raise RuntimeError(f"model revision mismatch: expected {MODEL_REVISION}, got {model_revision}")
    return {"source_commit": source_commit, "worktree_status": status or "clean",
            "cli_script_sha256": hashlib.sha256(embedder.read_bytes()).hexdigest(),
            "daemon_script": str(daemon),
            "daemon_script_sha256": hashlib.sha256(daemon.read_bytes()).hexdigest(),
            "model_revision": model_revision}


def verify_corpora(root: Path) -> dict[str, dict[str, str]]:
    info: dict[str, dict[str, str]] = {}
    for corpus, pin in CORPUS_PINS.items():
        repo = root / corpus
        head = require_command(["git", "rev-parse", "HEAD"], repo)
        dirty = require_command(["git", "status", "--porcelain"], repo)
        if head != pin:
            raise RuntimeError(f"{corpus} corpus pin mismatch: expected {pin}, got {head}")
        if dirty:
            raise RuntimeError(f"{corpus} corpus worktree is dirty")
        refs = require_command(["git", "for-each-ref", "--format=%(refname) %(objectname)"], repo).splitlines()
        expected_ref = f"refs/heads/benchmark-corpus {pin}"
        if refs != [expected_ref]:
            raise RuntimeError(f"{corpus} must have only the pinned benchmark-corpus ref; found {refs}")
        git_dir = Path(require_command(["git", "rev-parse", "--absolute-git-dir"], repo))
        for name in ("issue83-benchmark.sqlite", "repodb.sqlite"):
            if any((git_dir / suffix).exists() for suffix in (name, name + "-wal", name + "-shm")):
                raise RuntimeError(f"{corpus} already has an index at {git_dir / name}; use a fresh corpus clone")
        info[corpus] = {"commit": head, "worktree": "clean", "refs": refs, "git_dir": str(git_dir)}
    return info


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="clean checkout of the exact v0.3.0 tag")
    parser.add_argument("--binary", type=Path, required=True, help="release binary built from that tag")
    parser.add_argument("--corpus-root", type=Path, required=True, help="fresh root containing repodb/ and protonsync_ts/ pinned clones")
    parser.add_argument("--embedder", type=Path, required=True, help="real $HOME/bin/generate_embeddings executable")
    parser.add_argument("--semgrep-core", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="retain full raw JSON here; output contains retrieved source text")
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--timeout", type=int, default=3600)
    args = parser.parse_args()
    if args.repeat < 1 or args.limit < 1:
        parser.error("--repeat and --limit must be positive")

    source = args.source.resolve()
    binary = args.binary.resolve()
    corpus_root = args.corpus_root.resolve()
    embedder = args.embedder.resolve()
    semgrep_core = args.semgrep_core.resolve()
    output = args.output.resolve()
    home = Path(os.environ.get("HOME", "")).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError(f"release binary missing or not executable: {binary}")
    if not embedder.is_file() or not os.access(embedder, os.X_OK):
        raise RuntimeError(f"embedder missing or not executable: {embedder}")
    if embedder != (home / "bin/generate_embeddings").resolve():
        raise RuntimeError("the unmodified real-model path requires the actual HOME/bin/generate_embeddings executable")
    if not semgrep_core.is_file() or not os.access(semgrep_core, os.X_OK):
        raise RuntimeError(f"semgrep-core missing or not executable: {semgrep_core}")
    if output.exists():
        raise RuntimeError(f"refusing to overwrite raw result: {output}")
    source_info = verify_source(source)
    corpora_info = verify_corpora(corpus_root)
    benchmark_info = benchmark_assets()
    embedder_info = embedder_provenance(embedder, home)

    version = require_command([str(binary), "--version"])
    if version != "repodb 0.3.0":
        raise RuntimeError(f"binary is not RepoDB 0.3.0: {version}")
    output.parent.mkdir(parents=True, exist_ok=True)
    warmup = warm_embedder(embedder, corpus_root, output.parent / "candidate" / ".config")
    harness = load_harness()
    run_result = harness.run_variant(
        "candidate", binary, corpus_root, embedder, semgrep_core, output.parent,
        True, args.repeat, args.limit, args.timeout,
    )
    try:
        memory_bytes = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError, AttributeError):
        memory_bytes = None
    metadata = {
        "schema_version": 1,
        "ticket": 119,
        "run_kind": "released-v0.3.0-candidate-only-baseline; not an old/new comparison",
        "source": source_info,
        "binary": {"path": str(binary), "version": version,
                   "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                   "build_command": "CARGO_TARGET_DIR=/tmp/repodb-120-target cargo build --release --all-targets"},
        "corpora": corpora_info,
        "corpus_pins": CORPUS_PINS,
        "benchmark_assets": benchmark_info,
        "embedder": {"path": str(embedder), "version": require_command([str(embedder), "--version"]),
                     "sha256": hashlib.sha256(embedder.read_bytes()).hexdigest(),
                     "model": "sentence-transformers/all-MiniLM-L6-v2",
                     "provenance": embedder_info},
        "semgrep_core": {"path": str(semgrep_core),
                         "version": require_command([str(semgrep_core), "-version"]),
                         "sha256": hashlib.sha256(semgrep_core.read_bytes()).hexdigest()},

        "environment": {"platform": platform.platform(), "platform_release": platform.release(),
                        "machine": platform.machine(), "processor": platform.processor(),
                        "cpu_count_logical": os.cpu_count(), "memory_bytes": memory_bytes,
                        "python": sys.version,
                        "rustc": require_command(["rustc", "--version"]),
                        "cargo": require_command(["cargo", "--version"]),
                        "offline_environment": {"HF_HUB_OFFLINE": "1", "TRANSFORMERS_OFFLINE": "1",
                                                "XDG_CONFIG_HOME": str(output.parent / "candidate" / ".config"),
                                                "HOME": str(home),
                                                "HF_HOME": os.environ.get("HF_HOME"),
                                                "HUGGINGFACE_HUB_CACHE": os.environ.get("HUGGINGFACE_HUB_CACHE"),
                                                "TRANSFORMERS_CACHE": os.environ.get("TRANSFORMERS_CACHE"),
                                                "RUST_BACKTRACE": os.environ.get("RUST_BACKTRACE")}},
        "evaluation": {"tasks": 14, "top_k": args.limit,
                       "response_budget_bytes": harness.BUDGET_BYTES,
                       "quality_cutoffs": None,
                       "performance_cutoffs": None,
                       "cutoff_note": "No numeric quality or performance release cutoff was specified; the v0.3.0 baseline-derived gate was explicitly waived. Report measured indicators without pass/fail inference.",
                       "answer_correct": "all explicit task answer_terms occur in combined returned result text",
                       "source_reference_correct": "all expected path/line spans overlap at least one returned result",
                       "recall_at_k": "fraction of expected references with an overlapping returned path/line span"},
        "command_options": {"refresh": True, "repeat": args.repeat, "limit": args.limit,
                            "timeout_seconds": args.timeout},
        "warmup": warmup,
        "conditions": run_result["conditions"],
        "run": run_result,
        "limitations": [
            "The 14 source-derived tasks are not user-approved ground truth and are not representative enough to claim general retrieval quality.",
            "No stratified history-depth, dirty-worktree, exact-only, or semantic-only evaluation is performed.",
            "The model service is explicitly pre-warmed by one direct real-embedder batch; its startup and warmup time are excluded from refresh/query timings. OS caches are not dropped.",
            "Embedding subprocess calls and input/output bytes are unavailable without changing the real execution path; they remain null, not zero.",
            "One refresh per corpus is measured; refresh repeat variance is unavailable.",
            "No historical v0.2.0 comparison is included; this artifact does not retroactively pass the v0.3.0 release waiver.",
        ],
    }
    output.write_text(json.dumps(metadata, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote raw baseline results: {output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        raise SystemExit(f"issue-119 baseline failed: {error}")
