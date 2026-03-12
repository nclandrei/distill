#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_SCRIPT="$SCRIPT_DIR/build_codex_scan_fixture.py"

TEMP_HOME=""
SOURCE_CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
COPY_REAL_COUNT=8
NOISE_COUNT=18
DISTILL_BIN="$REPO_ROOT/target/debug/distill"
BUILD_BINARY=1

while [ $# -gt 0 ]; do
  case "$1" in
    --temp-home)
      TEMP_HOME="$2"
      shift 2
      ;;
    --source-codex-home)
      SOURCE_CODEX_HOME="$2"
      shift 2
      ;;
    --copy-real-count)
      COPY_REAL_COUNT="$2"
      shift 2
      ;;
    --noise-count)
      NOISE_COUNT="$2"
      shift 2
      ;;
    --distill-bin)
      DISTILL_BIN="$2"
      shift 2
      ;;
    --no-build)
      BUILD_BINARY=0
      shift
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "$TEMP_HOME" ]; then
  TEMP_HOME="$(mktemp -d "${TMPDIR%/}/distill-real-codex-XXXXXX")"
else
  mkdir -p "$TEMP_HOME"
fi

mkdir -p "$TEMP_HOME/.distill-runtime"

if [ "$BUILD_BINARY" -eq 1 ]; then
  (cd "$REPO_ROOT" && cargo build --quiet)
fi

python3 "$BUILD_SCRIPT" \
  --temp-home "$TEMP_HOME" \
  --source-codex-home "$SOURCE_CODEX_HOME" \
  --copy-real-count "$COPY_REAL_COUNT" \
  --noise-count "$NOISE_COUNT" \
  > "$TEMP_HOME/.distill-runtime/fixture-output.json"

RUN_DIR="$TEMP_HOME/.distill-runtime/real-codex-run"
SCAN_DEBUG_DIR="$RUN_DIR/scan-debug"
SCAN_LOG="$RUN_DIR/scan.log"
TIME_LOG="$RUN_DIR/time.log"
PS_LOG="$RUN_DIR/ps-samples.log"
TOP_LOG="$RUN_DIR/top-samples.log"
BEFORE_LOG="$RUN_DIR/source-codex-before.txt"
AFTER_LOG="$RUN_DIR/source-codex-after.txt"
DIFF_LOG="$RUN_DIR/source-codex-diff.txt"
SOURCE_ACCESS_LOG="$RUN_DIR/source-codex-access.log"
FIXTURE_CODEX_HOME="$TEMP_HOME/.codex"
mkdir -p "$RUN_DIR"

snapshot_tree() {
  local root="$1"
  local destination="$2"
  python3 - "$root" "$destination" <<'PY'
from pathlib import Path
import os
import sys

root = Path(sys.argv[1]).expanduser().resolve()
destination = Path(sys.argv[2]).expanduser().resolve()
lines = []
if root.exists():
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        stat = path.stat()
        try:
            relative = path.relative_to(root)
        except ValueError:
            relative = path
        lines.append(f"{int(stat.st_mtime)} {stat.st_size} {relative}")
destination.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")
PY
}

latest_status_file() {
  find "$SCAN_DEBUG_DIR" -name scan-status.json -type f -print 2>/dev/null | sort | tail -1
}

current_sample_pids() {
  local status_file
  status_file="$(latest_status_file)"
  if [ -n "$status_file" ]; then
    python3 - "$status_file" "$SCAN_PID" <<'PY'
import json
import sys

status_path = sys.argv[1]
fallback_pid = sys.argv[2]
with open(status_path, encoding="utf-8") as handle:
    status = json.load(handle)

pids = []
for key in ("scan_pid", "agent_pid"):
    value = status.get(key)
    if isinstance(value, int) and value > 0:
        pids.append(str(value))

if fallback_pid and fallback_pid not in pids:
    pids.insert(0, fallback_pid)

print(",".join(pids))
PY
  else
    echo "$SCAN_PID"
  fi
}

snapshot_tree "$SOURCE_CODEX_HOME" "$BEFORE_LOG"

(
  /usr/bin/time -l \
    env \
      HOME="$TEMP_HOME" \
      CODEX_HOME="$FIXTURE_CODEX_HOME" \
      DISTILL_SCAN_DEBUG_DIR="$SCAN_DEBUG_DIR" \
      "$DISTILL_BIN" scan --now
) >"$SCAN_LOG" 2>"$TIME_LOG" &
SCAN_PID=$!

(
  while kill -0 "$SCAN_PID" 2>/dev/null; do
    SAMPLE_PIDS="$(current_sample_pids)"
    {
      date -u +"%Y-%m-%dT%H:%M:%SZ"
      echo "pids=$SAMPLE_PIDS"
      ps -o pid,ppid,%cpu,%mem,rss,etime,command -p "$SAMPLE_PIDS" 2>/dev/null || true
      echo
    } >> "$PS_LOG"
    if command -v lsof >/dev/null 2>&1; then
      SOURCE_HITS="$(
        lsof -Fn -p "$SAMPLE_PIDS" 2>/dev/null | awk -v root="$SOURCE_CODEX_HOME" '
          /^n/ {
            path = substr($0, 2)
            if (path == root || index(path, root "/") == 1) {
              print path
            }
          }
        ' | sort -u
      )"
      if [ -n "$SOURCE_HITS" ]; then
        {
          date -u +"%Y-%m-%dT%H:%M:%SZ"
          printf '%s\n' "$SOURCE_HITS"
          echo
        } >> "$SOURCE_ACCESS_LOG"
      fi
    fi
    sleep 2
  done
) &
PS_SAMPLER_PID=$!

(
  while kill -0 "$SCAN_PID" 2>/dev/null; do
    {
      date -u +"%Y-%m-%dT%H:%M:%SZ"
      /usr/bin/top -l 1 -stats pid,cpu,mem,command -o cpu | head -n 25
      echo
    } >> "$TOP_LOG"
    sleep 5
  done
) &
TOP_SAMPLER_PID=$!

SCAN_EXIT=0
wait "$SCAN_PID" || SCAN_EXIT=$?
wait "$PS_SAMPLER_PID" || true
wait "$TOP_SAMPLER_PID" || true

snapshot_tree "$SOURCE_CODEX_HOME" "$AFTER_LOG"
diff -u "$BEFORE_LOG" "$AFTER_LOG" > "$DIFF_LOG" || true
SOURCE_CHANGED=0
if [ -s "$DIFF_LOG" ]; then
  SOURCE_CHANGED=1
fi
SOURCE_ACCESSED=0
if [ -s "$SOURCE_ACCESS_LOG" ]; then
  SOURCE_ACCESSED=1
fi

echo "temp_home=$TEMP_HOME"
echo "source_codex_home=$SOURCE_CODEX_HOME"
echo "fixture_codex_home=$FIXTURE_CODEX_HOME"
echo "scan_log=$SCAN_LOG"
echo "time_log=$TIME_LOG"
echo "ps_log=$PS_LOG"
echo "top_log=$TOP_LOG"
echo "scan_debug_dir=$SCAN_DEBUG_DIR"
echo "fixture_manifest=$TEMP_HOME/.distill-runtime/real-codex-fixture.json"
echo "source_snapshot_before=$BEFORE_LOG"
echo "source_snapshot_after=$AFTER_LOG"
echo "source_snapshot_diff=$DIFF_LOG"
echo "source_access_log=$SOURCE_ACCESS_LOG"
echo "source_changed=$SOURCE_CHANGED"
echo "source_accessed=$SOURCE_ACCESSED"
echo "scan_exit=$SCAN_EXIT"

if [ "$SOURCE_ACCESSED" -ne 0 ] && [ "$SCAN_EXIT" -eq 0 ]; then
  exit 1
fi

exit "$SCAN_EXIT"
