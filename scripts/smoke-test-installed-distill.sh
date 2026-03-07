#!/usr/bin/env bash
set -euo pipefail

input="${1:-}"
if [ -z "$input" ]; then
  echo "usage: $0 <distill-binary-or-tarball>" >&2
  exit 64
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

resolve_binary() {
  local source="$1"
  if [ -d "$source" ]; then
    find "$source" -type f -name distill -perm -u+x | head -n 1
    return
  fi

  case "$source" in
    *.tar.xz)
      local extracted="$workdir/extracted"
      mkdir -p "$extracted"
      tar -xJf "$source" -C "$extracted"
      find "$extracted" -type f -name distill -perm -u+x | head -n 1
      ;;
    *)
      printf '%s\n' "$source"
      ;;
  esac
}

bin_path="$(resolve_binary "$input")"
if [ -z "$bin_path" ] || [ ! -x "$bin_path" ]; then
  echo "could not find executable distill binary from: $input" >&2
  exit 1
fi

home_dir="$workdir/home"
mkdir -p "$home_dir"

export HOME="$home_dir"
export DISTILL_SYSTEMCTL_PATH=true
export DISTILL_LAUNCHCTL_PATH=true

"$bin_path" --help >"$workdir/help.out"

cat >"$workdir/onboarding.json" <<'JSON'
{
  "format_version": 1,
  "agents": [
    { "name": "claude", "enabled": true },
    { "name": "codex", "enabled": false }
  ],
  "scan_interval": "weekly",
  "proposal_agent": "claude",
  "shell": "zsh",
  "notifications": "none",
  "notification_icon": null,
  "install_shell_hook": false
}
JSON

"$bin_path" onboard --apply-json "$workdir/onboarding.json" >"$workdir/onboard.out"
grep -q "Onboarding applied from JSON." "$workdir/onboard.out"

"$bin_path" status >"$workdir/status.out"
grep -q "Scan interval:  weekly" "$workdir/status.out"
grep -q "Proposal agent: claude" "$workdir/status.out"

"$bin_path" scan --now >"$workdir/scan.out"
grep -q "No new sessions found since last scan." "$workdir/scan.out"

"$bin_path" review --write-json - >"$workdir/review.json"
grep -q '"format_version": 1' "$workdir/review.json"
grep -q '"proposals": \[\]' "$workdir/review.json"

echo "smoke test passed for $bin_path"
