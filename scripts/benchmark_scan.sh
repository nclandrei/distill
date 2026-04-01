#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_SCRIPT="$SCRIPT_DIR/build_codex_scan_fixture.py"
MOCK_AGENT="$SCRIPT_DIR/mock_codex_scan_agent.py"

REPORT_PATH=""
TEMP_HOME=""
DISTILL_BIN="$REPO_ROOT/target/debug/distill"
NOISE_COUNT=80
COPY_REAL_COUNT=0
KEEP_TEMP_HOME=0

while [ $# -gt 0 ]; do
  case "$1" in
    --report)
      REPORT_PATH="$2"
      shift 2
      ;;
    --temp-home)
      TEMP_HOME="$2"
      shift 2
      ;;
    --distill-bin)
      DISTILL_BIN="$2"
      shift 2
      ;;
    --noise-count)
      NOISE_COUNT="$2"
      shift 2
      ;;
    --copy-real-count)
      COPY_REAL_COUNT="$2"
      shift 2
      ;;
    --keep-temp-home)
      KEEP_TEMP_HOME=1
      shift
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [ -z "$REPORT_PATH" ]; then
  echo "--report is required" >&2
  exit 2
fi

if [ -z "$TEMP_HOME" ]; then
  TEMP_HOME="$(mktemp -d "${TMPDIR%/}/distill-perf-XXXXXX")"
else
  mkdir -p "$TEMP_HOME"
fi

cleanup() {
  if [ "$KEEP_TEMP_HOME" -eq 0 ]; then
    rm -rf "$TEMP_HOME"
  fi
}
trap cleanup EXIT

mkdir -p "$TEMP_HOME/.distill-runtime"

python3 "$BUILD_SCRIPT" \
  --temp-home "$TEMP_HOME" \
  --source-codex-home "${CODEX_HOME:-$HOME/.codex}" \
  --copy-real-count "$COPY_REAL_COUNT" \
  --noise-count "$NOISE_COUNT" \
  > "$TEMP_HOME/.distill-runtime/perf-fixture-output.json"

RUN_DIR="$TEMP_HOME/.distill-runtime/perf-benchmark"
SCAN_DEBUG_DIR="$RUN_DIR/scan-debug"
SCAN_LOG="$RUN_DIR/scan.log"
TIME_LOG="$RUN_DIR/time.log"
MOCK_BIN_DIR="$RUN_DIR/bin"
mkdir -p "$RUN_DIR" "$SCAN_DEBUG_DIR" "$MOCK_BIN_DIR"

cat > "$MOCK_BIN_DIR/codex" <<EOF
#!/usr/bin/env bash
exec python3 "$MOCK_AGENT" "\$@"
EOF
chmod +x "$MOCK_BIN_DIR/codex"

SCAN_EXIT=0
TIME_ARGS=()
case "$(uname -s)" in
  Darwin)
    TIME_ARGS=(-l)
    ;;
  Linux)
    TIME_ARGS=(-v)
    ;;
esac

PATH="$MOCK_BIN_DIR:$PATH" \
HOME="$TEMP_HOME" \
CODEX_HOME="$TEMP_HOME/.codex" \
DISTILL_SCAN_DEBUG_DIR="$SCAN_DEBUG_DIR" \
/usr/bin/time "${TIME_ARGS[@]}" "$DISTILL_BIN" scan --now >"$SCAN_LOG" 2>"$TIME_LOG" || SCAN_EXIT=$?

SCAN_STATUS_FILES="$(find "$SCAN_DEBUG_DIR" -name scan-status.json -type f -print | sort)"

mkdir -p "$(dirname "$REPORT_PATH")"
python3 - "$TEMP_HOME" "$RUN_DIR" "$SCAN_LOG" "$TIME_LOG" "$REPORT_PATH" "$SCAN_EXIT" $SCAN_STATUS_FILES <<'PY'
import json
import re
import sys
from pathlib import Path

temp_home = Path(sys.argv[1])
run_dir = Path(sys.argv[2])
scan_log = Path(sys.argv[3])
time_log = Path(sys.argv[4])
report_path = Path(sys.argv[5])
scan_exit = int(sys.argv[6])
scan_status_files = [Path(p) for p in sys.argv[7:] if p]

fixture_manifest_path = temp_home / ".distill-runtime" / "real-codex-fixture.json"
scan_state_path = temp_home / ".distill" / "scan-state.json"
proposals_dir = temp_home / ".distill" / "proposals"

fixture_manifest = json.loads(fixture_manifest_path.read_text(encoding="utf-8"))

# Aggregate scan status across all batches (continuous scan produces multiple)
all_statuses = []
for sf in scan_status_files:
    if sf.is_file():
        all_statuses.append(json.loads(sf.read_text(encoding="utf-8")))

if all_statuses:
    scan_status = dict(all_statuses[-1])  # start from last batch
    # Sum cumulative fields across all batches
    for key in ("candidate_sessions", "proposals_written", "discovered_sessions",
                "skipped_sessions", "ready_workflows"):
        scan_status[key] = sum(s.get(key, 0) for s in all_statuses)
    # Take max for per-batch limits
    for key in ("batch_size", "selected_raw_bytes"):
        scan_status[key] = max(s.get(key, 0) for s in all_statuses)
    scan_status["_batch_count"] = len(all_statuses)
else:
    scan_status = {}
scan_status_path = str(scan_status_files[-1]) if scan_status_files else None
scan_state = {}
if scan_state_path.is_file():
    scan_state = json.loads(scan_state_path.read_text(encoding="utf-8"))

time_text = time_log.read_text(encoding="utf-8")
wall_seconds = None
max_rss_bytes = None
peak_memory_footprint_bytes = None

match = re.search(r"^\s*([0-9]+(?:\.[0-9]+)?)\s+real\b", time_text, re.MULTILINE)
if match:
    wall_seconds = float(match.group(1))

match = re.search(r"^\s*([0-9]+)\s+maximum resident set size\b", time_text, re.MULTILINE)
if match:
    max_rss_bytes = int(match.group(1))

match = re.search(r"^\s*([0-9]+)\s+peak memory footprint\b", time_text, re.MULTILINE)
if match:
    peak_memory_footprint_bytes = int(match.group(1))

linux_wall = re.search(
    r"Elapsed \(wall clock\) time .*?:\s*((?:(\d+):)?(\d+):(\d+(?:\.\d+)?))",
    time_text,
)
if linux_wall and wall_seconds is None:
    hours = int(linux_wall.group(2) or 0)
    minutes = int(linux_wall.group(3))
    seconds = float(linux_wall.group(4))
    wall_seconds = hours * 3600 + minutes * 60 + seconds

linux_rss = re.search(
    r"Maximum resident set size \(kbytes\):\s*([0-9]+)",
    time_text,
)
if linux_rss and max_rss_bytes is None:
    max_rss_bytes = int(linux_rss.group(1)) * 1024

proposal_files = sorted(
    str(path) for path in proposals_dir.glob("*.md") if path.is_file()
)
workflow_keys = sorted(scan_state.get("workflows", {}).keys())

report = {
    "temp_home": str(temp_home),
    "run_dir": str(run_dir),
    "fixture_manifest_path": str(fixture_manifest_path),
    "scan_status_path": str(scan_status_path) if scan_status_path else None,
    "scan_log": str(scan_log),
    "time_log": str(time_log),
    "scan_exit": scan_exit,
    "wall_seconds": wall_seconds,
    "maximum_resident_set_size_bytes": max_rss_bytes,
    "peak_memory_footprint_bytes": peak_memory_footprint_bytes,
    "proposal_files": proposal_files,
    "workflow_keys": workflow_keys,
    "fixture_manifest": fixture_manifest,
    "scan_status": scan_status,
}

report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

echo "$REPORT_PATH"
