#!/usr/bin/env python3

import argparse
import json
import sys
from pathlib import Path
from typing import List


def fail(errors: List[str]) -> int:
    for error in errors:
        print(f"perf-check: {error}", file=sys.stderr)
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate a Distill scan benchmark report against repository budgets."
    )
    parser.add_argument("report", help="Path to benchmark report JSON.")
    parser.add_argument(
        "--budget",
        default="perf/scan-budget.json",
        help="Path to performance budget JSON.",
    )
    args = parser.parse_args()

    report_path = Path(args.report).resolve()
    budget_path = Path(args.budget).resolve()

    report = json.loads(report_path.read_text(encoding="utf-8"))
    budget = json.loads(budget_path.read_text(encoding="utf-8"))
    thresholds = budget["thresholds"]
    status = report.get("scan_status", {})

    errors = []  # type: List[str]

    if report.get("scan_exit") != 0:
        errors.append(f"scan exited with status {report.get('scan_exit')}")
    if status.get("state") != "completed":
        errors.append(f"scan-status state was {status.get('state')!r}, expected 'completed'")

    wall_seconds = report.get("wall_seconds")
    if wall_seconds is None:
        errors.append("benchmark report did not capture wall_seconds")
    elif wall_seconds > thresholds["max_wall_seconds"]:
        errors.append(
            f"wall time {wall_seconds:.2f}s exceeded budget {thresholds['max_wall_seconds']:.2f}s"
        )

    max_rss_bytes = report.get("maximum_resident_set_size_bytes")
    if max_rss_bytes is None:
        errors.append("benchmark report did not capture maximum_resident_set_size_bytes")
    elif max_rss_bytes > thresholds["max_max_rss_bytes"]:
        errors.append(
            f"max RSS {max_rss_bytes} exceeded budget {thresholds['max_max_rss_bytes']}"
        )

    peak_memory_footprint_bytes = report.get("peak_memory_footprint_bytes")
    if peak_memory_footprint_bytes is not None and peak_memory_footprint_bytes > thresholds["max_peak_memory_footprint_bytes"]:
        errors.append(
            "peak memory footprint "
            f"{peak_memory_footprint_bytes} exceeded budget {thresholds['max_peak_memory_footprint_bytes']}"
        )

    batch_size = status.get("batch_size")
    if batch_size is None or batch_size > thresholds["max_batch_size"]:
        errors.append(
            f"batch size {batch_size} exceeded budget {thresholds['max_batch_size']}"
        )

    selected_raw_bytes = status.get("selected_raw_bytes")
    if selected_raw_bytes is None or selected_raw_bytes > thresholds["max_selected_raw_bytes"]:
        errors.append(
            "selected raw bytes "
            f"{selected_raw_bytes} exceeded budget {thresholds['max_selected_raw_bytes']}"
        )

    candidate_sessions = status.get("candidate_sessions", 0)
    if candidate_sessions < thresholds["min_candidate_sessions"]:
        errors.append(
            f"candidate session count {candidate_sessions} was below required {thresholds['min_candidate_sessions']}"
        )

    proposals_written = status.get("proposals_written", 0)
    if proposals_written < thresholds["min_proposals_written"]:
        errors.append(
            f"proposals written {proposals_written} was below required {thresholds['min_proposals_written']}"
        )

    backlog_sessions = status.get("backlog_sessions", 0)
    if thresholds.get("require_backlog_drain", False):
        if not (0 < backlog_sessions < candidate_sessions):
            errors.append(
                "backlog drain check failed: expected remaining backlog to be greater than 0 "
                f"and less than candidate_sessions, got backlog_sessions={backlog_sessions}, "
                f"candidate_sessions={candidate_sessions}"
            )

    workflow_keys = set(report.get("workflow_keys", []))
    required_workflow_keys = set(thresholds.get("required_workflow_keys", []))
    missing_workflows = sorted(required_workflow_keys - workflow_keys)
    if missing_workflows:
        errors.append(
            f"missing required workflow keys in scan-state.json: {', '.join(missing_workflows)}"
        )

    if errors:
        return fail(errors)

    print(
        "perf-check: passed "
        f"(wall={wall_seconds:.2f}s, max_rss={max_rss_bytes}, proposals={proposals_written}, backlog={backlog_sessions})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
