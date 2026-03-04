#!/usr/bin/env bash
set -euo pipefail

die() {
  echo "Error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
distill_verify_session.sh

Usage:
  distill_verify_session.sh init [--repo PATH] [--state-file PATH] [--socket NAME] [--session NAME] [--target TARGET] [--command CMD] [--shell SHELL] [--restart]
  distill_verify_session.sh send [--state-file PATH] <tmux-key> [<tmux-key> ...]
  distill_verify_session.sh pane [--state-file PATH] [--lines N]
  distill_verify_session.sh screenshot [--state-file PATH] [--out ABS_PATH]
  distill_verify_session.sh status [--state-file PATH]
  distill_verify_session.sh cleanup [--state-file PATH] [--keep-state]

Notes:
  - Default state file: <repo>/.distill-runtime/verify-session.env
  - Uses isolated tmux server: -L <socket> -f /dev/null
  - Uses Ghostty window-id screenshot capture (-l) to avoid focus stealing.
EOF
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Required command not found: $1"
}

default_repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd
}

default_state_file_for_repo() {
  local repo_root="$1"
  echo "$repo_root/.distill-runtime/verify-session.env"
}

abs_path() {
  local p="$1"
  if [[ "$p" = /* ]]; then
    echo "$p"
  else
    echo "$(pwd)/$p"
  fi
}

ghostty_window_ids_all() {
  swift -e 'import CoreGraphics
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
if let info = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] {
  for w in info {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    let layer = w[kCGWindowLayer as String] as? Int ?? -1
    if owner == "Ghostty" && layer == 0, let id = w[kCGWindowNumber as String] as? Int {
      print(id)
    }
  }
}'
}

ghostty_frontmost_window_id() {
  ghostty_window_ids_all | head -n1
}

ghostty_window_exists_by_id() {
  local wid="$1"
  local exists
  exists="$(swift -e 'import CoreGraphics
let wid = Int(CommandLine.arguments[1]) ?? -1
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
if let info = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] {
  let found = info.contains { ($0[kCGWindowNumber as String] as? Int ?? -2) == wid }
  print(found ? "1" : "0")
} else {
  print("0")
}' "$wid")"
  [[ "$exists" == "1" ]]
}

window_id_in_list() {
  local wid="$1"
  local ids="$2"
  grep -qx "$wid" <<<"$ids"
}

wait_for_new_window_id() {
  local old_ids="$1"
  local attempts="${2:-50}"
  local sleep_s="${3:-0.1}"
  local i
  local current
  local wid
  for ((i = 1; i <= attempts; i++)); do
    current="$(ghostty_window_ids_all || true)"
    while read -r wid; do
      [[ -n "$wid" ]] || continue
      if ! window_id_in_list "$wid" "$old_ids"; then
        echo "$wid"
        return 0
      fi
    done <<<"$current"
    sleep "$sleep_s"
  done
  return 1
}

launch_ghostty_attach() {
  local socket="$1"
  local session="$2"
  open -na Ghostty --args -e tmux -L "$socket" attach -t "$session"
}

write_state_file() {
  local state_file="$1"
  mkdir -p "$(dirname "$state_file")"
  {
    printf 'VERIFY_REPO_ROOT=%q\n' "$VERIFY_REPO_ROOT"
    printf 'VERIFY_STATE_FILE=%q\n' "$state_file"
    printf 'VERIFY_TMUX_SOCKET=%q\n' "$VERIFY_TMUX_SOCKET"
    printf 'VERIFY_TMUX_SESSION=%q\n' "$VERIFY_TMUX_SESSION"
    printf 'VERIFY_TMUX_TARGET=%q\n' "$VERIFY_TMUX_TARGET"
    printf 'VERIFY_COMMAND=%q\n' "$VERIFY_COMMAND"
    printf 'VERIFY_SHELL=%q\n' "$VERIFY_SHELL"
    printf 'VERIFY_GHOSTTY_WINDOW_ID=%q\n' "${VERIFY_GHOSTTY_WINDOW_ID:-}"
    printf 'VERIFY_STARTED_AT=%q\n' "$VERIFY_STARTED_AT"
  } >"$state_file"
}

load_state_file() {
  local state_file="$1"
  [[ -f "$state_file" ]] || die "State file not found: $state_file"
  # shellcheck disable=SC1090
  source "$state_file"
  : "${VERIFY_REPO_ROOT:?Missing VERIFY_REPO_ROOT in state file}"
  : "${VERIFY_TMUX_SOCKET:?Missing VERIFY_TMUX_SOCKET in state file}"
  : "${VERIFY_TMUX_SESSION:?Missing VERIFY_TMUX_SESSION in state file}"
  : "${VERIFY_TMUX_TARGET:?Missing VERIFY_TMUX_TARGET in state file}"
  : "${VERIFY_COMMAND:?Missing VERIFY_COMMAND in state file}"
  : "${VERIFY_STARTED_AT:?Missing VERIFY_STARTED_AT in state file}"
  : "${VERIFY_SHELL:=${SHELL:-/bin/bash}}"
}

ensure_window_id() {
  local state_file="$1"
  local wid="${VERIFY_GHOSTTY_WINDOW_ID:-}"
  local old_ids

  if [[ -n "$wid" ]] && ghostty_window_exists_by_id "$wid"; then
    echo "$wid"
    return 0
  fi

  old_ids="$(ghostty_window_ids_all || true)"
  launch_ghostty_attach "$VERIFY_TMUX_SOCKET" "$VERIFY_TMUX_SESSION"

  wid="$(wait_for_new_window_id "$old_ids" || true)"
  if [[ -z "$wid" ]]; then
    wid="$(ghostty_frontmost_window_id || true)"
  fi

  [[ -n "$wid" ]] || die "Failed to resolve Ghostty window ID"
  ghostty_window_exists_by_id "$wid" || die "Resolved window ID is not valid: $wid"

  VERIFY_GHOSTTY_WINDOW_ID="$wid"
  write_state_file "$state_file"
  echo "$wid"
}

subcommand="${1:-}"
if [[ -z "$subcommand" ]]; then
  usage
  exit 1
fi
shift

case "$subcommand" in
  init)
    require_cmd tmux
    require_cmd swift
    require_cmd screencapture
    require_cmd open

    repo_root=""
    state_file=""
    socket="distill-ui"
    session="distill-ui"
    target=""
    command=""
    shell_bin=""
    restart=0

    while [[ $# -gt 0 ]]; do
      case "$1" in
        --repo)
          repo_root="$(abs_path "${2:-}")"
          shift 2
          ;;
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        --socket)
          socket="${2:-}"
          shift 2
          ;;
        --session)
          session="${2:-}"
          shift 2
          ;;
        --target)
          target="${2:-}"
          shift 2
          ;;
        --command)
          command="${2:-}"
          shift 2
          ;;
        --shell)
          shell_bin="${2:-}"
          shift 2
          ;;
        --restart)
          restart=1
          shift
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        *)
          die "Unknown init option: $1"
          ;;
      esac
    done

    [[ -n "$repo_root" ]] || repo_root="$(default_repo_root)"
    [[ -n "$state_file" ]] || state_file="$(default_state_file_for_repo "$repo_root")"
    [[ -n "$target" ]] || target="$session:0.0"
    [[ -n "$command" ]] || command="cd \"$repo_root\" && make run"

    if [[ -z "$shell_bin" ]]; then
      if command -v fish >/dev/null 2>&1; then
        shell_bin="$(command -v fish)"
      elif [[ -n "${SHELL:-}" ]]; then
        shell_bin="$SHELL"
      else
        shell_bin="/bin/bash"
      fi
    fi
    if [[ "$shell_bin" != /* ]]; then
      shell_bin="$(command -v "$shell_bin" 2>/dev/null || true)"
    fi
    [[ -n "$shell_bin" ]] || die "Unable to resolve shell binary"
    [[ -x "$shell_bin" ]] || die "Shell is not executable: $shell_bin"

    quoted_command="$(printf '%q' "$command")"
    command="exec \"$shell_bin\" -l -c $quoted_command"

    if tmux -L "$socket" has-session -t "$session" 2>/dev/null; then
      if [[ "$restart" == "1" ]]; then
        tmux -L "$socket" kill-session -t "$session"
      fi
    fi
    if ! tmux -L "$socket" has-session -t "$session" 2>/dev/null; then
      tmux -L "$socket" -f /dev/null new-session -Ad -s "$session" "$command"
    fi

    VERIFY_REPO_ROOT="$repo_root"
    VERIFY_TMUX_SOCKET="$socket"
    VERIFY_TMUX_SESSION="$session"
    VERIFY_TMUX_TARGET="$target"
    VERIFY_COMMAND="$command"
    VERIFY_SHELL="$shell_bin"
    VERIFY_STARTED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    old_ids="$(ghostty_window_ids_all || true)"
    launch_ghostty_attach "$socket" "$session"
    VERIFY_GHOSTTY_WINDOW_ID="$(wait_for_new_window_id "$old_ids" || true)"
    if [[ -z "$VERIFY_GHOSTTY_WINDOW_ID" ]]; then
      VERIFY_GHOSTTY_WINDOW_ID="$(ghostty_frontmost_window_id || true)"
    fi

    write_state_file "$state_file"

    echo "Initialized distill verify session."
    echo "State file: $state_file"
    echo "Repo root : $VERIFY_REPO_ROOT"
    echo "TMUX      : socket=$VERIFY_TMUX_SOCKET session=$VERIFY_TMUX_SESSION target=$VERIFY_TMUX_TARGET"
    echo "Shell     : $VERIFY_SHELL"
    echo "Ghostty   : window_id=${VERIFY_GHOSTTY_WINDOW_ID:-unknown}"
    ;;

  send)
    state_file=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        --)
          shift
          break
          ;;
        -*)
          die "Unknown send option: $1"
          ;;
        *)
          break
          ;;
      esac
    done

    if [[ -z "$state_file" ]]; then
      state_file="$(default_state_file_for_repo "$(default_repo_root)")"
    fi
    load_state_file "$state_file"
    [[ $# -gt 0 ]] || die "send requires at least one tmux key argument"

    tmux -L "$VERIFY_TMUX_SOCKET" send-keys -t "$VERIFY_TMUX_TARGET" "$@"
    ;;

  pane)
    state_file=""
    lines=200
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        --lines)
          lines="${2:-}"
          shift 2
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        *)
          die "Unknown pane option: $1"
          ;;
      esac
    done
    if [[ -z "$state_file" ]]; then
      state_file="$(default_state_file_for_repo "$(default_repo_root)")"
    fi
    load_state_file "$state_file"
    pane_output=""
    if pane_output="$(tmux -L "$VERIFY_TMUX_SOCKET" capture-pane -a -p -t "$VERIFY_TMUX_TARGET" 2>/dev/null)"; then
      :
    else
      pane_output="$(tmux -L "$VERIFY_TMUX_SOCKET" capture-pane -p -t "$VERIFY_TMUX_TARGET")"
    fi
    printf '%s\n' "$pane_output" | tail -n "$lines"
    ;;

  screenshot)
    require_cmd screencapture
    require_cmd swift
    require_cmd open

    state_file=""
    out_path=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        --out)
          out_path="$(abs_path "${2:-}")"
          shift 2
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        *)
          die "Unknown screenshot option: $1"
          ;;
      esac
    done
    if [[ -z "$state_file" ]]; then
      state_file="$(default_state_file_for_repo "$(default_repo_root)")"
    fi
    load_state_file "$state_file"

    if [[ -z "$out_path" ]]; then
      out_path="${TMPDIR%/}/codex-screenshot-ghostty-$(date +%Y%m%d-%H%M%S).png"
    fi
    mkdir -p "$(dirname "$out_path")"

    local_wid="$(ensure_window_id "$state_file")"
    screencapture -x -l "$local_wid" "$out_path"
    echo "$out_path"
    ;;

  status)
    state_file=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        *)
          die "Unknown status option: $1"
          ;;
      esac
    done
    if [[ -z "$state_file" ]]; then
      state_file="$(default_state_file_for_repo "$(default_repo_root)")"
    fi
    load_state_file "$state_file"

    tmux_ok="no"
    if tmux -L "$VERIFY_TMUX_SOCKET" has-session -t "$VERIFY_TMUX_SESSION" 2>/dev/null; then
      tmux_ok="yes"
    fi

    ghostty_wid_ok="no"
    if [[ -n "${VERIFY_GHOSTTY_WINDOW_ID:-}" ]] && ghostty_window_exists_by_id "$VERIFY_GHOSTTY_WINDOW_ID"; then
      ghostty_wid_ok="yes"
    fi

    echo "state_file=$state_file"
    echo "repo_root=$VERIFY_REPO_ROOT"
    echo "tmux_socket=$VERIFY_TMUX_SOCKET"
    echo "tmux_session=$VERIFY_TMUX_SESSION"
    echo "tmux_target=$VERIFY_TMUX_TARGET"
    echo "shell=$VERIFY_SHELL"
    echo "tmux_alive=$tmux_ok"
    echo "ghostty_window_id=${VERIFY_GHOSTTY_WINDOW_ID:-}"
    echo "ghostty_window_alive=$ghostty_wid_ok"
    echo "started_at=$VERIFY_STARTED_AT"
    ;;

  cleanup)
    state_file=""
    keep_state=0
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --state-file)
          state_file="$(abs_path "${2:-}")"
          shift 2
          ;;
        --keep-state)
          keep_state=1
          shift
          ;;
        -h|--help)
          usage
          exit 0
          ;;
        *)
          die "Unknown cleanup option: $1"
          ;;
      esac
    done
    if [[ -z "$state_file" ]]; then
      state_file="$(default_state_file_for_repo "$(default_repo_root)")"
    fi
    load_state_file "$state_file"

    if tmux -L "$VERIFY_TMUX_SOCKET" has-session -t "$VERIFY_TMUX_SESSION" 2>/dev/null; then
      tmux -L "$VERIFY_TMUX_SOCKET" kill-session -t "$VERIFY_TMUX_SESSION" || true
    fi

    if [[ "$keep_state" == "0" ]]; then
      rm -f "$state_file"
      echo "Removed state file: $state_file"
    else
      echo "Kept state file: $state_file"
    fi
    ;;

  -h|--help|help)
    usage
    ;;

  *)
    die "Unknown subcommand: $subcommand"
    ;;
esac
