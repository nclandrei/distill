# AGENTS.md

## Purpose
Reliable operating guide for agents working on this repository.
Use `$distill-verify` for visual/interactive verification tasks.
Prefer the `distill_verify_session.sh` helper for repeatable test sessions.

## Terminal Control Model

Use Ghostty window(s) backed by tmux sessions for both visual and non-visual work.
Do not use a PTY-only workflow for this project.
Use an isolated tmux socket/config to avoid user-level tmux config side effects.

### Preferred Helper

Script (from the local skill):

`/Users/anicolae/code/dotfiles/config/skills/distill-verify/scripts/distill_verify_session.sh`

State file (jj/git ignored):

`/Users/anicolae/code/distill/.distill-runtime/verify-session.env`

### Start or Reuse Session (Helper)

```bash
SCRIPT="/Users/anicolae/code/dotfiles/config/skills/distill-verify/scripts/distill_verify_session.sh"
"$SCRIPT" init \
  --repo /Users/anicolae/code/distill \
  --state-file /Users/anicolae/code/distill/.distill-runtime/verify-session.env \
  --socket distill-ui \
  --session distill-ui \
  --command 'cd /Users/anicolae/code/distill && cargo run --quiet --'
```

### Start or Reuse Session (Manual Fallback)

```bash
TMUX_SOCKET="distill-ui"
SESSION="distill-ui"
TARGET="$SESSION:0.0"

tmux -L "$TMUX_SOCKET" -f /dev/null new-session -Ad -s "$SESSION" \
  'cd /Users/anicolae/code/distill && cargo run --quiet --'
open -na Ghostty --args -e tmux -L "$TMUX_SOCKET" attach -t "$SESSION"
```

### Agent Control Pattern

```bash
# helper-driven control
SCRIPT="/Users/anicolae/code/dotfiles/config/skills/distill-verify/scripts/distill_verify_session.sh"
STATE="/Users/anicolae/code/distill/.distill-runtime/verify-session.env"
"$SCRIPT" status --state-file "$STATE"
"$SCRIPT" send --state-file "$STATE" j
"$SCRIPT" send --state-file "$STATE" Enter
"$SCRIPT" pane --state-file "$STATE" --lines 200
"$SCRIPT" screenshot --state-file "$STATE"

# manual fallback
# send keys/commands into the running terminal UI
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" 'j'
tmux -L "$TMUX_SOCKET" send-keys -t "$TARGET" Enter

# capture terminal text output when needed
tmux -L "$TMUX_SOCKET" capture-pane -p -t "$TARGET" | tail -200
```

### Cleanup (only when explicitly requested)

```bash
# helper cleanup (preferred)
SCRIPT="/Users/anicolae/code/dotfiles/config/skills/distill-verify/scripts/distill_verify_session.sh"
"$SCRIPT" cleanup --state-file /Users/anicolae/code/distill/.distill-runtime/verify-session.env

# manual fallback
tmux -L "$TMUX_SOCKET" kill-session -t "$SESSION"
```

## Screenshot (Visual Verification)

Use the `$screenshot` skill for OS-level capture when visual verification is needed.

- Save location rules:
  1. If the user gives a path, save there.
  2. If no path is given and this is a user-requested screenshot, save to Desktop with timestamp.
  3. If the agent needs inspection-only capture, save to `$TMPDIR`.

- Commands:

```bash
# Preferred (non-disruptive): capture Ghostty window by window ID without focusing it.
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

# Capture that specific window (works even if Ghostty is in background)
screencapture -x -l "$GHOSTTY_WID" /absolute/path/ghostty-window.png

# Full screen to explicit path
screencapture -x /absolute/path/screen.png

# Full screen to Desktop (default for user-requested screenshot without path)
screencapture -x "$HOME/Desktop/screenshot-$(date +%Y%m%d-%H%M%S).png"

# Full screen to temp (agent inspection)
screencapture -x "$TMPDIR/codex-screenshot-$(date +%Y%m%d-%H%M%S).png"

# Region capture
screencapture -x -R100,200,800,600 /absolute/path/region.png

# Interactive selection (window/region)
screencapture -x -i /absolute/path/selection.png
```

- Fallback only when `GHOSTTY_WID` is empty:
  - `osascript -e 'tell application "Ghostty" to activate'`
  - `sleep 1`
  - then take fullscreen/region capture
- Always return absolute output paths for created screenshots.
- Pair screenshots with `tmux capture-pane` output to prove the image matches the intended session.

## Verification Notes

- For test/demo review runs, prefer an isolated home (`HOME="$TMPDIR/...")` so real `~/.distill` data is not modified.
- For one-proposal review flows, pressing `a` can immediately complete the app and stop tmux.
- If tmux exits right after action, treat that as expected completion and verify outcomes via filesystem artifacts:
  - proposal removed from `.distill/proposals`
  - skill written in `.distill/skills`
  - decision appended to `.distill/history/decisions.jsonl`
- The helper stores reusable runtime state (socket/session/window-id) in `.distill-runtime/verify-session.env` and reuses it across screenshots/inputs.
