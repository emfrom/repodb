#!/usr/bin/env python3
"""Audit all returned marker-suite references against the two pinned corpus clones."""
from __future__ import annotations

import argparse
import json
import subprocess
from collections import Counter
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
RAW = HERE / "evidence" / "raw" / "quality" / "combined-26"


def source_lines(repo: Path, commit: str, path: str, cache: dict[tuple[str, str], list[str] | None]) -> list[str] | None:
    key = (commit, path)
    if key not in cache:
        result = subprocess.run(["git", "show", f"{commit}:{path}"], cwd=repo, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        cache[key] = result.stdout.splitlines() if result.returncode == 0 else None
    return cache[key]


def audit(label: str, repo: Path) -> dict[str, Any]:
    raw = json.loads((RAW / f"{label}-semantic.json").read_text(encoding="utf-8"))
    cache: dict[tuple[str, str], list[str] | None] = {}
    counts: Counter[str] = Counter()
    examples = []
    for probe in raw["probes"]:
        for repeat, run in enumerate(probe["runs"], start=1):
            for row in run["results"]:
                kind = row.get("kind")
                if kind == "content":
                    commit, path = row.get("commit_id"), row.get("path")
                    start, end = row.get("line_start"), row.get("line_end")
                elif kind == "diff" and row.get("side") == "added":
                    commit, path = row.get("child_commit_id"), row.get("new_path")
                    start, end = row.get("new_line_start"), row.get("new_line_end")
                elif kind == "diff" and row.get("side") == "removed":
                    commit, path = row.get("parent_commit_id"), row.get("old_path")
                    start, end = row.get("old_line_start"), row.get("old_line_end")
                else:
                    counts["invalid_reference_shape"] += 1
                    continue
                if not isinstance(commit, str) or not isinstance(path, str):
                    counts["invalid_reference_identity"] += 1
                    continue
                lines = source_lines(repo, commit, path, cache)
                if lines is None:
                    counts["missing_source"] += 1
                    continue
                if not isinstance(start, int) or not isinstance(end, int) or not 1 <= start <= end <= len(lines):
                    counts["invalid_line_range"] += 1
                    continue
                excerpt = "\n".join(lines[start - 1:end]).strip()
                text = str(row.get("text", "")).strip()
                omitted = not text and any(row.get(flag) is True for flag in (
                    "text_omitted_for_size", "head_text_omitted_for_size", "live_text_omitted_for_size"))
                contains = omitted or bool(text) and text in excerpt
                exact = omitted or bool(text) and (text == excerpt if kind == "content" else text in excerpt)
                counts["references"] += 1
                counts[f"kind_{kind}"] += 1
                counts["valid_source_ranges"] += 1
                counts["text_present_or_omitted_by_budget"] += int(contains)
                counts["strict_text_matches"] += int(exact)
                if not contains and len(examples) < 20:
                    examples.append({"language": probe["language"], "repeat": repeat,
                                     "kind": kind, "path": path, "line_start": start,
                                     "line_end": end, "text": text, "source_excerpt": excerpt})
    return {
        "binary_label": label,
        "binary_sha256": raw["binary_sha256"],
        "repository_head": raw["repository_head"],
        "strict_validator_flag_from_measure_26_semantic": all(
            run["all_content_references_well_formed"]
            for probe in raw["probes"] for run in probe["runs"]
        ),
        "counts": dict(counts),
        "line_and_text_audit_pass": (
            counts["references"] == counts["valid_source_ranges"]
            and counts["references"] == counts["text_present_or_omitted_by_budget"]
        ),
        "strict_full_excerpt_text_match_count": counts["strict_text_matches"],
        "strict_full_excerpt_text_mismatch_count": counts["references"] - counts["strict_text_matches"],
        "mismatch_examples": examples,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before-repo", type=Path, required=True)
    parser.add_argument("--after-repo", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = {
        "schema_version": 1,
        "validator_note": "The existing marker-suite helper requires a content unit to equal the entire cited line excerpt. This audit separately checks that every commit/path/line range resolves and that returned unit text occurs within the cited source excerpt; it reports strict full-line equality counts without hiding them.",
        "before": audit("before", args.before_repo.resolve()),
        "after": audit("after", args.after_repo.resolve()),
    }
    result["both_audits_pass"] = result["before"]["line_and_text_audit_pass"] and result["after"]["line_and_text_audit_pass"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({label: {"counts": result[label]["counts"],
                              "strict_full_excerpt_text_mismatch_count": result[label]["strict_full_excerpt_text_mismatch_count"],
                              "line_and_text_audit_pass": result[label]["line_and_text_audit_pass"]}
                      for label in ("before", "after")}, indent=2))


if __name__ == "__main__":
    main()
