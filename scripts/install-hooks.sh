#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

git -C "$repo_root" config core.hooksPath "$repo_root/.githooks"

jj --repository "$repo_root" config set --repo aliases.safe-commit \
  '["util", "exec", "--", "bash", "-lc", "exec \"$JJ_WORKSPACE_ROOT/scripts/jj-with-checks.sh\" commit \"$@\"", ""]'

jj --repository "$repo_root" config set --repo aliases.safe-describe \
  '["util", "exec", "--", "bash", "-lc", "exec \"$JJ_WORKSPACE_ROOT/scripts/jj-with-checks.sh\" describe \"$@\"", ""]'

cat <<EOF
Installed local checks:
  Git pre-commit hook path: $repo_root/.githooks
  jj alias: jj safe-commit ...
  jj alias: jj safe-describe ...
EOF
