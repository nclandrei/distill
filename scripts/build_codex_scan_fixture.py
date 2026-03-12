#!/usr/bin/env python3

import argparse
import json
import os
import shutil
from datetime import datetime, timedelta, timezone
from pathlib import Path

RUNTIME_FILES = [
    ".codex-global-state.json",
    ".personality_migration",
    "AGENTS.md",
    "auth.json",
    "history.jsonl",
    "models_cache.json",
    "session_index.jsonl",
    "version.json",
]

RUNTIME_DIRS = [
    "rules",
]


def codex_message(role: str, text: str) -> str:
    return json.dumps(
        {
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": role,
                "content": [
                    {
                        "type": "input_text" if role == "user" else "output_text",
                        "text": text,
                    }
                ],
            },
        }
    )


def codex_command(command: str) -> str:
    return json.dumps(
        {
            "type": "item.completed",
            "item": {"type": "command_execution", "command": command},
        }
    )


def write_session(
    path: Path,
    cwd: str,
    when: datetime,
    filler_before: int,
    filler_after: int,
    filler_width: int,
    include_workflow: bool,
    workflow_project: str,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [json.dumps({"type": "session_meta", "payload": {"cwd": cwd}})]

    for index in range(filler_before):
        filler = f"{workflow_project} before {index} " + ("x" * max(filler_width, 1))
        lines.append(codex_message("user", filler))
        lines.append(codex_message("assistant", f"Investigating {workflow_project} before {index}"))

    if include_workflow:
        lines.append(
            codex_message(
                "user",
                f"Finish the {workflow_project} work, land it, and verify the result.",
            )
        )
        lines.append(codex_command("jj land"))
        lines.append(codex_command(f"cargo test -p {workflow_project}"))
        lines.append(
            codex_message(
                "assistant",
                f"Landed {workflow_project} changes and verified tests.",
            )
        )
    else:
        lines.append(
            codex_message(
                "user",
                f"Keep iterating on unrelated {workflow_project} notes and docs.",
            )
        )
        lines.append(codex_command("sed -n '1,40p' README.md"))
        lines.append(
            codex_message(
                "assistant",
                f"Reviewed unrelated {workflow_project} docs.",
            )
        )

    for index in range(filler_after):
        filler = f"{workflow_project} after {index} " + ("y" * max(filler_width, 1))
        lines.append(codex_message("user", filler))
        lines.append(codex_message("assistant", f"Following up on {workflow_project} after {index}"))

    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    ts = when.timestamp()
    os.utime(path, (ts, ts))


def write_config(temp_home: Path) -> Path:
    config_dir = temp_home / ".distill"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "config.yaml"
    config_path.write_text(
        "\n".join(
            [
                "agents:",
                "  - name: claude",
                "    enabled: false",
                "  - name: codex",
                "    enabled: true",
                "  - name: opencode",
                "    enabled: false",
                "scan_interval: weekly",
                "proposal_agent: codex",
                "shell: zsh",
                "notifications: both",
                "",
            ]
        ),
        encoding="utf-8",
    )
    return config_path


def newest_real_sessions(source_codex_home: Path, count: int) -> list[Path]:
    sessions_root = source_codex_home / "sessions"
    if count <= 0 or not sessions_root.exists():
        return []

    files = [path for path in sessions_root.rglob("*.jsonl") if path.is_file()]
    files.sort(key=lambda path: path.stat().st_mtime, reverse=True)
    return files[:count]


def copy_real_sessions(source_codex_home: Path, temp_home: Path, count: int) -> list[str]:
    copied = []
    sessions_root = source_codex_home / "sessions"
    destination_root = temp_home / ".codex" / "sessions"
    for source in newest_real_sessions(source_codex_home, count):
        relative = source.relative_to(sessions_root)
        destination = destination_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        copied.append(str(destination))
    return copied


def copy_runtime_support(source_codex_home: Path, temp_home: Path) -> list[str]:
    destination_root = temp_home / ".codex"
    destination_root.mkdir(parents=True, exist_ok=True)
    copied = []

    for relative_name in RUNTIME_FILES:
        source = source_codex_home / relative_name
        if not source.is_file():
            continue
        destination = destination_root / relative_name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        copied.append(str(destination))

    for relative_name in RUNTIME_DIRS:
        source = source_codex_home / relative_name
        if not source.exists():
            continue
        destination = destination_root / relative_name
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(source, destination)
        copied.append(str(destination))

    return copied


def build_fixture(args: argparse.Namespace) -> dict:
    temp_home = Path(args.temp_home).expanduser().resolve()
    temp_home.mkdir(parents=True, exist_ok=True)
    source_codex_home = Path(args.source_codex_home).expanduser().resolve()

    config_path = write_config(temp_home)
    now = datetime.now(timezone.utc) - timedelta(hours=2)
    synthetic_root = temp_home / ".codex" / "sessions" / "2026" / "03" / "12"
    synthetic_root.mkdir(parents=True, exist_ok=True)

    workflow_specs = [
        ("workflow-atlas-short", "atlas", 1, 1, 24),
        ("workflow-ios-long", "ios-app", 14, 10, 200),
        ("workflow-web-medium", "web-ui", 5, 4, 96),
    ]
    synthetic_sessions = []
    for index, (name, project, before, after, width) in enumerate(workflow_specs):
        when = now - timedelta(minutes=(index + 1) * 5)
        path = synthetic_root / f"{name}.jsonl"
        write_session(
            path=path,
            cwd=f"/Users/test/code/{project}",
            when=when,
            filler_before=before,
            filler_after=after,
            filler_width=width,
            include_workflow=True,
            workflow_project=project,
        )
        synthetic_sessions.append(str(path))

    for index in range(args.noise_count):
        when = now - timedelta(minutes=(index + len(workflow_specs) + 1) * 5)
        project = f"noise-{index:02}"
        path = synthetic_root / f"noise-{index:02}.jsonl"
        write_session(
            path=path,
            cwd=f"/Users/test/code/{project}",
            when=when,
            filler_before=4 + (index % 5),
            filler_after=3 + (index % 4),
            filler_width=120 + (index % 4) * 80,
            include_workflow=False,
            workflow_project=project,
        )
        synthetic_sessions.append(str(path))

    copied_runtime_support = copy_runtime_support(source_codex_home, temp_home)
    copied_real_sessions = copy_real_sessions(source_codex_home, temp_home, args.copy_real_count)
    manifest = {
        "temp_home": str(temp_home),
        "config_path": str(config_path),
        "source_codex_home": str(source_codex_home),
        "fixture_codex_home": str(temp_home / ".codex"),
        "copied_runtime_support": copied_runtime_support,
        "copied_real_session_count": len(copied_real_sessions),
        "copied_real_sessions": copied_real_sessions,
        "synthetic_session_count": len(synthetic_sessions),
        "synthetic_sessions": synthetic_sessions,
        "workflow_sessions": synthetic_sessions[: len(workflow_specs)],
        "noise_session_count": args.noise_count,
    }
    manifest_path = temp_home / ".distill-runtime" / "real-codex-fixture.json"
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    manifest["manifest_path"] = str(manifest_path)
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build a temporary HOME with synthetic and copied real Codex sessions for Distill scan verification."
    )
    parser.add_argument("--temp-home", required=True, help="Temporary HOME to populate.")
    parser.add_argument(
        "--source-codex-home",
        default=os.environ.get("CODEX_HOME") or str(Path.home() / ".codex"),
        help="Real Codex home to copy sessions from.",
    )
    parser.add_argument(
        "--copy-real-count",
        type=int,
        default=8,
        help="Number of newest real Codex sessions to copy into the fixture.",
    )
    parser.add_argument(
        "--noise-count",
        type=int,
        default=18,
        help="Number of synthetic noise sessions to generate.",
    )
    args = parser.parse_args()
    manifest = build_fixture(args)
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
