# distill — V1 Plan

**distill** is a CLI tool that monitors AI agent sessions (Claude Code, Codex, and others), identifies patterns in how you work, and proposes new or improved skills. You review proposals interactively, and accepted skills are synced to all your agents.

## Installation

```
brew install nclandrei/tap/distill
```

## Commands

| Command                   | Purpose                                        |
|---------------------------|-------------------------------------------------|
| `distill`                 | First run triggers onboarding                   |
| `distill scan --now`      | Manual scan, don't wait for schedule             |
| `distill review`          | Interactive TUI to accept/reject/edit proposals  |
| `distill status`          | Show config, last run, pending proposal count    |
| `distill watch --install` | Install launchd plist for scheduled runs         |
| `distill watch --uninstall` | Remove launchd plist                           |

## Onboarding Flow (first run of `distill`)

1. Scan home directory for agent configs (`~/.claude`, `~/.codex`, others)
2. Ask which agents to monitor
3. Ask scan interval (daily / **weekly** default / monthly)
4. Ask which agent to use for generating skill proposals
5. Detect shell (zsh/bash/fish/other), offer to install notification hook
6. Ask notification preference (terminal / macOS native / both / none)
7. Write `~/.distill/config.yaml` and install launchd plist

## Directory Structure

```
~/.distill/
├── config.yaml              # onboarding choices & settings
├── proposals/               # pending proposals (one .md per proposal)
├── skills/                  # accepted skills (source of truth)
├── history/                 # audit log of accepted/rejected decisions
└── last-scan.json           # timestamp + session watermark
```

## Scan Pipeline

1. launchd triggers `distill scan` on configured schedule
2. Read sessions since last scan from all monitored agents
3. Load current skills from `~/.distill/skills/`
4. Call the configured agent (e.g. Claude Code) with sessions + existing skills
   - Prompt: "Here are recent sessions. Here are current skills. Propose: new skills, edits to existing skills, removals of stale skills."
5. Parse agent response into structured proposals
6. Write proposals to `~/.distill/proposals/`
7. Fire notifications (terminal hook + macOS native)

## Skill Sync

`~/.distill/skills/` is the canonical store. After `distill review` accepts a proposal, skills are synced to each agent's expected location:

- Claude Code: `~/.claude/CLAUDE.md` (appended or included)
- Codex: `~/.codex/instructions.md` (appended or included)
- Future agents: new adapters as needed

This means you author/approve once, and every agent gets the skill.

## Proposal Format

Each proposal is a markdown file in `~/.distill/proposals/` with frontmatter:

```yaml
---
type: new | improve | edit | remove
confidence: high | medium | low
target_skill: git-workflow.md    # for improve/edit/remove
evidence:
  - session: ~/.claude/sessions/abc123.jsonl
    pattern: "User manually ran git rebase workflow 4 times"
  - session: ~/.codex/sessions/def456.jsonl
    pattern: "Similar rebase pattern detected"
created: 2026-03-02T10:00:00Z
---
```

Followed by the proposed skill content (for `new`) or a before/after diff (for `improve`/`edit`).

## Notification System

### Terminal hook

Installed during onboarding into the user's shell config:

| Shell | File                                    | Syntax                                                              |
|-------|-----------------------------------------|---------------------------------------------------------------------|
| zsh   | `~/.zshrc`                              | `command -v distill &>/dev/null && distill notify --check`          |
| bash  | `~/.bashrc`                             | `command -v distill &>/dev/null && distill notify --check`          |
| fish  | `~/.config/fish/conf.d/distill.fish`    | `if command -q distill; distill notify --check; end`                |
| other | Manual — user told to add `distill notify --check` to their config |                                              |

When pending proposals exist, opening a new shell prints:

```
distill: 3 new proposals ready (2 new skills, 1 edit)
         Run 'distill review' to review them.
```

If nothing is pending, it prints nothing.

### macOS native notification

Triggered after `distill scan` completes, via `osascript` (fallback) or `terminal-notifier` (if available):

```
distill — 3 new skill proposals ready. Run 'distill review'.
```

## Tech Stack

| Component        | Choice                            | Rationale                                       |
|------------------|-----------------------------------|-------------------------------------------------|
| Language         | Rust                              | Single binary, strict compiler catches bugs at build time, strong type system for modeling proposals/skills/agents |
| TUI framework    | `ratatui`                         | Standard Rust TUI framework, active community   |
| CLI framework    | `clap` (derive)                   | Derive macros for zero-boilerplate arg parsing   |
| Config           | `serde` + `serde_yaml`            | Idiomatic Rust serialization                     |
| Async runtime    | `tokio` (if needed for subprocess) | Only if async subprocess calls warrant it, otherwise keep sync |
| Notifications    | `osascript` / `terminal-notifier` | No dependencies by default                      |
| Markdown parsing | `pulldown-cmark`                  | Parse/generate proposal markdown with frontmatter |
| Distribution     | `cargo-dist` + Homebrew tap       | Rust-native release pipeline with Homebrew support |
| CI               | GitHub Actions                    | `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt --check` |

## Target Platforms

| OS        | Scheduler                          | Notifications                        |
|-----------|------------------------------------|--------------------------------------|
| **macOS** | launchd plist (`~/Library/LaunchAgents/`) | `osascript` / `terminal-notifier` |
| **Linux** | systemd user unit (`~/.config/systemd/user/`) | `notify-send` / `libnotify`    |

Platform-specific code is isolated behind traits with conditional compilation (`#[cfg(target_os = "macos")]` / `#[cfg(target_os = "linux")]`).

---

## Work Breakdown

### Dependency graph

```
[1] Project scaffold ─────────────────────────┐
         │                                     │
         ├──► [2] Config system                │
         │         │                           │
         │         ├──► [4] Onboarding flow    │
         │         │         │                 │
         │         │         ├──► [8] Shell hook installer
         │         │         └──► [9] launchd plist installer
         │         │                           │
         │    [3] Agent adapters (read/write)   │
         │         │                           │
         │         ├──► [5] Session reader      │
         │         └──► [7] Skill sync          │
         │                   │                 │
         │    [6] Scan engine ◄────────────────┘
         │         │
         │         ▼
         │    [10] Proposal writer
         │         │
         │         ▼
         │    [11] Review TUI
         │         │
         │         ▼
         │    [12] Notification system
         │
         └──► [13] Homebrew formula + goreleaser
```

### Task breakdown

Below, tasks are grouped into **waves**. All tasks within a wave can be executed in parallel. A wave cannot start until all tasks in the previous wave are complete.

---

#### Wave 0 — Sequential foundation (NOT parallelizable)

> Must be done first by a single agent. Everything else depends on this.

| # | Task | Description | Done |
|---|------|-------------|------|
| 1 | **Project scaffold** | `cargo init`, workspace structure (`src/`, `src/commands/`, `src/agents/`, `src/config/`, `src/scanner/`, `src/proposals/`, `src/review/`, `src/sync/`, `src/notify/`, `src/schedule/`, `src/shell/`), `main.rs` entry point with clap derive root command, `Cargo.toml` with all dependencies, `Makefile` with `build`/`test`/`lint`/`fmt` targets. | yes |

---

#### Wave 1 — Core modules (fully parallel, 3 agents)

> These have no dependencies on each other, only on the scaffold.

| # | Task | Description | Done |
|---|------|-------------|------|
| 2 | **Config system** | `src/config/` — define `Config` struct with serde derive (monitored agents, interval, shell, notification prefs, agent for generation). Load/save `~/.distill/config.yaml` via `serde_yaml`. Ensure `~/.distill/` directory creation with `std::fs`. Enums for `Interval` (`Daily`, `Weekly`, `Monthly`), `NotificationPref`, `ShellType`. Unit tests. | yes |
| 3 | **Agent adapters** | `src/agents/` — define `Agent` trait with `fn read_sessions(&self, since: DateTime<Utc>) -> Result<Vec<Session>>` and `fn write_skill(&self, skill: &Skill) -> Result<()>`. Implement `ClaudeAdapter` (reads `~/.claude/` sessions, writes to `CLAUDE.md`) and `CodexAdapter` (reads `~/.codex/` sessions, writes to `instructions.md`). Define `Session` and `Skill` structs. Enum `AgentKind { Claude, Codex }`. Unit tests with fixture data. | yes |
| 13 | **Homebrew + cargo-dist** | `cargo-dist` config in `Cargo.toml` (`[dist]` metadata or `dist-workspace.toml`), GitHub Actions release workflow, Homebrew formula in `nclandrei/homebrew-tap`. Can be done now since it just needs `main.rs` to exist. | yes |

---

#### Wave 2 — Features that depend on config + adapters (fully parallel, 4 agents)

| # | Task | Description | Done |
|---|------|-------------|------|
| 4 | **Onboarding flow** | `src/onboard/` — interactive first-run flow using `dialoguer` (Select, MultiSelect, Confirm prompts) or ratatui. Scan home dir for agent configs, present multi-select for agents, interval picker, agent-for-generation picker. Calls config system to persist. Wire to root command (run onboarding if no config exists). | yes |
| 5 | **Session reader** | `src/scanner/reader.rs` — given a `Vec<Box<dyn Agent>>` and a `since: DateTime<Utc>`, collect all sessions. Deduplicate by session ID. Return `Vec<Session>`. Read/update `last-scan.json` via serde. Unit tests. | yes |
| 7 | **Skill sync** | `src/sync/` — read all `.md` files from `~/.distill/skills/` via `std::fs::read_dir`, call each agent adapter's `write_skill()` to sync. Idempotent (compare content hashes, skip unchanged). Wire to an internal sync step called after review. Unit tests. | yes |
| 10 | **Proposal writer** | `src/proposals/` — define `Proposal` struct with serde derive. Enum `ProposalType { New, Improve, Edit, Remove }`, enum `Confidence { High, Medium, Low }`. Serialize to markdown with YAML frontmatter (serde_yaml for frontmatter block, raw string for body). Write to / read from `~/.distill/proposals/`. List pending proposals. Unit tests. | yes |

---

#### Wave 3 — Scan engine (depends on Wave 2)

> This is the brain of the tool. Depends on session reader, proposal writer, and agent adapters.

| # | Task | Description | Done |
|---|------|-------------|------|
| 6 | **Scan engine** | `src/scanner/engine.rs` — orchestrate a full scan: call session reader, load existing skills, build prompt for the configured generation agent, invoke the agent (via `std::process::Command`, e.g. `claude --print` or `codex --quiet`), parse the agent's structured response into `Vec<Proposal>`, pass to proposal writer. Wire to `distill scan` subcommand with `--now` flag. Integration test with mock agent (inject a fake command that returns fixture output). | yes |

---

#### Wave 4 — User-facing features (parallel, 3 agents)

> Depend on proposals existing (Wave 3), but can be built in parallel with each other.

| # | Task | Description | Done |
|---|------|-------------|------|
| 8 | **Shell hook installer** | `src/shell/` — detect shell from `$SHELL` env var, generate correct hook snippet, write to correct config file (`~/.zshrc`, `~/.bashrc`, `~/.config/fish/conf.d/distill.fish`). Idempotent (check if marker comment already exists before writing). Called during onboarding. `distill notify --check` subcommand: count files in `~/.distill/proposals/` via `read_dir`, print summary or exit silently. | |
| 9 | **Scheduler installer** | `src/schedule/` — trait `Scheduler` with `install()`, `uninstall()`, `status()`. macOS impl: generate `~/Library/LaunchAgents/com.distill.agent.plist`, load/unload via `launchctl`. Linux impl: generate `~/.config/systemd/user/distill.service`, enable/disable via `systemctl --user`. `distill watch --install` and `--uninstall` subcommands. Conditional compilation via `#[cfg(target_os)]`. | |
| 11 | **Review TUI** | `src/review/` — ratatui TUI for `distill review`. List proposals in a selectable list widget, show diff/content in a scrollable pane for each, keybindings for accept (`a`), reject (`r`), edit (`e`), snooze (`s`), batch accept all (`A`). On accept: move proposal content to `~/.distill/skills/`, log decision to `~/.distill/history/`, trigger skill sync. On reject: log and delete proposal. | |

---

#### Wave 5 — Notifications + status (parallel, 2 agents)

| # | Task | Description | Done |
|---|------|-------------|------|
| 12 | **Notification system** | `src/notify/` — trait `Notifier` with `send(title, body) -> Result<()>`. macOS impl: `osascript` via `std::process::Command` (with `terminal-notifier` as optional enhancement). Linux impl: `notify-send` via `std::process::Command`. Called at end of `distill scan`. Respect user's notification preference from config. Conditional compilation via `#[cfg(target_os)]`. | |
| 14 | **`distill status` command** | `src/commands/status.rs` — show current config, last scan time, next scheduled scan, number of pending proposals, number of accepted skills. Simple table output via `comfy-table` or similar. | |

---

#### Wave 6 — Integration + polish (sequential)

| # | Task | Description | Done |
|---|------|-------------|------|
| 15 | **End-to-end integration test** | Full flow: onboard → scan → proposals created → review → skills synced. Can use mock agent responses. | |
| 16 | **README** | Installation instructions, demo GIF placeholder, usage examples. | |

---

### Summary table

| Wave | Tasks | Parallelism | Depends on |
|------|-------|-------------|------------|
| 0    | 1     | sequential  | —          |
| 1    | 2, 3, 13 | 3 agents | Wave 0     |
| 2    | 4, 5, 7, 10 | 4 agents | Wave 1  |
| 3    | 6     | 1 agent     | Wave 2     |
| 4    | 8, 9, 11 | 3 agents | Wave 3   |
| 5    | 12, 14 | 2 agents   | Wave 4     |
| 6    | 15, 16 | sequential | Wave 5     |
