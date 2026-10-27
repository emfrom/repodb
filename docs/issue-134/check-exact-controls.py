#!/usr/bin/env python3
"""Check exact-search negative controls and paginated duplicate occurrences."""
from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=root, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def valid_reference(root: Path, row: dict[str, object], query: str) -> bool:
    if row.get("kind") == "content":
        commit, path = row.get("commit_id"), row.get("path")
        start, end = row.get("line_start"), row.get("line_end")
    elif row.get("kind") == "diff":
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
    binary, root, database = map(Path.resolve, (args.binary, args.repo, args.database))
    languages = sorted(path.name for path in (Path(__file__).resolve().parents[2] / "tests/semgrep_corpus/sources").iterdir() if path.is_dir())
    negatives = []
    for language in languages:
        query = f"RepodbDefinitelyAbsent{language}BaselineNeedle0000"
        started = time.perf_counter()
        completed = subprocess.run(
            [str(binary), "--database", str(database), "search", query, "--exact",
             "--history=all", "--json", "--limit", "5"], cwd=root, text=True,
            capture_output=True, timeout=120)
        elapsed = time.perf_counter() - started
        if completed.returncode:
            raise RuntimeError(f"negative exact probe failed for {language}: {completed.stderr}")
        response = json.loads(completed.stdout)
        results = response if isinstance(response, list) else response.get("results", [])
        if results:
            raise AssertionError(f"negative probe unexpectedly matched: {language} {results}")
        negatives.append({"language": language, "query": query, "elapsed_seconds": elapsed,
                          "returned_results": 0})

    query = "function double(value)"
    started = time.perf_counter()
    completed = subprocess.run(
        [str(binary), "--database", str(database), "search", query, "--exact", "--history=all",
         "--explore", "--json", "--limit", "50"], cwd=root, text=True,
        capture_output=True, timeout=120)
    first_page_seconds = time.perf_counter() - started
    if completed.returncode:
        raise RuntimeError(f"duplicate explore failed: {completed.stderr}")
    page = json.loads(completed.stdout)
    pages = [page]
    snapshot = page.get("snapshot_id")
    cursor = page.get("cursor")
    while cursor:
        completed = subprocess.run(
            [str(binary), "--database", str(database), "search", query, "--exact", "--history=all",
             "--explore", "--json", "--limit", "50", "--cursor", cursor], cwd=root, text=True,
            capture_output=True, timeout=120)
        if completed.returncode:
            raise RuntimeError(f"duplicate explore continuation failed: {completed.stderr}")
        page = json.loads(completed.stdout)
        if page.get("snapshot_id") != snapshot:
            raise AssertionError("pagination changed snapshot identity")
        pages.append(page)
        cursor = page.get("cursor")
    refs = [row for page in pages for row in page.get("results", [])]
    expected_paths = {f"copy-{copy:04d}/javascript/sample.js" for copy in range(10)}
    content_refs = [r for r in refs if r.get("kind") == "content"]
    found_paths = {r.get("path") for r in content_refs}
    expected_total = pages[0].get("total_count")
    refs_valid = all(valid_reference(root, row, query) for row in refs)
    if (found_paths != expected_paths or len(refs) != expected_total
            or len(refs) != len({r.get("reference_id") for r in refs}) or not refs_valid):
        raise AssertionError(f"duplicate/explore mismatch: paths={found_paths}, refs={len(refs)}, total={expected_total}, refs_valid={refs_valid}")
    out = {"negative_controls": negatives,
           "duplicate_explore": {"query": query, "first_page_seconds": first_page_seconds,
                                 "pages": len(pages), "result_count": len(refs),
                                 "total_count": expected_total, "snapshot_id": snapshot,
                                 "distinct_content_paths": sorted(found_paths),
                                 "reference_ids_unique": True, "all_source_references_correct": refs_valid,
                                 "references": refs}}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"passed {len(negatives)} negative probes and duplicate explore ({len(refs)} references); raw={args.output}")


if __name__ == "__main__":
    main()
