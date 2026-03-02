# distill

CLI tool that monitors AI agent sessions (Claude Code, Codex), identifies patterns, and proposes reusable skills.

## Install

```
brew install nclandrei/homebrew-tap/distill
```

## Quick Start

```
distill              # First-run onboarding
distill scan --now   # Scan agent sessions for patterns
distill review       # Accept or reject proposals
distill status       # Show current state
```

## How it works

1. **Scan** — reads agent session logs, feeds them to an AI agent to identify patterns
2. **Propose** — writes structured proposals to `~/.distill/proposals/`
3. **Review** — interactive accept/reject/skip for each proposal
4. **Sync** — accepted skills are written to all agents' config files

## Commands

| Command | Description |
|---------|-------------|
| `distill` | First-run onboarding (detects agents, configures settings) |
| `distill scan --now` | Run an immediate scan for skill proposals |
| `distill review` | Interactively review pending proposals |
| `distill status` | Show config, pending proposals, accepted skills |
| `distill watch --install` | Install scheduled scan (launchd/systemd) |
| `distill watch --uninstall` | Remove scheduled scan |
| `distill notify --check` | Check for pending proposals (used by shell hook) |
