#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <jj-command> [args...]" >&2
  echo "Supported commands: commit, describe" >&2
  exit 2
fi

subcommand="$1"
shift

case "$subcommand" in
  commit|describe)
    ;;
  *)
    echo "Unsupported command '$subcommand'. Use commit or describe." >&2
    exit 2
    ;;
esac

repo_root="${JJ_WORKSPACE_ROOT:-$(jj workspace root)}"
"$repo_root/scripts/local-checks.sh"

exec jj "$subcommand" "$@"
