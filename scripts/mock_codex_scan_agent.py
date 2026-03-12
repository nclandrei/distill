#!/usr/bin/env python3

import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple

EVENT_RE = re.compile(r"^- \[(\d+)\] (.*)$")
PACKAGE_RE = re.compile(r"cargo test -p ([^\s`]+)")


def parse_args(argv: List[str]) -> Tuple[Path, Optional[Path]]:
    workspace = Path.cwd()
    output_last_message = None
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg in {"-C", "--cd"} and index + 1 < len(argv):
            workspace = Path(argv[index + 1])
            index += 2
            continue
        if arg in {"--output-last-message", "-o"} and index + 1 < len(argv):
            output_last_message = Path(argv[index + 1])
            index += 2
            continue
        if arg == "--output-schema" and index + 1 < len(argv):
            index += 2
            continue
        index += 1
    return workspace, output_last_message


def load_manifest(workspace: Path) -> Dict[str, object]:
    manifest_path = workspace / "manifest.json"
    return json.loads(manifest_path.read_text(encoding="utf-8"))


def read_events(path: Path) -> List[Tuple[int, str]]:
    events = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = EVENT_RE.match(line)
        if match:
            events.append((int(match.group(1)), match.group(2)))
    return events


def locate_workflow(path: Path) -> Optional[Tuple[str, int, int]]:
    events = read_events(path)
    for index, (event_number, text) in enumerate(events):
        if "COMMAND: jj land" not in text:
            continue
        if index + 1 >= len(events):
            continue
        next_event_number, next_text = events[index + 1]
        package_match = PACKAGE_RE.search(next_text)
        if "COMMAND: cargo test -p " not in next_text or not package_match:
            continue
        start_event = events[max(index - 1, 0)][0]
        end_event = events[min(index + 2, len(events) - 1)][0]
        return package_match.group(1), start_event, max(end_event, next_event_number)
    return None


def build_detection_response(manifest: Dict[str, object]) -> Dict[str, object]:
    inspected_files = []
    session_findings = []
    for session in manifest["candidate_sessions"]:
        staged_path = session["staged_path"]
        inspected_files.append(staged_path)
        workflow = locate_workflow(Path(staged_path))
        candidates = []
        if workflow is not None:
            package, start_event, end_event = workflow
            candidates.append(
                {
                    "workflow_key": "land-and-test-changes",
                    "workflow_label": "Land And Test Changes",
                    "note": f"Land the prepared change with jj and immediately verify it with cargo test for {package}.",
                    "start_event": start_event,
                    "end_event": end_event,
                }
            )
            summary = f"Landed a prepared change and verified it with cargo tests for {package}."
        else:
            summary = "Reviewed a session without a stable reusable workflow."
        session_findings.append(
            {
                "session": staged_path,
                "summary": summary,
                "candidates": candidates,
            }
        )

    return {
        "inspected_files": inspected_files,
        "session_findings": session_findings,
    }


def build_proposal_response(manifest: Dict[str, object]) -> Dict[str, object]:
    inspected_files = []
    file_findings = []
    evidence = []
    packages = []

    for session in manifest["candidate_sessions"]:
        staged_path = session["staged_path"]
        inspected_files.append(staged_path)
        workflow = locate_workflow(Path(staged_path))
        package = "package"
        if workflow is not None:
            package = workflow[0]
        packages.append(package)
        file_findings.append(
            {
                "session": staged_path,
                "summary": f"Uses the same finish-and-verify loop for `{package}`: `jj land` followed by `cargo test -p {package}`.",
            }
        )
        evidence.append(
            {
                "session": staged_path,
                "pattern": f"`jj land` followed by `cargo test -p {package}` and a concise success report.",
            }
        )

    body = (
        "# Land And Test Changes\n"
        "## When to use\n"
        "Use this skill when the user asks to finish prepared Rust work in a Jujutsu repo by landing the current change and immediately verifying it with a package-scoped Cargo test command.\n\n"
        "## Steps\n"
        "1. Identify the narrowest relevant package-level Cargo test command for the changed component.\n"
        "2. If the correct package is unclear, ask before landing.\n"
        "3. Run `jj land`.\n"
        "4. Run the scoped Cargo verification command right after the land step.\n"
        "5. Report both outcomes together in one concise summary.\n\n"
        "## Verification\n"
        "- Confirm that `jj land` exits successfully.\n"
        "- Confirm that the scoped Cargo test command exits successfully.\n"
        "- Mention the verified package or target in the final response.\n\n"
        "## Pitfalls\n"
        "- Do not broaden the test scope when a package-level command is available.\n"
        "- Do not claim success if tests did not run.\n"
        "- Do not guess the verification target when the task context is ambiguous.\n"
    )

    return {
        "inspected_files": inspected_files,
        "file_findings": file_findings,
        "proposals": [
            {
                "type": "new",
                "confidence": "high",
                "target_skill": "land-and-test-changes",
                "evidence": evidence,
                "body": body,
            }
        ],
    }


def emit_audit_lines(inspected_files: List[str], response_text: str) -> None:
    print(json.dumps({"type": "thread.started", "thread_id": "perf-mock"}))
    print(json.dumps({"type": "turn.started"}))
    for index, path in enumerate(inspected_files):
        print(
            json.dumps(
                {
                    "type": "item.completed",
                    "item": {
                        "id": f"read_{index}",
                        "type": "command_execution",
                        "command": f"sed -n '1,220p' '{path}'",
                        "aggregated_output": "",
                        "exit_code": 0,
                        "status": "completed",
                    },
                }
            )
        )
    print(
        json.dumps(
            {
                "type": "item.completed",
                "item": {
                    "id": "agent_0",
                    "type": "agent_message",
                    "text": response_text,
                },
            }
        )
    )
    print(json.dumps({"type": "turn.completed"}))


def main(argv: List[str]) -> int:
    workspace, output_last_message = parse_args(argv)
    manifest = load_manifest(workspace)
    if workspace.name.startswith("workflow-"):
        response = build_proposal_response(manifest)
    else:
        response = build_detection_response(manifest)

    response_text = json.dumps(response)
    if output_last_message is not None:
        output_last_message.write_text(response_text, encoding="utf-8")

    emit_audit_lines(response["inspected_files"], response_text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
