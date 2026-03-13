#!/usr/bin/env bash
set -euo pipefail

repo_root="$(
  if [ -n "${JJ_WORKSPACE_ROOT:-}" ]; then
    printf '%s\n' "$JJ_WORKSPACE_ROOT"
  elif jj root >/dev/null 2>&1; then
    jj root
  elif git rev-parse --show-toplevel >/dev/null 2>&1; then
    git rev-parse --show-toplevel
  else
    pwd
  fi
)"
cd "$repo_root"

echo "[local-checks] cargo fmt --all"
cargo fmt --all

echo "[local-checks] cargo clippy --fix --allow-dirty --allow-staged --allow-no-vcs -- -D warnings"
cargo clippy --fix --allow-dirty --allow-staged --allow-no-vcs -- -D warnings

echo "[local-checks] cargo fmt --all"
cargo fmt --all

echo "[local-checks] cargo clippy -- -D warnings"
cargo clippy -- -D warnings

echo "[local-checks] cargo test"
cargo test

echo "[local-checks] make perf-check"
make perf-check
