#!/usr/bin/env python3
"""Check exact content/source references for V1 and V2 in all 26 languages."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
GENERATOR = HERE / "generate-corpus.py"
FIXTURES = HERE.parents[1] / "tests/semgrep_corpus/sources"


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=root, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def validate_result(root: Path, result: dict[str, object], query: str) -> bool:
    kind = result.get("kind")
    if kind == "content":
        commit, path = result.get("commit_id"), result.get("path")
        start, end = result.get("line_start"), result.get("line_end")
    elif kind == "diff":
        if result.get("side") == "added":
            commit, path = result.get("child_commit_id"), result.get("new_path")
            start, end = result.get("new_line_start"), result.get("new_line_end")
        elif result.get("side") == "removed":
            commit, path = result.get("parent_commit_id"), result.get("old_path")
            start, end = result.get("old_line_start"), result.get("old_line_end")
        else:
            return False
    else:
        return False
    if not isinstance(commit, str) or not isinstance(path, str) or not isinstance(start, int) or not isinstance(end, int):
        return False
    try:
        lines = git(root, "show", f"{commit}:{path}").splitlines()
    except subprocess.CalledProcessError:
        return False
    return 1 <= start <= end <= len(lines) and query in "\\n".join(lines[start - 1:end])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, binary, db = map(Path.resolve, (args.repo, args.binary, args.database))
    commits = git(root, "rev-list", "--reverse", "HEAD").splitlines()
    if len(commits) != 2:
        raise SystemExit(f"expected exactly two corpus commits, found {len(commits)}")
    v1, v2 = commits
    rows = []
    fixtures = sorted(FIXTURES.glob("*/*"))
    if len(fixtures) != 26:
        raise SystemExit(f"expected 26 fixtures, found {len(fixtures)}")
    for fixture in fixtures:
        language = fixture.parent.name
        extension = fixture.suffix.lstrip(".")
        source = fixture.read_text(encoding="utf-8")
        match = re.search(r"Repodb[A-Za-z0-9]+SampleV1", source)
        if not match:
            raise SystemExit(f"cannot derive fixture marker: {language}")
        marker = match.group()
        if "SampleV1" not in marker:
            raise SystemExit(f"cannot derive fixture marker: {language} {marker}")
        relative = f"copy-0000/{language}/sample.{extension}"
        for version, commit in (("V1", v1), ("V2", v2)):
            query = marker.replace("SampleV1", f"Sample0000{version}")
            started = time.perf_counter()
            completed = subprocess.run(
                [str(binary), "--database", str(db), "search", query, "--exact",
                 "--history=all", "--json", "--limit", "5"], cwd=root, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
            elapsed = time.perf_counter() - started
            if completed.returncode:
                raise RuntimeError(f"{language}/{version} exact query failed: {completed.stderr}")
            results = json.loads(completed.stdout)
            content = [r for r in results if r.get("kind") == "content" and r.get("exact_match")]
            matching = [r for r in content if r.get("commit_id") == commit and r.get("path") == relative
                        and query in r.get("text", "")]
            if not matching:
                raise AssertionError(f"missing exact content result for {language}/{version}: {query}")
            if any(not validate_result(root, result, query) for result in results):
                raise AssertionError(f"a returned source reference is invalid: {language}/{version}")
            line_ok = False
            for hit in matching:
                lines = git(root, "show", f"{commit}:{relative}").splitlines()
                start, end = hit.get("line_start"), hit.get("line_end")
                if isinstance(start, int) and isinstance(end, int) and 1 <= start <= end <= len(lines):
                    if query in "\n".join(lines[start - 1:end]):
                        line_ok = True
            if not line_ok:
                raise AssertionError(f"line reference does not resolve to exact text: {language}/{version}")
            rows.append({"language": language, "version": version, "query": query,
                         "expected_commit": commit, "expected_path": relative,
                         "elapsed_seconds": elapsed, "returned_results": len(results),
                         "matching_content_results": len(matching), "returned_reference_count": len(results),
                         "all_returned_references_correct": line_ok, "source_reference_correct": line_ok,
                         "result_references": results})
    output = {"schema_version": 1, "generator_sha256": hashlib.sha256(GENERATOR.read_bytes()).hexdigest(),
              "repo_head": git(root, "rev-parse", "HEAD").strip(), "v1_commit": v1, "v2_commit": v2,
              "database_bytes": db.stat().st_size, "probes": rows,
              "summary": {"count": len(rows), "all_exact_hits": True, "all_source_references_correct": True}}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"passed {len(rows)} exact source-reference probes; raw={args.output}")


if __name__ == "__main__":
    main()
