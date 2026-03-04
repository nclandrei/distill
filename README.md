# distill

CLI tool that monitors AI agent sessions (Claude Code, Codex), identifies recurring patterns, and turns them into reusable skills.

## For Humans

### Install

```bash
brew install nclandrei/homebrew-tap/distill
```

### Quick Start

```bash
distill              # First-run onboarding (interactive TUI)
distill scan --now   # Scan agent sessions for new proposals
distill review       # Review proposals in TUI (accept/reject/edit/snooze/batch)
distill status       # Show config + pending proposals + last scan
```

### Commands

| Command | Description |
|---------|-------------|
| `distill` | Run onboarding on first run, otherwise print quick usage hints |
| `distill onboard` | Run onboarding TUI explicitly |
| `distill scan --now` | Run an immediate scan for skill proposals |
| `distill review` | Review pending proposals in a TUI (`a/r/e/s/A`) |
| `distill status` | Show config, pending proposals, accepted skills |
| `distill watch --install` | Install scheduled scan (launchd/systemd) |
| `distill watch --uninstall` | Remove scheduled scan |
| `distill notify --check` | Check for pending proposals (used by shell hook) |

## For AI Agents

Use one-shot JSON modes to avoid TUI interaction.

Reference examples are committed in:
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
- Contains all pending proposals plus an editable `decision` field.
- Missing decisions default to `skip`.
- Applying decisions writes skills, logs history, removes processed proposals, and syncs accepted skills to configured agents.

### 3) Stdin/stdout mode

Both onboarding and review JSON flags accept `-` as path:
- `--write-json -` writes JSON to stdout
- `--apply-json -` reads JSON from stdin
