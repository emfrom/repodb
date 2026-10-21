#!/usr/bin/env python3
"""Reproduce pinned Rust operation metrics for Issue #121.

Requires rust-code-analysis-cli 0.0.25 and the repository's installed stable
Rust/Clippy. It measures a fixed representative operation set, not every source
file in the repository.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path
from typing import Any

OPERATIONS = [
    ("src/main.rs", "execute"),
    ("src/imports.rs", "refresh_with_size_limit"),
    ("src/history.rs", "refresh_scoped"),
    ("src/history.rs", "prepare_refresh_excluding"),
    ("src/history.rs", "stage_diff_pair"),
    ("src/history.rs", "search_impl"),
    ("src/worktree.rs", "search_exact"),
    ("src/worktree.rs", "map_semantic_ranges"),
    ("src/ranking.rs", "rank_evidence"),
]
SCOPE_NODES = {"function_item", "closure_expression"}
CONTROL_NODES = {"if_expression", "match_expression", "for_expression", "while_expression", "loop_expression"}
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
NODE = re.compile(
    r"\{([A-Za-z_][A-Za-z0-9_]*):\d+\} from \((\d+), (\d+)\) to \((\d+), (\d+)\)"
)
COGNITIVE_DIAGNOSTIC = re.compile(
    r"warning: the function has a cognitive complexity of \((\d+)/(\d+)\)\n"
    r"\s+--> ([^\n]+):(\d+):\d+"
)


def run(command: list[str], cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, check=False)


def metric_spaces(space: dict[str, Any]):
    yield space
    for child in space.get("spaces", []):
        yield from metric_spaces(child)


def own_metric(space: dict[str, Any], metric: str, field: str) -> float:
    """Subtract nested function/closure spaces from the tool's aggregate."""
    total = float(space["metrics"][metric][field])
    nested = sum(
        float(child["metrics"][metric][field])
        for child in space.get("spaces", [])
        if child.get("kind") == "function"
    )
    return total - nested


def position_offset(source_lines: list[bytes], row: int, column: int) -> int:
    if row == len(source_lines) + 1 and column == 1:
        return sum(map(len, source_lines))
    if row < 1 or row > len(source_lines):
        raise ValueError(f"AST line out of range: {row}")
    return sum(len(line) for line in source_lines[: row - 1]) + column - 1


def parse_ast(dump: str, source: bytes) -> list[dict[str, Any]]:
    lines = source.splitlines(keepends=True)
    nodes: list[dict[str, Any]] = []
    for raw in dump.splitlines():
        line = ANSI.sub("", raw)
        match = NODE.search(line)
        if not match:
            continue
        kind, sr, sc, er, ec = match.groups()
        marker = max(line.find("├─"), line.find("╰─"))
        depth = 0 if marker < 0 else (marker // 3) + 1
        start = position_offset(lines, int(sr), int(sc))
        end = position_offset(lines, int(er), int(ec))
        nodes.append({"kind": kind, "depth": depth, "start_line": int(sr), "start_column": int(sc),
                      "end_line": int(er), "end_column": int(ec), "start": start, "end": end})
    if not nodes or nodes[0]["kind"] != "source_file":
        raise RuntimeError("rust-code-analysis-cli AST dump was not recognized")
    if any(node["kind"] == "ERROR" for node in nodes):
        raise RuntimeError("AST dump contains parser ERROR nodes")
    return nodes


def scope_for(nodes: list[dict[str, Any]], start_line: int, end_line: int) -> dict[str, Any]:
    found = [node for node in nodes if node["kind"] == "function_item"
             and node["start_line"] == start_line and node["end_line"] == end_line]
    if len(found) != 1:
        raise RuntimeError(f"expected one AST function span at {start_line}-{end_line}, found {len(found)}")
    return found[0]


def belongs_to(node: dict[str, Any], scope: dict[str, Any], scopes: list[dict[str, Any]]) -> bool:
    containers = [candidate for candidate in scopes
                  if candidate["depth"] < node["depth"]
                  and candidate["start"] <= node["start"] and candidate["end"] >= node["end"]]
    if not containers:
        return False
    return max(containers, key=lambda candidate: candidate["depth"]) is scope


def direct_children(nodes: list[dict[str, Any]], parent: dict[str, Any]) -> list[dict[str, Any]]:
    return [node for node in nodes
            if node["depth"] == parent["depth"] + 1
            and parent["start"] <= node["start"] and node["end"] <= parent["end"]]


def callee_name(call: dict[str, Any], nodes: list[dict[str, Any]], source: bytes) -> str:
    children = direct_children(nodes, call)
    if not children:
        raise RuntimeError(f"call at {call['start_line']} has no AST callee child")
    node = children[0]
    while node["kind"] in {"generic_function", "reference_expression", "parenthesized_expression"}:
        children = direct_children(nodes, node)
        if not children:
            break
        node = children[0]
    if node["kind"] == "field_expression":
        fields = [candidate for candidate in nodes
                  if candidate["kind"] == "field_identifier"
                  and node["start"] <= candidate["start"] and candidate["end"] <= node["end"]]
        if fields:
            field = max(fields, key=lambda candidate: candidate["start"])
            return "method:" + source[field["start"]:field["end"]].decode("utf-8")
    if node["kind"] in {"identifier", "type_identifier", "scoped_identifier"}:
        target = source[node["start"]:node["end"]].decode("utf-8")
        return "path:" + re.sub(r"\s+", " ", target).strip()
    if node["kind"] == "closure_expression":
        return "<closure-call>"
    if node["kind"] == "call_expression":
        return "<call-result>"
    return "syntax:" + re.sub(r"\s+", " ", source[node["start"]:node["end"]].decode("utf-8")).strip()


def ast_measurements(nodes: list[dict[str, Any]], scope: dict[str, Any], source: bytes) -> dict[str, Any]:
    scopes = [node for node in nodes if node["kind"] in SCOPE_NODES]
    calls = [node for node in nodes if node["kind"] == "call_expression" and belongs_to(node, scope, scopes)]
    targets = {callee_name(call, nodes, source) for call in calls}

    controls = [node for node in nodes if node["kind"] in CONTROL_NODES and belongs_to(node, scope, scopes)]
    max_nesting = 0
    for node in controls:
        depth = 1 + sum(
            parent["depth"] < node["depth"]
            and parent["start"] <= node["start"] and parent["end"] >= node["end"]
            for parent in controls
        )
        max_nesting = max(max_nesting, depth)
    return {"call_expression_count": len(calls), "syntactic_call_target_fanout": len(targets),
            "call_targets": sorted(targets), "max_control_nesting": max_nesting}


def parse_clippy(log: str) -> dict[tuple[str, int], int]:
    scores: dict[tuple[str, int], int] = {}
    for match in COGNITIVE_DIAGNOSTIC.finditer(log):
        score, threshold, location, line = match.groups()
        if int(threshold) != 0:
            raise RuntimeError(f"unexpected Clippy threshold in diagnostic: {threshold}")
        path = location.removeprefix("./")
        key = (path, int(line))
        value = int(score)
        if key in scores and scores[key] != value:
            raise RuntimeError(f"conflicting Clippy scores for {key}")
        scores[key] = value
    return scores


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True, help="write the retained JSON measurement artifact")
    parser.add_argument("--clippy-log", type=Path, required=True, help="retain raw Clippy output here")
    parser.add_argument("--target-dir", type=Path, default=Path("/tmp/issue121-measure-target"))
    args = parser.parse_args()
    root = args.root.resolve()
    output = args.output.resolve()
    clippy_log = args.clippy_log.resolve()
    target_dir = args.target_dir.resolve()

    cli_version = run(["rust-code-analysis-cli", "--version"], root)
    if cli_version.returncode or cli_version.stdout.strip() != "rust-code-analysis-cli 0.0.25":
        raise RuntimeError(f"requires rust-code-analysis-cli 0.0.25, got {cli_version.stdout.strip()}")
    rustc_version = run(["rustc", "--version"], root)
    cargo_version = run(["cargo", "--version"], root)
    clippy_version = run(["cargo", "clippy", "--version"], root)
    if rustc_version.stdout.strip() != "rustc 1.99.0 (b940084d7 2026-09-28)":
        raise RuntimeError(f"requires rustc 1.99.0, got {rustc_version.stdout.strip()}")
    if cargo_version.stdout.strip() != "cargo 1.99.0 (5f94df478 2026-08-27)":
        raise RuntimeError(f"requires cargo 1.99.0, got {cargo_version.stdout.strip()}")
    if clippy_version.stdout.strip() != "clippy 0.1.99 (b940084d7e 2026-09-28)":
        raise RuntimeError(f"requires Clippy 0.1.99, got {clippy_version.stdout.strip()}")
    commit = run(["git", "rev-parse", "HEAD"], root)
    if commit.returncode:
        raise RuntimeError("root is not a Git checkout")
    source_paths = sorted({path for path, _ in OPERATIONS})
    source_check = run(["git", "diff", "--quiet", commit.stdout.strip(), "--", *source_paths], root)
    if source_check.returncode:
        raise RuntimeError("measured source files differ from the pinned HEAD; commit source changes first")

    output.parent.mkdir(parents=True, exist_ok=True)
    clippy_log.parent.mkdir(parents=True, exist_ok=True)
    target_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="issue121-metrics-") as tmp:
        tmp_path = Path(tmp)
        config_dir = tmp_path / "clippy-config"
        config_dir.mkdir()
        (config_dir / "clippy.toml").write_text("cognitive-complexity-threshold = 0\n", encoding="utf-8")
        env = os.environ.copy()
        env["CLIPPY_CONF_DIR"] = str(config_dir)
        env["CARGO_TARGET_DIR"] = str(target_dir)
        clippy_command = ["cargo", "clippy", "--bin", "repodb", "--all-features", "--", "-W", "clippy::cognitive_complexity"]
        clippy = run(clippy_command, root, env)
        clippy_log.write_text(clippy.stdout, encoding="utf-8")
        clippy_status = clippy_log.with_suffix(clippy_log.suffix + ".status")
        clippy_status.write_text(f"{clippy.returncode}\n", encoding="utf-8")
        if clippy.returncode:
            raise RuntimeError(f"Clippy failed ({clippy.returncode}); see {clippy_log}")
        cognitive_scores = parse_clippy(clippy.stdout)

        operations = []
        raw_metrics: dict[str, Any] = {}
        for index, (relative_path, function_name) in enumerate(OPERATIONS):
            file_path = root / relative_path
            if not file_path.is_file():
                raise RuntimeError(f"source file missing: {relative_path}")
            source = file_path.read_bytes()
            digest = hashlib.sha256(source).hexdigest()
            metrics_dir = tmp_path / f"metrics-{index}"
            metrics_dir.mkdir()
            metrics_command = ["rust-code-analysis-cli", "--paths", relative_path, "--metrics",
                               "--output-format", "json", "--pr", "--output", str(metrics_dir)]
            metrics_result = run(metrics_command, root)
            if metrics_result.returncode:
                raise RuntimeError(f"metrics failed for {relative_path}: {metrics_result.stdout[-2000:]}")
            metrics_file = metrics_dir / f"{relative_path}.json"
            metrics_data = json.loads(metrics_file.read_text(encoding="utf-8"))
            raw_metrics[relative_path] = metrics_data
            spaces = [space for space in metric_spaces(metrics_data)
                      if space.get("kind") == "function" and space.get("name") == function_name]
            if len(spaces) != 1:
                raise RuntimeError(f"expected one metrics scope {relative_path}::{function_name}, found {len(spaces)}")
            space = spaces[0]
            start_line, end_line = int(space["start_line"]), int(space["end_line"])
            ast_result = run(["rust-code-analysis-cli", "--paths", relative_path, "--dump"], root)
            if ast_result.returncode:
                raise RuntimeError(f"AST dump failed for {relative_path}: {ast_result.stdout[-2000:]}")
            nodes = parse_ast(ast_result.stdout, source)
            ast_scope = scope_for(nodes, start_line, end_line)
            ast_values = ast_measurements(nodes, ast_scope, source)
            cognitive = cognitive_scores.get((relative_path, start_line))
            if cognitive is None:
                raise RuntimeError(f"Clippy cognitive score missing for {relative_path}:{start_line}")
            lloc = own_metric(space, "loc", "lloc")
            cyclomatic = own_metric(space, "cyclomatic", "sum")
            if not lloc.is_integer() or not cyclomatic.is_integer():
                raise RuntimeError(f"non-integral Rust metrics for {relative_path}::{function_name}")
            operations.append({
                "file": relative_path,
                "function": function_name,
                "start_line": start_line,
                "end_line": end_line,
                "source_sha256": digest,
                "logical_lines_lloc": int(lloc),
                "cyclomatic_rust_code_analysis": int(cyclomatic),
                "cognitive_clippy": cognitive,
                **ast_values,
            })

    artifact = {
        "schema_version": 1,
        "source_commit": commit.stdout.strip(),
        "measurement_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_paths_sha256": {path: hashlib.sha256((root / path).read_bytes()).hexdigest()
                                 for path in sorted({path for path, _ in OPERATIONS})},
        "tools": {"rust_code_analysis_cli": cli_version.stdout.strip(),
                  "rustc": rustc_version.stdout.strip(), "cargo": cargo_version.stdout.strip(),
                  "cargo_clippy": clippy_version.stdout.strip()},
        "commands": {
            "metrics": "rust-code-analysis-cli --paths <file> --metrics --output-format json --pr --output <directory>",
            "ast": "rust-code-analysis-cli --paths <file> --dump",
            "cognitive": "CLIPPY_CONF_DIR=<temporary clippy.toml with cognitive-complexity-threshold = 0> cargo clippy --bin repodb --all-features -- -W clippy::cognitive_complexity",
        },
        "definitions": {
            "logical_lines_lloc": "rust-code-analysis-cli 0.0.25 Rust LLOC: AST Statement, EmptyStatement, ExpressionStatement, LetDeclaration, AssignmentExpression and CompoundAssignmentExpr nodes; nested function/closure metric aggregates are subtracted so each named operation is counted on its own.",
            "cyclomatic_rust_code_analysis": "rust-code-analysis-cli 0.0.25 Rust cyclomatic metric; nested function/closure aggregates are subtracted for the named operation.",
            "cognitive_clippy": "Clippy cognitive_complexity lint score from the installed stable toolchain, forced to report every positive score with configured threshold 0; closures are separate lint scopes.",
            "syntactic_call_target_fanout": "number of distinct AST-normalized callee names in call_expression nodes: field/method calls use the final field identifier, scoped identifiers keep their full path, plain identifiers keep their name, and closure/call-result callees use fixed buckets. Repeated names count once. No type/trait resolution; macro invocations are excluded. Nested functions/closures are assigned to their own scope.",
            "max_control_nesting": "maximum number of ancestor nodes among if_expression, match_expression, for_expression, while_expression and loop_expression in the named function AST; nested function/closure scopes are excluded.",
        },
        "clippy_raw_log": clippy_log.name,
        "clippy_raw_log_sha256": hashlib.sha256(clippy_log.read_bytes()).hexdigest(),
        "clippy_exit_status": clippy.returncode,
        "operations": operations,
        "limitations": [
            "Fan-out is syntax-level call-target fan-out, not a type-resolved whole-program call graph.",
            "Control nesting excludes boolean-expression complexity, macro expansions, async control flow, and external library internals.",
            "The evidence is a pinned static measurement and structural review, not a runtime defect or a mandate to refactor to meet soft thresholds.",
        ],
    }
    raw_output = output.with_name(output.stem + "-raw-metrics.json.gz")
    raw_bytes = json.dumps(raw_metrics, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")
    raw_output.write_bytes(gzip.compress(raw_bytes, compresslevel=9, mtime=0))
    artifact["raw_metrics_artifact"] = raw_output.name
    artifact["raw_metrics_artifact_sha256"] = hashlib.sha256(raw_output.read_bytes()).hexdigest()
    output.write_text(json.dumps(artifact, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    print(f"wrote {output}")
    print(f"wrote {raw_output}")
    print(f"Clippy log: {clippy_log} (exit {clippy.returncode})")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        raise SystemExit(f"issue-121 measurement failed: {error}")
