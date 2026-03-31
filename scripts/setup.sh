#!/bin/bash
# Cloud environment setup script for Claude Code web environments.
# Automatically invoked by session-start.sh if the setup marker is missing.
# Can also be pasted into the "Setup script" field in Claude Code environment
# settings at claude.ai/code for faster cold starts (runs before session-start).
#
# Runs as root on Ubuntu 24.04. Idempotent — safe to run multiple times.
set -euo pipefail

SETUP_START=$(date +%s)
echo "=== Cloud environment setup ($(date -Iseconds)) ==="

_installed() { command -v "$1" &>/dev/null; }
_timer() {
  local label="$1" start="$2"
  echo "  done: ${label} ($(( $(date +%s) - start ))s)"
}

# ── System packages ──────────────────────────────────────────────────────────
t=$(date +%s)
echo "Installing system packages..."
apt-get update -qq

apt-get install -y -qq --no-install-recommends \
  jq curl wget httpie build-essential \
  tree htop ripgrep fd-find bat \
  2>/dev/null || true

apt-get clean
_timer "System packages" "$t"

# ── gh CLI ───────────────────────────────────────────────────────────────────
if ! _installed gh; then
  t=$(date +%s)
  echo "Installing gh CLI..."
  GH_VERSION="2.74.1"
  curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_amd64.deb" \
    -o /tmp/gh.deb && dpkg -i /tmp/gh.deb && rm -f /tmp/gh.deb \
    || apt-get install -y -qq gh 2>/dev/null \
    || echo "  Warning: gh CLI installation failed (non-fatal)"
  _timer "gh CLI" "$t"
fi

# ── Rust ─────────────────────────────────────────────────────────────────────
if ! _installed rustc; then
  t=$(date +%s)
  echo "Installing Rust..."
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal 2>/dev/null || true
  [ -f /root/.cargo/env ] && source /root/.cargo/env
  _timer "Rust" "$t"
fi

# ── Persist environment variables ────────────────────────────────────────────
MARKER="# === claude-code-setup ==="
if ! grep -q "$MARKER" /etc/environment 2>/dev/null; then
  cat >> /etc/environment <<ENVEOF
${MARKER}
ENVEOF
  echo 'PATH="/root/.cargo/bin:${PATH}"' >> /etc/environment
fi

export PATH="/root/.cargo/bin:$PATH"


# ── Summary ──────────────────────────────────────────────────────────────────
ELAPSED=$(( $(date +%s) - SETUP_START ))
echo ""
echo "=== Setup complete (${ELAPSED}s) ==="
printf "%-10s %s\n" "Rust:" "$(rustc --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "gh:" "$(gh --version | head -1 2>/dev/null || echo 'not found')"
