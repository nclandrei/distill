#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/convert-live-mcp-smoke.sh [options]

Options:
  --servers "<name>=<npx-package>[,<name>=<npx-package>...]"
      Override default server matrix.
      Default: "memory=@modelcontextprotocol/server-memory,chrome-devtools=chrome-devtools-mcp@latest"

  --root <path>
      Explicit output root for temp HOME/config/log/artifacts.
      Default: $TMPDIR/distill-live-mcp-smoke-<timestamp>

  --keep
      Keep artifacts even when all checks pass.

  -h, --help
      Show this help.
EOF
}

repo_root="${JJ_WORKSPACE_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
cd "$repo_root"

servers_arg="memory=@modelcontextprotocol/server-memory,chrome-devtools=chrome-devtools-mcp@latest"
custom_root=""
keep=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --servers)
      [[ $# -ge 2 ]] || { echo "Missing value for --servers" >&2; exit 2; }
      servers_arg="$2"
      shift 2
      ;;
    --root)
      [[ $# -ge 2 ]] || { echo "Missing value for --root" >&2; exit 2; }
      custom_root="$2"
      shift 2
      ;;
    --keep)
      keep=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

timestamp="$(date +%Y%m%d-%H%M%S)"
test_root="${custom_root:-${TMPDIR:-/tmp}/distill-live-mcp-smoke-$timestamp}"
test_home="$test_root/home"
skills_dir="$test_home/.distill/skills"
config_path="$test_root/mcp.json"
log_path="$test_root/smoke.log"
summary_path="$test_root/summary.txt"

mkdir -p "$test_home" "$skills_dir"

echo "[smoke] repo_root=$repo_root"
echo "[smoke] test_root=$test_root"
echo "[smoke] log=$log_path"

echo "[smoke] building distill binary"
cargo build --quiet
distill_bin="${DISTILL_BIN:-$repo_root/target/debug/distill}"
if [[ ! -x "$distill_bin" ]]; then
  echo "distill binary not found at $distill_bin" >&2
  exit 1
fi

IFS=',' read -r -a server_defs <<< "$servers_arg"
if [[ "${#server_defs[@]}" -eq 0 ]]; then
  echo "No servers provided." >&2
  exit 2
fi

declare -a server_names=()
declare -a server_packages=()
for def in "${server_defs[@]}"; do
  name="${def%%=*}"
  package="${def#*=}"
  if [[ -z "$name" || -z "$package" || "$name" == "$package" ]]; then
    echo "Invalid server definition: '$def' (expected <name>=<npx-package>)" >&2
    exit 2
  fi
  server_names+=("$name")
  server_packages+=("$package")
done

{
  echo "{"
  echo "  \"mcpServers\": {"
  for idx in "${!server_names[@]}"; do
    name="${server_names[$idx]}"
    package="${server_packages[$idx]}"
    comma=","
    if [[ "$idx" -eq "$((${#server_names[@]} - 1))" ]]; then
      comma=""
    fi
    cat <<EOF
    "$name": {
      "command": "npx",
      "args": ["-y", "$package"]
    }$comma
EOF
  done
  echo "  }"
  echo "}"
} > "$config_path"

echo "[smoke] wrote config: $config_path"

fail=0
pass=0

run_step() {
  local label="$1"
  shift
  {
    echo
    echo "### $label"
    echo "+ $distill_bin $*"
  } | tee -a "$log_path"

  if HOME="$test_home" "$distill_bin" "$@" 2>&1 | tee -a "$log_path"; then
    echo "--- step=$label exit=0" | tee -a "$log_path"
    pass=$((pass + 1))
  else
    code=$?
    echo "--- step=$label exit=$code" | tee -a "$log_path"
    fail=$((fail + 1))
  fi
}

run_step "list" convert list --config "$config_path"

for name in "${server_names[@]}"; do
  run_step "$name inspect" convert inspect "$name" --config "$config_path"
  run_step "$name plan" convert plan "$name" --mode auto --config "$config_path"
  run_step "$name apply" convert apply "$name" --mode auto --config "$config_path" --skills-dir "$skills_dir"
  run_step "$name verify" convert verify "$name" --config "$config_path" --skills-dir "$skills_dir"
  run_step "$name one-shot" convert "$name" --config "$config_path" --skills-dir "$skills_dir"
done

manifest_count="$(find "$skills_dir/.distill-manifests" -maxdepth 1 -type f 2>/dev/null | wc -l | tr -d ' ')"
skill_count="$(find "$skills_dir" -maxdepth 1 -type f -name 'mcp-*.md' | wc -l | tr -d ' ')"

{
  echo "steps_passed=$pass"
  echo "steps_failed=$fail"
  echo "skills_dir=$skills_dir"
  echo "skills_count=$skill_count"
  echo "manifest_count=$manifest_count"
  echo "config_path=$config_path"
  echo "log_path=$log_path"
  echo "server_matrix=$servers_arg"
} | tee "$summary_path"

if [[ "$fail" -ne 0 ]]; then
  echo "[smoke] FAIL ($fail step(s) failed). Artifacts kept at: $test_root"
  exit 1
fi

echo "[smoke] PASS. Summary: $summary_path"

if [[ "$keep" -eq 1 ]]; then
  echo "[smoke] Keeping artifacts at: $test_root"
else
  echo "[smoke] Cleaning artifacts at: $test_root"
  rm -rf "$test_root"
fi

