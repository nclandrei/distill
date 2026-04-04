#!/bin/bash
# SessionStart hook — runs every time a session starts (new or resumed).
# Configured in .claude/settings.json under hooks.SessionStart.

# Only run in remote (cloud) environments
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  echo "Local environment detected — skipping remote setup."
  exit 0
fi

echo "=== Session start (remote) ==="

# ── Auto-run setup.sh if it hasn't run yet ───────────────────────────────────
SETUP_MARKER="# === claude-code-setup ==="
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if ! grep -q "$SETUP_MARKER" /etc/environment 2>/dev/null; then
  echo "Setup marker not found — running setup.sh automatically..."
  if [ -x "${SCRIPT_DIR}/setup.sh" ] || [ -f "${SCRIPT_DIR}/setup.sh" ]; then
    bash "${SCRIPT_DIR}/setup.sh" 2>&1 || echo "Warning: setup.sh exited with $? (non-fatal)"
  else
    echo "Warning: setup.sh not found at ${SCRIPT_DIR}/setup.sh"
  fi
fi

# Source persisted env vars from setup.sh
set -a; source /etc/environment 2>/dev/null || true; set +a


# ── Detect Chromium ──────────────────────────────────────────────────────────
PLAYWRIGHT_CHROMIUM=$(find /root/.cache/ms-playwright -name "chrome" -path "*/chrome-linux/chrome" 2>/dev/null | head -1)
[ -z "$PLAYWRIGHT_CHROMIUM" ] && \
  PLAYWRIGHT_CHROMIUM=$(find /root/.cache/ms-playwright -name "headless_shell" -path "*/chrome-linux/headless_shell" 2>/dev/null | head -1)

# ── Detect toolchain paths ──────────────────────────────────────────────────
CARGO_BIN=""; [ -d /root/.cargo/bin ] && CARGO_BIN="/root/.cargo/bin"
UV_BIN=""; [ -d /root/.local/bin ] && UV_BIN="/root/.local/bin"
DENO_BIN=""; [ -d /root/.deno/bin ] && DENO_BIN="/root/.deno/bin"


# ── Persist env vars for Claude's Bash tool ──────────────────────────────────
_persist() {
  local k="$1" v="$2"
  [ -n "${CLAUDE_ENV_FILE:-}" ] && echo "${k}=${v}" >> "$CLAUDE_ENV_FILE"
  export "${k}=${v}"
}

_persist CHROME_BIN                         "${PLAYWRIGHT_CHROMIUM:-}"
_persist PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH "${PLAYWRIGHT_CHROMIUM:-}"
_persist PUPPETEER_EXECUTABLE_PATH          "${PLAYWRIGHT_CHROMIUM:-}"
_persist PUPPETEER_SKIP_DOWNLOAD            "true"
_persist GOPATH                             "/root/go"
_persist DOTNET_ROOT                        "/root/.dotnet"

NEW_PATH="/usr/local/go/bin:/root/go/bin:/root/.dotnet"
[ -n "${CARGO_BIN:-}" ] && NEW_PATH="${CARGO_BIN}:${NEW_PATH}"
[ -n "${UV_BIN:-}" ] && NEW_PATH="${UV_BIN}:${NEW_PATH}"
[ -n "${DENO_BIN:-}" ] && NEW_PATH="${DENO_BIN}:${NEW_PATH}"
[ -n "$NEW_PATH" ] && _persist PATH "${NEW_PATH}:${PATH}"

# Fallback for when CLAUDE_ENV_FILE isn't available
if [ -z "${CLAUDE_ENV_FILE:-}" ]; then
  cat > /etc/profile.d/claude-code-env.sh <<'PROFILE'
export GOPATH=/root/go
export DOTNET_ROOT=/root/.dotnet
export PATH="/root/.cargo/bin:/root/.local/bin:/root/.deno/bin:/usr/local/go/bin:/root/go/bin:/root/.dotnet:$PATH"
PROFILE
  if [ -n "${PLAYWRIGHT_CHROMIUM:-}" ]; then
    cat >> /etc/profile.d/claude-code-env.sh <<CHROMIUM
export CHROME_BIN="${PLAYWRIGHT_CHROMIUM}"
export PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH="${PLAYWRIGHT_CHROMIUM}"
export PUPPETEER_EXECUTABLE_PATH="${PLAYWRIGHT_CHROMIUM}"
export PUPPETEER_SKIP_DOWNLOAD=true
CHROMIUM
  fi
fi


# ── Install project dependencies ────────────────────────────────────────────
cd "${CLAUDE_PROJECT_DIR:-$(pwd)}" 2>/dev/null || exit 0

# Node
if   [ -f package-lock.json ];  then npm install --prefer-offline 2>/dev/null || true
elif [ -f pnpm-lock.yaml ];     then pnpm install --frozen-lockfile 2>/dev/null || true
elif [ -f yarn.lock ];          then yarn install --frozen-lockfile 2>/dev/null || true
elif [ -f bun.lock ] || [ -f bun.lockb ]; then bun install --frozen-lockfile 2>/dev/null || true
fi

# Deno
[ -f deno.json ] || [ -f deno.jsonc ] && command -v deno &>/dev/null && deno install 2>/dev/null || true

# Python
if [ -f pyproject.toml ]; then
  if   command -v uv &>/dev/null;     then uv sync 2>/dev/null || uv pip install -e . 2>/dev/null || true
  elif command -v poetry &>/dev/null;  then poetry install 2>/dev/null || true
  else pip install -e . 2>/dev/null || true; fi
elif [ -f requirements.txt ]; then
  if command -v uv &>/dev/null; then uv pip install -q -r requirements.txt 2>/dev/null || true
  else pip install -q -r requirements.txt 2>/dev/null || true; fi
fi

[ -f go.mod ] && go mod download 2>/dev/null || true
[ -f Cargo.toml ] && command -v cargo &>/dev/null && cargo fetch 2>/dev/null || true
[ -f Gemfile ] && command -v bundle &>/dev/null && bundle install --quiet 2>/dev/null || true
[ -f mix.exs ] && command -v mix &>/dev/null && mix deps.get 2>/dev/null || true
[ -f "*.csproj" ] || [ -f "*.fsproj" ] && command -v dotnet &>/dev/null && dotnet restore 2>/dev/null || true
[ -f composer.json ] && command -v composer &>/dev/null && composer install --no-interaction --quiet 2>/dev/null || true

echo "=== Session ready ==="
exit 0
