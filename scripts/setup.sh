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

# ── Browser dependencies ────────────────────────────────────────────────────
t=$(date +%s)
echo "Installing browser dependencies..."
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  fonts-liberation libnss3 libatk-bridge2.0-0 libdrm2 libxcomposite1 \
  libxdamage1 libxrandr2 libgbm1 libasound2t64 libpango-1.0-0 libcairo2 \
  libcups2 libxss1 libgtk-3-0 libxshmfence1 xvfb \
  2>/dev/null || true
apt-get clean
_timer "Browser deps" "$t"

# ── Chromium via Playwright ──────────────────────────────────────────────────
t=$(date +%s)
PLAYWRIGHT_CHROMIUM=$(find /root/.cache/ms-playwright -name "chrome" -path "*/chrome-linux/chrome" 2>/dev/null | head -1)
if [ -z "$PLAYWRIGHT_CHROMIUM" ]; then
  echo "Installing Playwright Chromium..."
  npx playwright install --with-deps chromium 2>/dev/null || true
  PLAYWRIGHT_CHROMIUM=$(find /root/.cache/ms-playwright -name "chrome" -path "*/chrome-linux/chrome" 2>/dev/null | head -1)
else
  echo "Playwright Chromium already installed"
fi
if [ -z "$PLAYWRIGHT_CHROMIUM" ]; then
  PLAYWRIGHT_CHROMIUM=$(find /root/.cache/ms-playwright -name "headless_shell" -path "*/chrome-linux/headless_shell" 2>/dev/null | head -1)
fi

# Symlink to standard PATH locations so tools find Chromium without env vars
if [ -n "$PLAYWRIGHT_CHROMIUM" ]; then
  ln -sf "$PLAYWRIGHT_CHROMIUM" /usr/local/bin/chromium
  ln -sf "$PLAYWRIGHT_CHROMIUM" /usr/local/bin/google-chrome
  ln -sf "$PLAYWRIGHT_CHROMIUM" /usr/local/bin/chromium-browser
fi

# Move mismatched pre-installed chromedriver aside
for p in /opt/node22/bin/chromedriver /opt/node20/bin/chromedriver; do
  [ -f "$p" ] && [ ! -f "${p}.orig" ] && mv "$p" "${p}.orig"
done
_timer "Chromium" "$t"

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

# ── SQLite ───────────────────────────────────────────────────────────────────
if ! _installed sqlite3; then
  t=$(date +%s)
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends sqlite3 libsqlite3-dev 2>/dev/null || true
  _timer "SQLite" "$t"
fi

# ── PostgreSQL client ────────────────────────────────────────────────────────
if ! _installed psql; then
  t=$(date +%s)
  echo "Installing PostgreSQL client..."
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends postgresql-client 2>/dev/null || true
  _timer "PostgreSQL client" "$t"
fi

# ── Redis CLI ────────────────────────────────────────────────────────────────
if ! _installed redis-cli; then
  t=$(date +%s)
  echo "Installing Redis tools..."
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends redis-tools 2>/dev/null || true
  _timer "Redis CLI" "$t"
fi

# ── Docker CLI ───────────────────────────────────────────────────────────────
# NOTE: Docker CLI is often pre-installed but the daemon may not be running.
# This ensures the CLI is available for remote Docker or docker compose files.
if ! _installed docker; then
  t=$(date +%s)
  echo "Installing Docker CLI..."
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends docker.io 2>/dev/null || true
  _timer "Docker CLI" "$t"
fi

# ── Go ───────────────────────────────────────────────────────────────────────
if ! _installed go; then
  t=$(date +%s)
  GO_VERSION="1.24.7"
  echo "Installing Go ${GO_VERSION}..."
  curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-amd64.tar.gz" \
    | tar -C /usr/local -xzf -
  ln -sf /usr/local/go/bin/go   /usr/local/bin/go
  ln -sf /usr/local/go/bin/gofmt /usr/local/bin/gofmt
  _timer "Go ${GO_VERSION}" "$t"
fi

# ── Rust ─────────────────────────────────────────────────────────────────────
if ! _installed rustc; then
  t=$(date +%s)
  echo "Installing Rust..."
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal 2>/dev/null || true
  [ -f /root/.cargo/env ] && source /root/.cargo/env
  _timer "Rust" "$t"
fi

# ── Deno ─────────────────────────────────────────────────────────────────────
if ! _installed deno; then
  t=$(date +%s)
  echo "Installing Deno..."
  curl -fsSL https://deno.land/install.sh | sh 2>/dev/null || true
  [ -f /root/.deno/bin/deno ] && ln -sf /root/.deno/bin/deno /usr/local/bin/deno
  _timer "Deno" "$t"
fi

# ── Elixir + Erlang ─────────────────────────────────────────────────────────
if ! _installed elixir; then
  t=$(date +%s)
  echo "Installing Erlang + Elixir..."
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends erlang elixir 2>/dev/null || true
  _installed mix && mix local.hex --force 2>/dev/null || true
  _installed mix && mix local.rebar --force 2>/dev/null || true
  _timer "Elixir" "$t"
fi

# ── Zig ──────────────────────────────────────────────────────────────────────
if ! _installed zig; then
  t=$(date +%s)
  ZIG_VERSION="0.15.2"
  echo "Installing Zig ${ZIG_VERSION}..."
  curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" \
    | tar -C /usr/local -xJf -
  ln -sf /usr/local/zig-x86_64-linux-${ZIG_VERSION}/zig /usr/local/bin/zig
  _timer "Zig ${ZIG_VERSION}" "$t"
fi

# ── .NET ─────────────────────────────────────────────────────────────────────
if ! _installed dotnet; then
  t=$(date +%s)
  echo "Installing .NET SDK..."
  # Use the official install script — works on all Ubuntu versions reliably
  curl -fsSL https://dot.net/v1/dotnet-install.sh | bash -s -- --channel STS 2>/dev/null || true
  [ -f /root/.dotnet/dotnet ] && ln -sf /root/.dotnet/dotnet /usr/local/bin/dotnet
  _timer ".NET" "$t"
fi

# ── PHP ──────────────────────────────────────────────────────────────────────
if ! _installed php; then
  t=$(date +%s)
  echo "Installing PHP..."
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends \
    php-cli php-mbstring php-xml php-curl php-zip unzip \
    2>/dev/null || true
  # Composer
  if ! _installed composer; then
    curl -fsSL https://getcomposer.org/installer | php -- --install-dir=/usr/local/bin --filename=composer 2>/dev/null || true
  fi
  _timer "PHP" "$t"
fi

# ── uv (fast Python package manager) ────────────────────────────────────────
if ! _installed uv; then
  t=$(date +%s)
  echo "Installing uv..."
  curl -fsSL https://astral.sh/uv/install.sh | sh 2>/dev/null || true
  _timer "uv" "$t"
fi

# ── Node.js package managers ────────────────────────────────────────────────
t=$(date +%s)
# Ensure npm global bin is on PATH for this script
NPM_PREFIX="$(npm config get prefix 2>/dev/null)"
export PATH="${NPM_PREFIX}/bin:${PATH}"
_installed pnpm || npm install -g pnpm || true
_installed yarn || npm install -g yarn || true
for bin in pnpm pnpx yarn yarnpkg; do
  SRC="${NPM_PREFIX}/bin/${bin}"
  [ -f "$SRC" ] && [ ! -e "/usr/local/bin/${bin}" ] && ln -sf "$SRC" "/usr/local/bin/${bin}"
done
_timer "JS package managers" "$t"

# ── Persist environment variables ────────────────────────────────────────────
MARKER="# === claude-code-setup ==="
if ! grep -q "$MARKER" /etc/environment 2>/dev/null; then
  cat >> /etc/environment <<ENVEOF
${MARKER}
PUPPETEER_SKIP_DOWNLOAD=true
GOPATH=/root/go
DOTNET_ROOT=/root/.dotnet
ENVEOF
  if [ -n "${PLAYWRIGHT_CHROMIUM:-}" ]; then
    echo "PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=${PLAYWRIGHT_CHROMIUM}" >> /etc/environment
    echo "PUPPETEER_EXECUTABLE_PATH=${PLAYWRIGHT_CHROMIUM}" >> /etc/environment
    echo "CHROME_BIN=${PLAYWRIGHT_CHROMIUM}" >> /etc/environment
  fi
  echo 'PATH="/root/.cargo/bin:/root/.local/bin:/root/.deno/bin:/usr/local/go/bin:/root/go/bin:/root/.dotnet:${PATH}"' >> /etc/environment
fi

export GOPATH=/root/go
export DOTNET_ROOT=/root/.dotnet
export PATH="/root/.cargo/bin:/root/.local/bin:/root/.deno/bin:/usr/local/go/bin:/root/go/bin:/root/.dotnet:$PATH"


# ── Summary ──────────────────────────────────────────────────────────────────
ELAPSED=$(( $(date +%s) - SETUP_START ))
echo ""
echo "=== Setup complete (${ELAPSED}s) ==="
printf "%-10s %s\n" "Node:" "$(node --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "npm:" "$(npm --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "pnpm:" "$(pnpm --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "yarn:" "$(yarn --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "bun:" "$(bun --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Deno:" "$(deno --version | head -1 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Python:" "$(python3 --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "uv:" "$(uv --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Go:" "$(go version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Rust:" "$(rustc --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Ruby:" "$(ruby --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Java:" "$(java -version 2>&1 | head -1 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Elixir:" "$(elixir --version | tail -1 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Zig:" "$(zig version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "dotnet:" "$(dotnet --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "PHP:" "$(php --version | head -1 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "gh:" "$(gh --version | head -1 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "sqlite3:" "$(sqlite3 --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "psql:" "$(psql --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "redis-cli:" "$(redis-cli --version 2>/dev/null || echo 'not found')"
printf "%-10s %s\n" "Docker:" "$(docker --version 2>/dev/null || echo 'not found')"
echo "Chromium:  ${PLAYWRIGHT_CHROMIUM:-not found}"
