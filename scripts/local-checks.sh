#!/usr/bin/env bash
set -euo pipefail

repo_root="${JJ_WORKSPACE_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
cd "$repo_root"

echo "[local-checks] cargo fmt --all"
cargo fmt --all

echo "[local-checks] cargo clippy --fix --allow-dirty --allow-staged -- -D warnings"
cargo clippy --fix --allow-dirty --allow-staged -- -D warnings

echo "[local-checks] cargo fmt --all"
cargo fmt --all

echo "[local-checks] cargo clippy -- -D warnings"
cargo clippy -- -D warnings

echo "[local-checks] cargo test"
cargo test
