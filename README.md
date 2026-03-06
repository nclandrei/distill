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
| `distill convert <server> [--json] [--backend codex\|claude] [--backend-auto]` | One-shot V4 flow: discover -> build -> runtime contract-test -> apply (atomic per server) |
| `distill convert discover <server\|--all> [--out dossier.json] [--backend ...]` | Generate backend-neutral dossier JSON from runtime + backend tool analysis |
| `distill convert build --from-dossier dossier.json [--skills-dir ...]` | Generate orchestrator + per-tool skill files from dossier |
| `distill convert contract-test --from-dossier dossier.json [--report ...] [--allow-side-effects] [--probe-timeout-seconds N] [--probe-retries N]` | Execute real runtime `tools/call` probes per tool (`happy-path`, `invalid-input`, `side-effect-safety`) |
| `distill convert apply --from-dossier dossier.json --yes [--skills-dir ...]` | Re-run runtime contract probes, then apply only fully passing dossiers and mutate MCP config atomically |
| `distill convert ... --backend-health` | Print backend availability diagnostics (codex/claude) |
| `distill convert list [--json] [--config <path>]` | Discover MCP servers from known config locations |
| `distill convert inspect <server> [--json] [--config <path>]` | Inspect one MCP server profile and recommendation |
| `distill convert plan <server> [--mode auto|hybrid|replace] [--json]` | Generate a conversion plan with safety gates |
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

### 4) MCP Convert V4 (non-interactive)

One-shot (full atomic conversion per server):

```bash
distill convert custom-1:playwright --json
```

Stepwise V4:

```bash
distill convert discover custom-1:playwright --out dossier.json --json --config /path/to/mcp.json
distill convert build --from-dossier dossier.json --json
distill convert contract-test --from-dossier dossier.json --report contract.json --json
# optional probe controls:
#   --allow-side-effects
#   --probe-timeout-seconds 45
#   --probe-retries 1
distill convert apply --from-dossier dossier.json --yes --json
```

Backend selection:
- `--backend codex|claude`: explicit override (fails if unavailable)
- default: auto-detect (`codex` then `claude`)
- `--backend-health`: print diagnostics
- `--allow-side-effects`: allow explicit side-effectful probes (default is safe/non-mutating)
- `--probe-timeout-seconds <N>`: per-probe runtime timeout (default `30`)
- `--probe-retries <N>`: retries for failed probes (default `0`)
- config keys (`~/.distill/config.yaml`):
  - `convert.backend_preference: auto|codex|claude`
  - `convert.backend_timeout_seconds`
  - `convert.backend_chunk_size`
  - `convert.probe_timeout_seconds`
  - `convert.probe_retries`
  - `convert.allow_side_effect_probes`

V4 behavior notes:
- Source of truth for tool surface is runtime `tools/list` introspection.
- Codex and Claude backends both produce the same strict dossier schema.
- Contract tests execute live MCP `tools/call` probes and record request/response previews, duration, and error kind.
- Missing schema + missing deterministic probe args is a hard `schema-gap` failure.
- Default safety policy never requires mutating probes; write/destructive tools must have a safe guard path unless `--allow-side-effects` is set.
- If source code is unavailable, fallback evidence remains runtime metadata + runtime contract tests.
- Final conversion gate is atomic per server: no MCP config mutation unless all tool dossiers and runtime probes pass.
- Skills default to `~/.agents/skills/`.
- Legacy `list|inspect|plan|verify` commands remain available for troubleshooting/migration.

### 5) Live MCP smoke harness

Run reusable end-to-end conversion checks against live MCP packages using isolated HOME/config:

```bash
make convert-live-smoke
```

By default this runs:
- `memory=@modelcontextprotocol/server-memory`
- `chrome-devtools=chrome-devtools-mcp@latest`

Custom matrix example:

```bash
./scripts/convert-live-mcp-smoke.sh \
  --servers "memory=@modelcontextprotocol/server-memory,chrome-devtools=chrome-devtools-mcp@latest" \
  --keep
```

What it validates per server:
- `convert discover ... --out dossier.json`
- `convert build --from-dossier dossier.json`
- `convert contract-test --from-dossier dossier.json`
- `convert apply --from-dossier dossier.json --yes`
- one-shot `convert <server>`
