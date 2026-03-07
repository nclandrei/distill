# distill

![distill icon](assets/icons/png/color/distill-color-256.png)

`distill` helps you turn repeated AI-agent work into reusable skills. It watches Claude/Codex sessions, proposes improvements, and lets you accept them with a quick review flow.

## Install

```bash
# Homebrew (recommended)
brew install nclandrei/homebrew-tap/distill

# crates.io
cargo install distill --locked
```

Icon assets used by notifications and docs:
- SVG: `assets/icons/distill-icon.svg`
- PNG: `assets/icons/png/color/distill-color-256.png`

## Requirements

- Distill needs at least one supported agent CLI on `PATH`:
  - Claude Code via `claude`
  - Codex CLI via `codex`
- Onboarding marks an agent as detected only when its CLI is discoverable on `PATH`.
- Scans require the configured `proposal_agent` CLI to be installed and already authenticated.
- Distill reads local session logs from:
  - Claude: `~/.claude/projects/**/*.jsonl`
  - Codex: `~/.codex/sessions/**/*.jsonl`
- If a scan stalls because the upstream agent is slow, raise `DISTILL_AGENT_TIMEOUT_SECS` (default: 900 seconds).

## Quick Start

```bash
which claude || which codex
distill              # First-run onboarding (interactive TUI)
distill scan --now   # Scan sessions for new skill proposals
distill review       # Review proposals (accept/reject/edit/snooze/batch)
distill status       # Check config + pending proposals + last scan
```

## Local Commit Checks (Git + jj)

For this repo, run:

```bash
make hooks-install
```

This configures:
- Git `pre-commit` hook (via `core.hooksPath`) to run local checks before `git commit`
- repo-local `jj` aliases:
  - `jj safe-commit ...`
  - `jj safe-describe ...`

The shared check pipeline auto-applies format/fixes, then verifies tests:

```bash
cargo fmt --all
cargo clippy --fix --allow-dirty --allow-staged -- -D warnings
cargo fmt --all
cargo clippy -- -D warnings
cargo test
```

Notes:
- `jj` currently does not have native commit hooks, so the practical equivalent is using the `safe-*` aliases for commit-time enforcement.
- You can run the same pipeline anytime with `make local-checks`.

## Commands

| Command | Description |
|---------|-------------|
| `distill` | Run onboarding on first run, otherwise print quick usage hints |
| `distill onboard` | Run onboarding TUI explicitly |
| `distill scan --now` | Run an immediate scan for skill proposals |
| `distill review` | Review pending proposals in a TUI (`a/r/e/s/A`) |
| `distill dedupe [--dry-run]` | Detect duplicate global skills and propose removals |
| `distill sync-agents ...` | Propose `AGENTS.md` updates from project evidence |
| `distill status` | Show config, pending proposals, accepted skills |
| `distill watch --install` | Install scheduled scan (launchd/systemd) |
| `distill watch --uninstall` | Remove scheduled scan |
| `distill notify --check` | Check for pending proposals (used by shell hook) |

`sync-agents` examples:
- `distill sync-agents --projects /abs/repo --dry-run`
- `distill sync-agents --projects /abs/repo1,/abs/repo2 --save-projects`
- `distill sync-agents --all-configured`
- `distill sync-agents --list-configured`

## Notifications

- `notifications`: `terminal|native|both|none`
- `notification_icon`: `null` or absolute icon path
- Terminal notifications show an inline icon when supported by the terminal:
  - Ghostty/kitty-like terminals via kitty graphics protocol
  - iTerm2 via OSC 1337
  - Fallback is text-only
- Optional terminal controls:
  - `DISTILL_TERMINAL_IMAGE=on|off` (`on` by default; set `off` to disable)
  - `DISTILL_TERMINAL_IMAGE_PROTOCOL=ansi|kitty|iterm|none`
- If running inside tmux, enable passthrough for image rendering: `set -g allow-passthrough on`.
- In tmux sessions, distill auto-detects the attached terminal (`ghostty/kitty` first, then `iTerm`) and falls back to text-only when passthrough is disabled.
- SVG `notification_icon` values are rasterized to PNG for terminal inline rendering.
- On Linux native notifications, `notification_icon: null` falls back to the built-in project icon automatically.
- On macOS native notifications, distill tries `terminal-notifier -appIcon <icon>` first and falls back to AppleScript notification if that path fails.

## Platform Notes

- Scheduled scans use `launchd` on macOS and `systemd --user` on Linux.
- Native notifications use `terminal-notifier` on macOS and `notify-send` on Linux when available.
- If those native notifier tools are missing, terminal notifications still work when `notifications` is `terminal` or `both`.

## For AI Agents

Use one-shot JSON modes to avoid TUI interaction.

Reference examples:
- `examples/onboarding.json`
- `examples/review.json`

### 1) Onboarding (non-interactive)

```bash
distill onboard --write-json onboarding.json
# edit onboarding.json
distill onboard --apply-json onboarding.json
```

`onboarding.json` fields:
- `format_version` (currently `1`)
- `agents` (`[{"name":"claude|codex","enabled":true|false}]`)
- `scan_interval` (`daily|weekly|monthly`)
- `proposal_agent` (`claude|codex`)
- `shell` (`zsh|bash|fish|other`)
- `notifications` (`terminal|native|both|none`)
- `notification_icon` (`null` or absolute path)
- `install_shell_hook` (`true|false`)

### 2) Review (non-interactive)

```bash
distill review --write-json review.json
# set decision for each proposal: accept | reject | skip
distill review --apply-json review.json
```

`review.json` behavior:
- Contains all pending proposals plus an editable `decision` field
- Missing decisions default to `skip`
- Applying decisions writes skills, logs history, and removes processed proposals
- Accepted skills are synced to:
  - `~/.agents/skills/<skill-name>/SKILL.md` (default shared target)
  - `~/.claude/skills/<skill-name>/SKILL.md` when Claude is enabled

### 3) Stdin/stdout mode

Both onboarding and review JSON flags accept `-` as path:
- `--write-json -` writes JSON to stdout
- `--apply-json -` reads JSON from stdin
