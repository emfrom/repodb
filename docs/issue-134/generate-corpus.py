#!/usr/bin/env python3
"""Generate a deterministic, scaled Git history from the 26 Semgrep fixtures."""
from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[2] / "tests/semgrep_corpus/sources"


def run(args: list[str], cwd: Path) -> str:
    result = subprocess.run(args, cwd=cwd, check=True, text=True, stdout=subprocess.PIPE)
    return result.stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--copies", type=int, default=10,
                        help="copies of all 26 language fixtures in each of two commits")
    args = parser.parse_args()
    if args.copies < 1:
        parser.error("--copies must be positive")
    repo = args.destination.resolve()
    if repo.exists() and any(repo.iterdir()):
        parser.error(f"destination must be absent or empty: {repo}")
    repo.mkdir(parents=True, exist_ok=True)
    inputs = sorted(FIXTURES.glob("*/*"))
    if len(inputs) != 26:
        raise SystemExit(f"expected 26 fixture inputs, found {len(inputs)}")
    input_hash = hashlib.sha256()
    for path in inputs:
        input_hash.update(path.relative_to(FIXTURES).as_posix().encode() + b"\0")
        input_hash.update(path.read_bytes() + b"\0")
    (repo / ".gitignore").write_text("", encoding="utf-8")
    for copy in range(args.copies):
        for source in inputs:
            relative = source.relative_to(FIXTURES)
            text = source.read_text(encoding="utf-8")
            text = re.sub(r"SampleV1", f"Sample{copy:04d}V1", text)
            target = repo / f"copy-{copy:04d}" / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
    run(["git", "init", "-q"], repo)
    run(["git", "config", "user.name", "RepoDB baseline corpus"], repo)
    run(["git", "config", "user.email", "benchmark@example.invalid"], repo)
    run(["git", "add", "."], repo)
    dates = {"GIT_AUTHOR_DATE": "2025-01-01T00:00:00+00:00",
             "GIT_COMMITTER_DATE": "2025-01-01T00:00:00+00:00"}
    subprocess.run(["git", "commit", "-q", "-m", "all-language fixture set V1"],
                   cwd=repo, env={**os.environ, **dates}, check=True)
    v1 = run(["git", "rev-parse", "HEAD"], repo)
    for copy_dir in sorted(repo.glob("copy-*")):
        for path in sorted(copy_dir.rglob("*")):
            if path.is_file():
                text = path.read_text(encoding="utf-8")
                path.write_text(re.sub(r"Sample(\d{4})V1", r"Sample\1V2", text), encoding="utf-8")
    run(["git", "add", "."], repo)
    dates["GIT_AUTHOR_DATE"] = dates["GIT_COMMITTER_DATE"] = "2025-01-02T00:00:00+00:00"
    subprocess.run(["git", "commit", "-q", "-m", "all-language fixture set V2"],
                   cwd=repo, env={**os.environ, **dates}, check=True)
    v2 = run(["git", "rev-parse", "HEAD"], repo)
    print(f"generator_sha256={hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}")
    print(f"fixture_input_sha256={input_hash.hexdigest()}")
    print(f"fixture_count={len(inputs)}")
    print(f"copies={args.copies}")
    print(f"files_per_commit={len(inputs) * args.copies}")
    print(f"v1_commit={v1}")
    print(f"v2_commit={v2}")
    print(f"repo_head={run(['git', 'rev-parse', 'HEAD'], repo)}")
    tree = run(["git", "ls-tree", "-r", "-l", "HEAD"], repo)
    print(f"tracked_source_bytes={sum(int(line.split()[3]) for line in tree.splitlines())}")


if __name__ == "__main__":
    main()
