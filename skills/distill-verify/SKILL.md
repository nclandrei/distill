---
name: distill-verify
description: Verify and debug the Distill terminal app in a real Ghostty window using tmux control plus screenshot-based visual checks. Use for ratatui/onboarding/review screens and any interactive UI behavior.
---

# Distill Verify

Use this skill when the Distill app must be exercised as a real terminal UI, not a PTY-only stream.
Prefer the bundled helper script for stateful sessions:

`scripts/distill_verify_session.sh`

## Workflow

### 1) Start or reuse session (preferred helper)

```bash
SCRIPT="/Users/anicolae/code/distill/skills/distill-verify/scripts/distill_verify_session.sh"
"$SCRIPT" init \
  --repo /Users/anicolae/code/distill \
  --state-file /Users/anicolae/code/distill/.distill-runtime/verify-session.env \
  --socket distill-ui \
  --session distill-ui \
  --command 'cd /Users/anicolae/code/distill && make run'
```

Helper state is persisted in the repository (ignored by VCS):

`/Users/anicolae/code/distill/.distill-runtime/verify-session.env`

### 2) Drive the running session (preferred helper)

```bash
SCRIPT="/Users/anicolae/code/distill/skills/distill-verify/scripts/distill_verify_session.sh"
STATE="/Users/anicolae/code/distill/.distill-runtime/verify-session.env"

"$SCRIPT" status --state-file "$STATE"
"$SCRIPT" send --state-file "$STATE" j
"$SCRIPT" send --state-file "$STATE" Enter
"$SCRIPT" pane --state-file "$STATE" --lines 200
```

### 3) Capture screenshots (preferred helper, no focus steal)

```bash
SCRIPT="/Users/anicolae/code/distill/skills/distill-verify/scripts/distill_verify_session.sh"
STATE="/Users/anicolae/code/distill/.distill-runtime/verify-session.env"

"$SCRIPT" screenshot --state-file "$STATE"
"$SCRIPT" screenshot --state-file "$STATE" --out /absolute/path/shot.png
```

### 4) Cleanup (only when requested)

```bash
SCRIPT="/Users/anicolae/code/distill/skills/distill-verify/scripts/distill_verify_session.sh"
STATE="/Users/anicolae/code/distill/.distill-runtime/verify-session.env"

"$SCRIPT" cleanup --state-file "$STATE"
```

## Manual Fallback

Use tmux to send deterministic key input while the UI is rendered in Ghostty.

```bash
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" 'j'
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" Enter
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" Down
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" Up
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" Escape
```

Text output can be sampled as needed:

```bash
tmux -L "$TMUX_SOCKET" capture-pane -p -t "$TARGET" | tail -200
```

### Screenshot Fallback

Use the `$screenshot` skill for capture rules.

Preferred (non-disruptive): capture Ghostty window by window ID without focus steal.

```bash
GHOSTTY_WID="$(
  swift -e 'import CoreGraphics
let opts = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)
if let info = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] {
  let wins = info.filter {
    ($0[kCGWindowOwnerName as String] as? String) == "Ghostty" &&
    (($0[kCGWindowLayer as String] as? Int) ?? -1) == 0
  }
  if let w = wins.first, let id = w[kCGWindowNumber as String] as? Int { print(id) }
}'
)"

screencapture -x -l "$GHOSTTY_WID" "$TMPDIR/codex-screenshot-$(date +%Y%m%d-%H%M%S).png"
```

Inspection capture (agent-side):

```bash
screencapture -x "$TMPDIR/codex-screenshot-$(date +%Y%m%d-%H%M%S).png"
```

User-requested screenshot with no path:

```bash
screencapture -x "$HOME/Desktop/screenshot-$(date +%Y%m%d-%H%M%S).png"
```

Always return absolute file paths for screenshots.
Pair screenshots with `tmux capture-pane` output for state confirmation.

Fallback when `GHOSTTY_WID` is empty:

```bash
osascript -e 'tell application "Ghostty" to activate'
sleep 1
screencapture -x "$TMPDIR/codex-screenshot-$(date +%Y%m%d-%H%M%S).png"
```

## Review-Flow Validation (Recommended)

For demo/testing review flows, isolate app state via temporary HOME:

```bash
TEST_HOME="${TMPDIR%/}/distill-flow-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$TEST_HOME/.distill/proposals" "$TEST_HOME/.distill/skills" "$TEST_HOME/.distill/history"
```

Run review under that HOME:

```bash
tmux -L "$TMUX_SOCKET" -f /dev/null new-session -Ad -s "$SESSION" \
  "cd /Users/anicolae/code/distill && HOME=\"$TEST_HOME\" cargo run -- review"
```

If a single pending proposal is accepted (`a`), the process may exit immediately and tmux may stop.
Treat this as success when artifacts confirm it:

- proposal file removed from `$TEST_HOME/.distill/proposals`
- accepted skill written under `$TEST_HOME/.distill/skills`
- accepted entry appended to `$TEST_HOME/.distill/history/decisions.jsonl`

## Reliability Rules

- Prefer Ghostty + tmux for all Distill verification tasks.
- Do not switch to PTY-only flow for UI validation.
- Always run tmux with `-f /dev/null` and a dedicated `-L` socket for predictable behavior.
- Address panes explicitly as `session:window.pane` (for example `distill-ui:0.0`).
- Prefer `scripts/distill_verify_session.sh` so socket/session/window-id are reused from state.
- Do not kill the tmux session unless the user asks for cleanup.
- If Ghostty is unavailable, stop and report the blocker instead of silently changing terminal emulator.
