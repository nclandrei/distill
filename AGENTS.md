# AGENTS.md

## Purpose
Reliable operating guide for agents working on this repository.
Use `$distill-verify` for visual/interactive verification tasks.

## Terminal Control Model

Use Ghostty window(s) backed by tmux sessions for both visual and non-visual work.
Do not use a PTY-only workflow for this project.

### Start or Reuse Session

```bash
tmux new-session -Ad -s distill-ui 'cd /Users/anicolae/code/distill && make run'
open -na Ghostty --args -e tmux attach -t distill-ui
```

### Agent Control Pattern

```bash
# send keys/commands into the running terminal UI
tmux send-keys -t distill-ui 'j'
tmux send-keys -t distill-ui Enter

# capture terminal text output when needed
tmux capture-pane -p -t distill-ui | tail -200
```

### Cleanup (only when explicitly requested)

```bash
tmux kill-session -t distill-ui
```

## Screenshot (Visual Verification)

Use the `$screenshot` skill for OS-level capture when visual verification is needed.

- Save location rules:
  1. If the user gives a path, save there.
  2. If no path is given and this is a user-requested screenshot, save to Desktop with timestamp.
  3. If the agent needs inspection-only capture, save to `$TMPDIR`.

- Commands:

```bash
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

- Always return absolute output paths for created screenshots.
