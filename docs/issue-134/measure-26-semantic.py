#!/usr/bin/env python3
"""Measure semantic-only retrieval of one reviewed marker per language fixture."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parents[1] / "tests/semgrep_corpus/sources"


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=root, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def valid_reference(root: Path, row: dict[str, object]) -> bool:
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
    if not 1 <= start <= end <= len(lines):
        return False
    excerpt = "\n".join(lines[start - 1:end]).strip()
    text = str(row.get("text", "")).strip()
    if not text and any(row.get(flag) is True for flag in (
        "text_omitted_for_size", "head_text_omitted_for_size", "live_text_omitted_for_size"
    )):
        return True
    return bool(text) and (text == excerpt if row.get("kind") == "content" else text in excerpt)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, binary, db = map(Path.resolve, (args.repo, args.binary, args.database))
    head = git(root, "rev-parse", "HEAD").strip()
    fixtures = sorted(FIXTURES.glob("*/*"))
    if len(fixtures) != 26:
        raise SystemExit(f"expected 26 fixtures, found {len(fixtures)}")
    probes = []
    for fixture in fixtures:
        language = fixture.parent.name
        source = fixture.read_text(encoding="utf-8")
        marker_match = re.search(r"Repodb[A-Za-z0-9]+SampleV1", source)
        if not marker_match:
            raise SystemExit(f"missing marker in {fixture}")
        marker = marker_match.group().replace("SampleV1", "Sample0000V2")
        path = f"copy-0000/{language}/sample.{fixture.suffix.lstrip('.')}"
        query = f"What does the {language} source define or identify using {marker}?"
        runs = []
        for repeat in range(args.repeat):
            started = time.perf_counter()
            completed = subprocess.run(
                [str(binary), "--database", str(db), "search", query, "--semantic",
                 "--history=all", "--json", "--limit", "5"], cwd=root, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
            elapsed = time.perf_counter() - started
            if completed.returncode:
                raise RuntimeError(f"{language} semantic query failed: {completed.stderr}")
            response = json.loads(completed.stdout)
            results = response if isinstance(response, list) else response.get("results", [])
            relevant = [row for row in results if row.get("path") == path and marker in row.get("text", "")]
            refs_valid = all(valid_reference(root, row) for row in results)
            runs.append({"elapsed_seconds": elapsed, "returned_results": len(results),
                         "target_in_top5": bool(relevant), "target_rank": next(
                             (index + 1 for index, row in enumerate(results) if row in relevant), None),
                         "all_content_references_well_formed": refs_valid,
                         "results": results})
        probes.append({"language": language, "query": query, "expected_path": path,
                       "expected_marker": marker, "runs": runs})
        print(f"{language}: target@5={sum(run['target_in_top5'] for run in runs)}/{args.repeat}", flush=True)
    summary = {"probe_count": len(probes), "repeat_count": args.repeat,
               "successful_target_retrievals": sum(run["target_in_top5"] for probe in probes for run in probe["runs"]),
               "total_attempts": len(probes) * args.repeat}
    output = {"schema_version": 1, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "fixture_input_sha256": hashlib.sha256(b"".join(path.read_bytes() for path in fixtures)).hexdigest(),
              "repository_head": head, "database_bytes": db.stat().st_size,
              "result_limit": 5, "history": "all", "summary": summary, "probes": probes}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"summary={summary}; raw={args.output}")


if __name__ == "__main__":
    main()
