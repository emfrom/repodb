#!/usr/bin/env python3
"""Measure the pinned v0.3.1 binary on v0.3.0 RepoDB and combined-26 histories."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

REPO_PIN = "f7cc6bc5451204c5696f1bbef7e5575772820f47"


def git(args: list[str], cwd: Path | None = None) -> str:
    return subprocess.run(["git", *args], cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.strip()


def descendants(root: int) -> set[int]:
    rows = subprocess.run(["ps", "-eo", "pid=,ppid=,rss="], check=True, text=True,
                          stdout=subprocess.PIPE).stdout.splitlines()
    parents: dict[int, int] = {}
    rss: dict[int, int] = {}
    for row in rows:
        try:
            pid, ppid, memory = map(int, row.split())
        except ValueError:
            continue
        parents[pid] = ppid
        rss[pid] = memory
    found = {root}
    changed = True
    while changed:
        changed = False
        for pid, ppid in parents.items():
            if ppid in found and pid not in found:
                found.add(pid)
                changed = True
    return found & rss.keys()


def active_semgrep(pids: set[int]) -> bool:
    if not pids:
        return False
    completed = subprocess.run(["ps", "-o", "comm=", "-p", ",".join(map(str, pids))],
                               text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    return any("semgrep-core" in name.lower() for name in completed.stdout.splitlines())


def run_measured(command: list[str], cwd: Path, env: dict[str, str], daemon_pid: int | None,
                 timeout: int, measure_parser_stage: bool = False) -> dict[str, object]:
    started = time.perf_counter()
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, start_new_session=True)
    peak_kb = 0
    semgrep_active_wall_s = 0.0
    last_sample_time: float | None = None
    last_semgrep_active = False
    try:
        while process.poll() is None:
            if time.perf_counter() - started > timeout:
                process.kill()
                process.wait()
                raise TimeoutError(f"command exceeded {timeout}s: {' '.join(command)}")
            refresh_pids = descendants(process.pid)
            pids = set(refresh_pids)
            if daemon_pid:
                pids |= descendants(daemon_pid)
            if pids:
                ps = subprocess.run(["ps", "-o", "rss=", "-p", ",".join(map(str, pids))],
                                    text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                peak_kb = max(peak_kb, sum(int(value) for value in ps.stdout.split()))
            now = time.perf_counter()
            if last_sample_time is not None and last_semgrep_active:
                semgrep_active_wall_s += now - last_sample_time
            last_sample_time = now
            if measure_parser_stage:
                last_semgrep_active = active_semgrep(refresh_pids)
            time.sleep(0.05)
        stdout, stderr = process.communicate(timeout=timeout)
    except BaseException:
        process.kill()
        process.wait()
        raise
    elapsed = time.perf_counter() - started
    if process.returncode:
        raise RuntimeError(f"command failed ({process.returncode}): {' '.join(command)}\n{stderr[-4000:]}\n{stdout[-1000:]}")
    result: dict[str, object] = {"command": command, "elapsed_seconds": elapsed,
                                 "sampled_process_tree_peak_rss_kib": peak_kb,
                                 "returncode": process.returncode,
                                 "stdout_tail": stdout[-2000:], "stderr_tail": stderr[-2000:]}
    if measure_parser_stage:
        result["sampled_semgrep_process_active_union_wall_seconds"] = semgrep_active_wall_s
        result["sampler_interval_seconds"] = 0.05
    return result


def materialize(source: Path, commit: str, dest: Path) -> None:
    dest.mkdir(parents=True)
    git(["init", "-q"], dest)
    git(["fetch", "--no-tags", str(source), commit], dest)
    git(["checkout", "-q", "-b", "benchmark-corpus", "FETCH_HEAD"], dest)
    refs = git(["for-each-ref", "--format=%(refname) %(objectname)"], dest).splitlines()
    expected = f"refs/heads/benchmark-corpus {commit}"
    if refs != [expected]:
        raise RuntimeError(f"unexpected corpus refs in {dest}: {refs}")


def table_counts(database: Path) -> dict[str, int]:
    import sqlite3
    connection = sqlite3.connect(database)
    tables = [row[0] for row in connection.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")]
    counts: dict[str, int] = {}
    for table in tables:
        escaped = table.replace('"', '""')
        counts[table] = connection.execute(f'SELECT count(*) FROM "{escaped}"').fetchone()[0]
    connection.close()
    return counts


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--repo-source", type=Path, required=True)
    parser.add_argument("--scaled-source", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--embedder", type=Path, required=True)
    parser.add_argument("--semgrep-core", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--datasets", nargs="+", choices=("repodb", "combined_26"),
                        default=("repodb", "combined_26"))
    parser.add_argument("--timeout", type=int, default=3600, help="outer timeout for one refresh command")
    parser.add_argument("--process-timeout", type=int, default=300,
                        help="RepoDB subprocess timeout override; hold constant for candidate comparison")
    parser.add_argument("--measure-parser-stage", action="store_true",
                        help="sample semgrep-core active wall time as a diagnostic; adds sampler overhead")
    args = parser.parse_args()
    if args.repeat < 1:
        parser.error("repeat must be positive; final comparison should use at least 3")
    root = args.root.resolve()
    if root.exists() and any(root.iterdir()):
        parser.error(f"output root must be absent or empty: {root}")
    root.mkdir(parents=True)
    binary, embedder, semgrep = map(Path.resolve, (args.binary, args.embedder, args.semgrep_core))
    env = os.environ.copy()
    config_root = root / "config"
    config = config_root / "repodb"
    config.mkdir(parents=True)
    (config / "config.toml").write_text(
        f'[tools]\nembedding_command = "{embedder}"\nsemgrep_core = "{semgrep}"\n', encoding="utf-8")
    if not 1 <= args.process_timeout <= 3600:
        parser.error("--process-timeout must be from 1 through 3600")
    env.update({"XDG_CONFIG_HOME": str(config_root), "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1", "RUST_BACKTRACE": "1",
                "REPODB_REFRESH_TIMEOUT_SECS": str(args.process_timeout),
                "PATH": str(embedder.parent) + os.pathsep + env.get("PATH", "")})
    # Prewarm the same unmodified embedder/model outside measured refresh windows.
    warm_started = time.perf_counter()
    warm = subprocess.run([str(embedder), "--json", "-0", "-"], input="baseline warmup alpha\0baseline warmup beta",
                          cwd=args.repo_source, env=env, text=True, capture_output=True, timeout=300)
    warm_elapsed = time.perf_counter() - warm_started
    if warm.returncode:
        raise RuntimeError(f"embedder warmup failed: {warm.stderr}")
    vectors = [json.loads(line) for line in warm.stdout.splitlines()]
    if len(vectors) != 2 or any(len(vector) != 384 for vector in vectors):
        raise RuntimeError("embedder warmup did not yield two 384-dimensional vectors")
    pidfile = Path.home() / ".cache/generate_embeddings/daemon.pid"
    daemon_pid = int(pidfile.read_text().strip()) if pidfile.exists() else None
    out = root / "raw-baseline.json"
    results: dict[str, object] = {
        "schema_version": 1,
        "binary": {"path": str(binary), "version": subprocess.run([str(binary), "--version"], check=True, text=True, capture_output=True).stdout.strip(),
                   "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()},
        "repo_pin": REPO_PIN,
        "process_timeout_seconds": args.process_timeout,
        "outer_command_timeout_seconds": args.timeout,
        "scaled_head": git(["rev-parse", "HEAD"], args.scaled_source),
        "embedder": {"path": str(embedder), "version": subprocess.run([str(embedder), "--version"], check=True, text=True, capture_output=True).stdout.strip(),
                     "sha256": hashlib.sha256(embedder.read_bytes()).hexdigest(),
                     "daemon_sha256": hashlib.sha256(embedder.with_name("generate_embeddings.daemon").read_bytes()).hexdigest(),
                     "daemon_pid": daemon_pid, "model_revision": (Path.home() / ".cache/huggingface/hub/models--sentence-transformers--all-MiniLM-L6-v2/refs/main").read_text().strip(),
                     "warmup_seconds_excluded": warm_elapsed},
        "semgrep_core": {"path": str(semgrep), "version": subprocess.run([str(semgrep), "-version"], text=True, capture_output=True).stdout.strip(),
                         "sha256": hashlib.sha256(semgrep.read_bytes()).hexdigest()},
        "refresh": {},
    }
    dataset_sources = {"repodb": (args.repo_source, REPO_PIN),
                       "combined_26": (args.scaled_source, results["scaled_head"])}
    for dataset in args.datasets:
        source, pin = dataset_sources[dataset]
        measurements = []
        for repeat in range(1, args.repeat + 1):
            run_root = root / dataset / f"run-{repeat:02d}"
            repo = run_root / "repo"
            run_root.mkdir(parents=True)
            materialize(source.resolve(), str(pin), repo)
            database = run_root / "repodb.sqlite"
            print(f"starting {dataset} repeat {repeat}/{args.repeat} fresh refresh", flush=True)
            init = run_measured([str(binary), "--database", str(database), "init"], repo, env, daemon_pid,
                                args.timeout, args.measure_parser_stage)
            init["database_bytes"] = database.stat().st_size
            init["table_counts"] = table_counts(database)
            results["refresh"][dataset] = {"pin": pin, "runs": measurements + [
                {"repeat": repeat, "fresh_refresh": init, "unchanged_refresh": None,
                 "status": "fresh_only"}]}
            out.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
            unchanged = run_measured([str(binary), "--database", str(database), "refresh"], repo, env, daemon_pid,
                                     args.timeout, args.measure_parser_stage)
            unchanged["database_bytes"] = database.stat().st_size
            measurements.append({"repeat": repeat, "fresh_refresh": init, "unchanged_refresh": unchanged,
                                 "database_bytes_after_refresh": database.stat().st_size})
            results["refresh"][dataset] = {"pin": pin, "runs": measurements}
            out.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
            print(f"finished {dataset} repeat {repeat}: fresh={init['elapsed_seconds']:.2f}s unchanged={unchanged['elapsed_seconds']:.2f}s", flush=True)
        results["refresh"][dataset] = {"pin": pin, "runs": measurements}
        out.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(out)


if __name__ == "__main__":
    main()
