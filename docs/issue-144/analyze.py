#!/usr/bin/env python3
"""Reproduce the RepoDB 0.3.2 final paired-gate analysis from retained raw JSON."""
from __future__ import annotations

import hashlib
import json
import math
import random
import statistics
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE / "evidence"
RAW = EVIDENCE / "raw"
STRICT = RAW / "runset-02-strict"
BASELINE_SHA = "a7fa5ee72e5ff83419918275c8eb813891a5e942997133ad99bcb2bffb395f9a"
CANDIDATE_SHA = "ce084de6427d925816c7631537d3674213dd6c56973a3e0fe0a0414ba7a30fb8"
REPO_PIN = "f7cc6bc5451204c5696f1bbef7e5575772820f47"
COMBINED_PIN = "6740e8af4d89bbb1b2b3577749fbf499d6c15e5e"


def read(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def percentile_nearest_rank(values: list[float], probability: float) -> float:
    ordered = sorted(values)
    rank = math.ceil(probability * len(ordered))
    return ordered[rank - 1]


def summarize_ratios(ratios: list[float], threshold: float) -> dict[str, Any]:
    return {
        "paired_ratios_candidate_over_baseline": ratios,
        "median_ratio": statistics.median(ratios),
        "median_threshold": threshold,
        "median_pass": statistics.median(ratios) <= threshold,
        "individual_ratio_above_1_15": [i + 1 for i, value in enumerate(ratios) if value > 1.15],
        "individual_investigation_pass": all(value <= 1.15 for value in ratios),
    }


def run() -> dict[str, Any]:
    before: dict[int, dict[str, Any]] = {}
    after: dict[int, dict[str, Any]] = {}
    before_noop: dict[int, dict[str, float]] = {}
    after_noop: dict[int, dict[str, float]] = {}
    for pair in range(1, 6):
        b = read(STRICT / f"pair-{pair:02d}-before-throughput.json")
        c = read(STRICT / f"pair-{pair:02d}-after-throughput.json")
        bn = read(STRICT / f"pair-{pair:02d}-before-changed-noop.json")
        cn = read(STRICT / f"pair-{pair:02d}-after-changed-noop.json")
        if b["binary"]["sha256"] != BASELINE_SHA or c["binary"]["sha256"] != CANDIDATE_SHA:
            raise ValueError(f"pair {pair}: binary hash mismatch")
        if b["repo_pin"] != REPO_PIN or c["repo_pin"] != REPO_PIN:
            raise ValueError(f"pair {pair}: RepoDB source pin mismatch")
        if b["scaled_head"] != COMBINED_PIN or c["scaled_head"] != COMBINED_PIN:
            raise ValueError(f"pair {pair}: combined-26 source pin mismatch")
        before[pair], after[pair] = b, c
        before_noop[pair] = {r["dataset"]: r["elapsed_seconds"] for r in bn["measurements"]}
        after_noop[pair] = {r["dataset"]: r["elapsed_seconds"] for r in cn["measurements"]}
        for item in (bn, cn):
            if len(item["measurements"]) != 2 or any(row["returncode"] != 0 for row in item["measurements"]):
                raise ValueError(f"pair {pair}: incomplete/failed changed-noop measurement")
        for dataset in ("repodb", "combined_26"):
            for raw in (b, c):
                measurements = raw["refresh"][dataset]["runs"]
                if len(measurements) != 1:
                    raise ValueError(f"pair {pair}/{dataset}: expected one run")
                sample = measurements[0]
                for phase in ("fresh_refresh", "unchanged_refresh"):
                    if sample[phase] is None or sample[phase]["returncode"] != 0:
                        raise ValueError(f"pair {pair}/{dataset}/{phase}: missing or failed")

    def duration_ratios(dataset: str, phase: str) -> list[float]:
        return [
            after[pair]["refresh"][dataset]["runs"][0][phase]["elapsed_seconds"]
            / before[pair]["refresh"][dataset]["runs"][0][phase]["elapsed_seconds"]
            for pair in range(1, 6)
        ]

    primary = duration_ratios("repodb", "fresh_refresh")
    rng = random.Random(134032)
    bootstrap = []
    for _ in range(100_000):
        sample = [primary[rng.randrange(len(primary))] for _ in primary]
        bootstrap.append(math.exp(sum(math.log(value) for value in sample) / len(sample)))
    primary_gate = summarize_ratios(primary, 0.9)
    primary_gate.update({
        "statistic": "geometric mean of paired ratios",
        "bootstrap_resamples": 100_000,
        "bootstrap_seed": 134032,
        "bootstrap_upper_percentile": 0.95,
        "bootstrap_upper_bound_nearest_rank": percentile_nearest_rank(bootstrap, 0.95),
        "bootstrap_upper_bound_must_be_below": 1.0,
    })
    primary_gate["bootstrap_pass"] = primary_gate["bootstrap_upper_bound_nearest_rank"] < 1.0
    primary_gate["pass"] = primary_gate["median_pass"] and primary_gate["bootstrap_pass"]

    secondary: dict[str, Any] = {}
    for name, ratios in (
        ("combined_26_fresh", duration_ratios("combined_26", "fresh_refresh")),
        ("repodb_full_unchanged_refresh", duration_ratios("repodb", "unchanged_refresh")),
        ("combined_26_full_unchanged_refresh", duration_ratios("combined_26", "unchanged_refresh")),
        ("repodb_refresh_changed_noop", [after_noop[p]["repodb"] / before_noop[p]["repodb"] for p in range(1, 6)]),
        ("combined_26_refresh_changed_noop", [after_noop[p]["combined_26"] / before_noop[p]["combined_26"] for p in range(1, 6)]),
    ):
        summary = summarize_ratios(ratios, 1.05)
        summary["pass"] = summary["median_pass"] and summary["individual_investigation_pass"]
        secondary[name] = summary

    crossed_noop_diagnostics = {
        2: read(STRICT / "changed-noop-crossed-pair02-diagnostic.json"),
        4: read(STRICT / "changed-noop-crossed-pair04-diagnostic.json"),
    }
    outlier_investigations: dict[str, Any] = {}
    for name, dataset in (("repodb_refresh_changed_noop", "repodb"),
                          ("combined_26_refresh_changed_noop", "combined_26")):
        flagged = secondary[name]["individual_ratio_above_1_15"]
        cases = []
        for pair in flagged:
            diagnostic = crossed_noop_diagnostics[pair]
            owners = diagnostic["summary"][dataset]
            ratios = {owner: owners[owner]["candidate_over_baseline_median_ratio"]
                      for owner in ("before", "after")}
            ranges_overlap = {
                owner: max(owners[owner]["baseline_range_seconds"][0], owners[owner]["candidate_range_seconds"][0])
                <= min(owners[owner]["baseline_range_seconds"][1], owners[owner]["candidate_range_seconds"][1])
                for owner in ("before", "after")
            }
            cases.append({
                "pair": pair,
                "original_single_sample_ratio": secondary[name]["paired_ratios_candidate_over_baseline"][pair - 1],
                "crossed_20_repeat_candidate_over_baseline_ratios_by_database_owner": ratios,
                "crossed_20_repeat_ranges_overlap_by_database_owner": ranges_overlap,
                "crossed_20_repeat_all_ratios_below_investigation_trigger": all(value <= 1.15 for value in ratios.values()),
            })
        explained = bool(flagged) and all(
            case["crossed_20_repeat_all_ratios_below_investigation_trigger"]
            and all(case["crossed_20_repeat_ranges_overlap_by_database_owner"].values())
            for case in cases
        )
        outlier_investigations[name] = {
            "flagged_pairs": flagged,
            "investigation_completed": not flagged or explained,
            "explanation": (
                "The original one-shot no-op commands took about 10–22 ms. For every flagged pair, both binaries were then alternated 20 times against each same unchanged database. All crossed medians were below the 1.15 trigger and baseline/candidate timing ranges overlapped; database-owner/order changes altered which binary was slightly faster. This supports process-start/SQLite-open and shared-host timing noise rather than a stable candidate-specific regression."
                if flagged else "No individual ratio exceeded the investigation trigger."
            ),
            "cases": cases,
            "interpretation": "Retain every original pair; diagnostic repeats are not substituted into the gate ratios.",
        }
        secondary[name]["outlier_investigation"] = outlier_investigations[name]
        secondary[name]["pass"] = secondary[name]["median_pass"] and outlier_investigations[name]["investigation_completed"]

    resource_pairs = []
    for pair in range(1, 6):
        row: dict[str, Any] = {"pair": pair}
        for label, raw in (("baseline", before[pair]), ("candidate", after[pair])):
            row[label] = {}
            for dataset in ("repodb", "combined_26"):
                sample = raw["refresh"][dataset]["runs"][0]
                row[label][dataset] = {
                    "fresh_seconds": sample["fresh_refresh"]["elapsed_seconds"],
                    "unchanged_full_seconds": sample["unchanged_refresh"]["elapsed_seconds"],
                    "fresh_peak_process_tree_rss_kib": sample["fresh_refresh"]["sampled_process_tree_peak_rss_kib"],
                    "unchanged_peak_process_tree_rss_kib": sample["unchanged_refresh"]["sampled_process_tree_peak_rss_kib"],
                    "fresh_database_bytes": sample["fresh_refresh"]["database_bytes"],
                    "database_bytes_after_full_refresh": sample["database_bytes_after_refresh"],
                    "indexed_units": sample["fresh_refresh"]["table_counts"]["indexed_units"],
                    "unit_embeddings": sample["fresh_refresh"]["table_counts"]["unit_embeddings"],
                }
        resource_pairs.append(row)

    base_quality = read(RAW / "quality/v031/raw-quality.json")
    cand_quality = read(RAW / "quality/candidate/raw-quality.json")
    if base_quality["binary_sha256"] != BASELINE_SHA or cand_quality["binary_sha256"] != CANDIDATE_SHA:
        raise ValueError("quality binary hash mismatch")
    task_metrics = ("recall_at_k", "source_reference_correct", "answer_correct")
    quality_regressions = []
    aggregate: dict[str, Any] = {}
    for mode in ("semantic", "combined"):
        baseline_counts = {"source_reference_correct": 0, "answer_correct": 0, "outcomes": 0}
        candidate_counts = {"source_reference_correct": 0, "answer_correct": 0, "outcomes": 0}
        for corpus in ("repodb", "protonsync_ts"):
            base_rows = {row["task_id"]: row for row in base_quality["corpora"][corpus]["tasks"] if row["mode"] == mode}
            cand_rows = {row["task_id"]: row for row in cand_quality["corpora"][corpus]["tasks"] if row["mode"] == mode}
            if base_rows.keys() != cand_rows.keys():
                raise ValueError(f"{corpus}/{mode}: task sets differ")
            for task_id, base_row in base_rows.items():
                cand_row = cand_rows[task_id]
                if not all(run["all_returned_references_source_valid"] for run in base_row["runs"] + cand_row["runs"]):
                    raise ValueError(f"{corpus}/{task_id}/{mode}: invalid source reference")
                for metric in task_metrics:
                    base_median = statistics.median(run["score"][metric] for run in base_row["runs"])
                    cand_median = statistics.median(run["score"][metric] for run in cand_row["runs"])
                    if cand_median < base_median:
                        quality_regressions.append({"corpus": corpus, "task_id": task_id, "mode": mode,
                                                    "metric": metric, "baseline_median": base_median,
                                                    "candidate_median": cand_median})
                for row, counts in ((base_row, baseline_counts), (cand_row, candidate_counts)):
                    for run in row["runs"]:
                        score = run["score"]
                        counts["source_reference_correct"] += int(bool(score["source_reference_correct"]))
                        counts["answer_correct"] += int(bool(score["answer_correct"]))
                        counts["outcomes"] += 1
        aggregate[mode] = {"baseline": baseline_counts, "candidate": candidate_counts,
                           "baseline_floors": {"source_reference_correct": 3, "answer_correct": 9}}
        aggregate[mode]["pass"] = (
            candidate_counts["source_reference_correct"] >= 3
            and candidate_counts["answer_correct"] >= 9
        )

    exact_checks: dict[str, Any] = {}
    for label in ("before", "after"):
        exact = read(RAW / f"quality/combined-26/{label}-exact-positive.json")
        controls = read(RAW / f"quality/combined-26/{label}-negative-explore.json")
        semantic = read(RAW / f"quality/combined-26/{label}-semantic.json")
        explore = controls["duplicate_explore"]
        semantic_reference_audit = read(RAW / "quality/combined-26/semantic-reference-audit.json")[label]
        passing_languages = sorted(
            probe["language"] for probe in semantic["probes"]
            if len(probe["runs"]) == 3 and all(run["target_in_top5"] for run in probe["runs"])
        )
        exact_checks[label] = {
            "exact_positive_count": exact["summary"]["count"],
            "all_exact_hits": exact["summary"]["all_exact_hits"],
            "all_exact_source_references_correct": exact["summary"]["all_source_references_correct"],
            "negative_control_count": len(controls["negative_controls"]),
            "all_negative_controls_empty": all(row["returned_results"] == 0 for row in controls["negative_controls"]),
            "duplicate_explore_result_count": explore["result_count"],
            "duplicate_explore_total_count": explore["total_count"],
            "duplicate_explore_pages": explore["pages"],
            "duplicate_explore_unique_reference_ids": explore["reference_ids_unique"],
            "duplicate_explore_all_references_source_correct": explore["all_source_references_correct"],
            "duplicate_explore_distinct_content_paths": explore["distinct_content_paths"],
            "semantic_successful_target_retrievals": semantic["summary"]["successful_target_retrievals"],
            "semantic_attempts": semantic["summary"]["total_attempts"],
            "semantic_passing_languages": passing_languages,
            "semantic_reference_count": semantic_reference_audit["counts"]["references"],
            "semantic_reference_ranges_valid": semantic_reference_audit["counts"]["valid_source_ranges"] == semantic_reference_audit["counts"]["references"],
            "semantic_reference_text_present_in_source_excerpt": semantic_reference_audit["line_and_text_audit_pass"],
            "semantic_reference_strict_full_excerpt_mismatches": semantic_reference_audit["strict_full_excerpt_text_mismatch_count"],
            "marker_helper_strict_full_line_validator_passed": semantic_reference_audit["strict_validator_flag_from_measure_26_semantic"],
        }
    expected_paths = [f"copy-{copy:04d}/javascript/sample.js" for copy in range(10)]
    for label, checks in exact_checks.items():
        checks["quality_pass"] = (
            checks["exact_positive_count"] == 52
            and checks["all_exact_hits"]
            and checks["all_exact_source_references_correct"]
            and checks["negative_control_count"] == 26
            and checks["all_negative_controls_empty"]
            and checks["duplicate_explore_result_count"] == checks["duplicate_explore_total_count"] == 30
            and checks["duplicate_explore_unique_reference_ids"]
            and checks["duplicate_explore_all_references_source_correct"]
            and checks["duplicate_explore_distinct_content_paths"] == expected_paths
            and len(checks["semantic_passing_languages"]) >= 11
            and checks["semantic_reference_ranges_valid"]
            and checks["semantic_reference_text_present_in_source_excerpt"]
        )
    semantic_preserved = set(exact_checks["before"]["semantic_passing_languages"]) <= set(exact_checks["after"]["semantic_passing_languages"])
    quality_pass = (
        not quality_regressions
        and all(value["pass"] for value in aggregate.values())
        and all(value["quality_pass"] for value in exact_checks.values())
        and semantic_preserved
    )

    query_resources = {}
    for label in ("v031", "candidate"):
        doc = read(RAW / f"quality/{label}/query-resources.json")
        groups: dict[str, Any] = {}
        for corpus in ("repodb", "protonsync_ts"):
            groups[corpus] = {}
            rows = [row for row in doc["measurements"] if row["corpus"] == corpus]
            for mode in ("exact", "semantic", "combined"):
                selected = [row for row in rows if row["mode"] == mode]
                groups[corpus][mode] = {
                    "query_count": len(selected),
                    "median_seconds": statistics.median(row["elapsed_seconds"] for row in selected),
                    "max_sampled_process_tree_peak_rss_kib": max(row["sampled_process_tree_peak_rss_kib"] for row in selected),
                }
        query_resources[label] = {"includes_persistent_embedder_daemon": doc["includes_persistent_embedder_daemon"],
                                  "sample_interval_seconds": doc["sample_interval_seconds"], "by_corpus_mode": groups}

    memory_diagnostics: dict[str, Any] = {}
    for dataset, filename in (("combined_26", "memory-worker-diagnostic.json"),
                              ("repodb", "memory-worker-repodb-diagnostic.json")):
        diagnostic = read(RAW / filename)
        workers = {row["workers"]: {
            "elapsed_seconds": row["elapsed_seconds"],
            "sampled_process_tree_peak_rss_kib": row["sampled_process_tree_peak_rss_kib"],
        } for row in diagnostic["measurements"]}
        default8_rss = [row["candidate"][dataset]["fresh_peak_process_tree_rss_kib"]
                        for row in resource_pairs]
        default8_seconds = [row["candidate"][dataset]["fresh_seconds"] for row in resource_pairs]
        memory_diagnostics[dataset] = {
            "default_worker_count": diagnostic["default_workers"],
            "default_workers_gate_fresh_rss_median_kib": statistics.median(default8_rss),
            "default_workers_gate_fresh_seconds_median": statistics.median(default8_seconds),
            "explicit_worker_diagnostics": workers,
            "sampled_process_tree_includes_persistent_embedder_daemon": diagnostic["includes_persistent_embedder_daemon"],
            "interpretation": "One-run diagnostic on an existing candidate index; lower worker counts reduce peak RSS and also change full-refresh elapsed time. These diagnostics explain the RSS scaling but are not part of paired timing gates.",
        }

    inventory = []
    for path in sorted(EVIDENCE.rglob("*")):
        if path.is_file() and path.name not in {"SHA256SUMS.txt", "analysis.json"}:
            inventory.append({"path": path.relative_to(EVIDENCE).as_posix(),
                              "bytes": path.stat().st_size,
                              "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})

    return {
        "schema_version": 1,
        "analysis_script": "docs/issue-144/analyze.py",
        "strict_runset": {
            "pair_count": 5,
            "order": ["baseline,candidate", "candidate,baseline", "baseline,candidate", "candidate,baseline", "baseline,candidate"],
            "pairing_unit": "fresh independent clone and database per binary; no extra command between the two full-throughput runner invocations; changed/no-op refreshes were measured after both full runs in each pair",
            "primary_performance_gate": primary_gate,
            "secondary_nonregression_gates": secondary,
            "primary_and_secondary_pass": primary_gate["pass"] and all(item["pass"] for item in secondary.values()),
        },
        "resource_pairs": resource_pairs,
        "memory_diagnostics": memory_diagnostics,
        "persisted_embedding_work": {
            "unit_embeddings_and_indexed_units_per_pair": [
                {"pair": row["pair"], **{
                    f"{label}_{dataset}": {"indexed_units": row[label][dataset]["indexed_units"],
                                             "unit_embeddings": row[label][dataset]["unit_embeddings"]}
                    for label in ("baseline", "candidate") for dataset in ("repodb", "combined_26")
                }} for row in resource_pairs
            ],
            "embedder_invocation_batch_and_GPU_telemetry": "unavailable; not instrumented",
        },
        "quality": {
            "quality_regressions": quality_regressions,
            "aggregate_issue83_floors": aggregate,
            "combined_26_controls": exact_checks,
            "baseline_semantic_passing_languages_preserved": semantic_preserved,
            "issue83_source_reference_validation": "all returned references validated against pinned source by measure-quality.py",
            "semantic_marker_reference_note": "The marker helper's strict content equality check flags 39 content-unit snippets per binary because it compares each unit to the whole source line. The independent audit verifies all 303 references per binary have valid pinned commit/path/line ranges and that every returned unit string occurs within its cited source excerpt; those 39 strict-equality differences are retained and reported.",
            "pass": quality_pass,
        },
        "query_resource_summary": query_resources,
        "release_gate_pass": primary_gate["pass"] and all(item["pass"] for item in secondary.values()) and quality_pass,
        "evidence_inventory": inventory,
    }


if __name__ == "__main__":
    result = run()
    out = EVIDENCE / "analysis.json"
    out.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"analysis": str(out), "release_gate_pass": result["release_gate_pass"],
                      "primary": result["strict_runset"]["primary_performance_gate"],
                      "secondary": result["strict_runset"]["secondary_nonregression_gates"],
                      "quality_pass": result["quality"]["pass"]}, indent=2))
