# distill

![distill icon](assets/icons/png/color/distill-color-256.png)

`distill` helps you turn repeated AI-agent work into reusable skills. It watches Claude/Codex sessions, proposes improvements, and lets you accept them with a quick review flow.

## Install

```bash
# Homebrew (recommended)
brew install nclandrei/homebrew-tap/distill

# crates.io
cargo install distill
```

Icon assets used by notifications and docs:
- SVG: `assets/icons/distill-icon.svg`
- PNG: `assets/icons/png/color/distill-color-256.png`

## Quick Start

```bash
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
| `distill convert <server> [--replace --yes] [--json] [--config <path>]` | One-shot inspect + plan + apply + verify flow (safe hybrid by default) |
| `distill convert list [--json] [--config <path>]` | Discover MCP servers from known config locations |
| `distill convert inspect <server> [--json] [--config <path>]` | Inspect one MCP server profile and recommendation |
| `distill convert plan <server> [--mode auto|hybrid|replace] [--json]` | Generate a conversion plan with safety gates |
| `distill convert apply <server> [--mode auto|hybrid|replace] [--yes] [--json]` | Generate one orchestrator skill plus per-tool capability skills (and optionally update MCP config for replace mode) |
| `distill convert verify <server> [--json] [--config <path>]` | Verify generated orchestrator/capability skill parity against live MCP tools |
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

### 4) MCP convert planning (non-interactive)

`distill convert` supports one-shot conversion by default:

```bash
distill convert custom-1:playwright --json
distill convert custom-1:playwright --replace --yes --json
```

Expert/automation subcommands are still available:

```bash
distill convert list --json
distill convert inspect custom-1:playwright --json --config /path/to/mcp.json
distill convert plan custom-1:playwright --mode auto --json --config /path/to/mcp.json
distill convert apply custom-1:playwright --mode auto --json --config /path/to/mcp.json
distill convert verify custom-1:playwright --json --config /path/to/mcp.json
```

Behavior notes:
- Discovery reads default MCP locations (`~/.claude/mcp.json`, `~/.claude/settings.json`, `~/.codex/mcp.json`, `~/.codex/config.toml`, project-level variants, and shared config paths) plus any `--config` paths.
- `distill convert <server>` defaults to safe hybrid application and then verifies parity.
- `inspect` accepts either a full server id (`source:name`) or a unique server name.
- `plan --mode replace` is blocked automatically when the server is not a safe replacement candidate.
- `apply` writes one orchestrator skill (`mcp-<server>.md`) plus capability skills (`mcp-<server>-tool-<tool>.md`) into `~/.distill/skills/` by default, and stores parity metadata in `~/.distill/skills/.distill-manifests/`.
- `apply --mode replace` requires `--yes`, creates a backup of the MCP config, and removes the target server entry when safe.
- `verify` checks required tool coverage and generated file presence, and reports parity gaps (`missing_in_server`, `missing_in_skill`, `missing_skill_files`).
