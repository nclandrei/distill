#!/bin/bash
# Diagnostic script for Claude Code web environments.
# Usage: bash scripts/diagnose.sh
set -uo pipefail

G='\033[0;32m'; Y='\033[0;33m'; R='\033[0;31m'; N='\033[0m'
ok()   { echo -e "  ${G}ok${N}  $1"; }
warn() { echo -e "  ${Y}!!${N}  $1"; }
fail() { echo -e "  ${R}no${N}  $1"; }

_check() {
  local name="$1" cmd="${2:-$1}"
  if command -v "$cmd" &>/dev/null; then
    ok "$name: $($cmd --version 2>&1 | head -1)"
  else
    fail "$name: not installed"
  fi
}

echo "Claude Code Web Environment Diagnostics"
echo "========================================"
echo ""

echo "System"
ok "OS: $(grep PRETTY_NAME /etc/os-release 2>/dev/null | cut -d= -f2 | tr -d '\"' || echo unknown)"
ok "CPU: $(nproc 2>/dev/null || echo ?) cores | RAM: $(free -h 2>/dev/null | awk '/Mem:/{print $2}' || echo ?) | Disk: $(df -h / 2>/dev/null | awk 'NR==2{print $4}' || echo ?)"

echo ""
echo "Cloud Environment"
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] && ok "CLAUDE_CODE_REMOTE=true" || warn "CLAUDE_CODE_REMOTE not set"
[ -n "${CLAUDE_ENV_FILE:-}" ] && ok "CLAUDE_ENV_FILE is set" || warn "CLAUDE_ENV_FILE not set"

echo ""
echo "Toolchains"
_check "Rust" rustc
_check Cargo cargo

echo ""
echo "CLI Tools"
_check git
_check gh
_check jq
_check curl

echo ""
echo "Setup Status"
grep -q "claude-code-setup" /etc/environment 2>/dev/null && ok "setup.sh has run" || warn "setup.sh marker not found"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "${SCRIPT_DIR}/setup.sh" ] && ok "setup.sh exists" || fail "setup.sh missing"
[ -f "${SCRIPT_DIR}/session-start.sh" ] && ok "session-start.sh exists" || fail "session-start.sh missing"
SETTINGS="${SCRIPT_DIR}/../.claude/settings.json"
[ -f "$SETTINGS" ] && grep -q "SessionStart" "$SETTINGS" 2>/dev/null \
  && ok "SessionStart hook wired in settings.json" \
  || warn "SessionStart hook not configured"

echo ""
echo "Done."
