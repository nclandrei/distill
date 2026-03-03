# distill — V2 Roadmap

Ideas and features planned after V1 ships.

---

## `distill convert` — MCP to Skills

### Problem

MCP (Model Context Protocol) servers dump their full tool schemas into the agent's context window on every invocation. An agent with 5 MCPs connected can burn thousands of tokens on tool definitions before it does anything useful — and most of those tools aren't relevant to the current task.

Skills are the opposite: markdown files loaded on-demand, only when the agent determines they're contextually relevant. Same capabilities, fraction of the context cost.

### Solution

```
distill convert --mcp <server-name>
```

This would:

1. **Introspect** the MCP server — read its tool definitions, JSON schemas, descriptions, and examples
2. **Group** related tools into logical skill units (e.g., a Jira MCP with 15 tools becomes 3-4 focused skills)
3. **Generate** skill markdown files that teach the agent how to accomplish the same tasks
4. **Output** to `~/.distill/skills/` and sync to all agents

### Example

A Playwright MCP exposing tools like `navigate`, `click`, `fill`, `screenshot`, `evaluate` becomes:

- `browser-navigation.md` — how to navigate and interact with pages
- `browser-testing.md` — how to run assertions and capture screenshots
- `browser-form-automation.md` — how to fill and submit forms

Each skill contains the knowledge of *when* and *how* to use those tools, without loading all tool schemas into context at all times.

### Subcommands

| Command | Purpose |
|---|---|
| `distill convert --mcp <name>` | Convert a single MCP server to skills |
| `distill convert --mcp-all` | Convert all configured MCP servers |
| `distill convert --list-mcps` | List detected MCP servers and their tool counts |
| `distill convert --dry-run --mcp <name>` | Preview what skills would be generated |

### Where to read MCP configs from

- Claude Code: `~/.claude/mcp.json` or project-level `.claude/mcp.json`
- Codex: wherever Codex stores MCP config
- Generic: allow `--mcp-config <path>` for arbitrary configs

---

## Project-Level Skills

### Problem

V1 writes all skills globally to `~/.distill/skills/`. But developers work on multiple projects with different tech stacks, conventions, and workflows. A Rails project's skills are irrelevant (and potentially confusing) when working on a Go microservice.

### Solution

`distill` detects per-project session directories (e.g., `.claude/` inside a git repo) and proposes project-specific skills that live in the project, not globally.

```
~/my-rails-app/
├── .distill/
│   └── skills/
│       ├── rails-migration-workflow.md
│       └── rspec-testing-patterns.md
├── .claude/
│   └── CLAUDE.md  ← project skills synced here
└── src/
```

### Behavior

- During scan, `distill` checks if sessions originated from a project directory
- Project-specific patterns produce project-specific proposals
- `distill review` shows proposals grouped by scope (global vs project)
- Project skills take precedence over global skills when there's a conflict

---

## `distill sync-agents` — AGENTS.md Drift Detection

### Problem

`AGENTS.md` files drift over time. Teams discover better test commands, config flags, CI checks, and workflow tweaks during real sessions, but those improvements often stay buried in chat history and never make it back into project instructions.

### Solution

Add a periodic, user-configurable sync pass that reviews recent sessions and proposes `AGENTS.md` updates per project:

```
distill sync-agents --projects <allowlist>
```

### Behavior

1. Scan latest sessions per configured project and extract candidate instruction tweaks (config, test commands, CI/debug workflows, gotchas)
2. Compare candidates against current `AGENTS.md` to avoid duplicate or conflicting guidance
3. Generate a patch proposal instead of editing blindly
4. Preserve user-authored rules and style; only update/add where confidence is high
5. Route changes through review (`distill review`) with rationale and source session references

### Guardrails

- Project scope is explicit and user-controlled (allowlist or config file)
- Never overwrite explicit user constraints; prefer additive updates
- Require repeated evidence (or explicit user approval) before adding new defaults
- Support `--dry-run` so teams can inspect changes before applying

### Possible commands

| Command | Purpose |
|---|---|
| `distill sync-agents --projects <list>` | Propose `AGENTS.md` updates for selected projects |
| `distill sync-agents --all-configured` | Run across every configured project |
| `distill sync-agents --dry-run` | Preview proposals without writing files |
| `distill sync-agents --since <date>` | Limit evidence window to recent sessions |

### Reference automation profile (from user concept)

- Name: `Sync AGENTS.md`
- Project scope: selected repositories (allowlist-based)
- Execution environment: worktree
- Example cadence: daily at `18:00`, weekdays (Mon-Fri)
- Expected report: inbox summary with `Updated` or `No changes`, evidence used (`files/commits`), and exact sections changed or skipped

### Reference prompt template

```text
Review each selected repository and keep AGENTS.md aligned with current engineering workflow. Read AGENTS.md first, then inspect recent commits and changed files. Detect concrete updates to commands, test workflows, CI behavior, tool/config paths, environment setup, and branch/review conventions. Update AGENTS.md only when evidence exists in repository files or git history, keep edits minimal, preserve style, and do not invent rules. If no update is needed, leave files unchanged. Post an inbox summary with: Updated or No changes, evidence used (files/commits), and exact sections changed or skipped.
```

---

## Skill Deduplication

### Problem

Over time, skills accumulate. Global skills might overlap with project skills. Two skills might cover the same workflow slightly differently. Renamed or evolved skills might leave stale versions behind.

### Solution

Periodic deduplication pass (can run as part of `distill scan` or as `distill dedupe`):

1. Semantic comparison of all skills (using the configured agent)
2. Detect overlaps, conflicts, and stale skills
3. Propose merges, removals, or consolidations as regular proposals
4. User reviews via `distill review` as usual

---

## `distill publish` — Skill Registry

### Problem

Good skills are valuable beyond a single developer. Teams and the community could benefit from sharing battle-tested skills.

### Solution

A public registry (think Homebrew formulas, but for skills):

```
distill publish git-workflow.md
```

This would:

1. Validate the skill format
2. Push to a central registry (GitHub-based, like Homebrew taps)
3. Allow others to install: `distill install @nclandrei/git-workflow`

### Registry structure

```
github.com/distill-skills/registry/
├── skills/
│   ├── git-workflow/
│   │   ├── skill.md
│   │   ├── metadata.yaml  (author, version, agents, tags)
│   │   └── README.md
│   └── docker-debugging/
│       ├── skill.md
│       ├── metadata.yaml
│       └── README.md
└── index.yaml
```

### Commands

| Command | Purpose |
|---|---|
| `distill publish <skill>` | Publish a skill to the registry |
| `distill search <query>` | Search the registry |
| `distill install <skill>` | Install a community skill |
| `distill update` | Update installed community skills |

---

## Team Sync

### Problem

Teams working on the same codebase develop the same patterns independently. One developer's distill might propose a skill that another developer already has.

### Solution

Shared distill config committed to the repo:

```
my-project/
├── .distill/
│   ├── team-config.yaml    # shared settings
│   └── skills/             # team skills (committed)
│       ├── deployment-workflow.md
│       └── code-review-checklist.md
```

### Behavior

- Team skills are version-controlled alongside the code
- `distill` merges team skills with personal skills (personal takes precedence on conflict)
- New team members get project skills automatically on clone
- Proposals can be flagged as "team-wide" during review, which means they get committed to the repo's `.distill/skills/` rather than `~/.distill/skills/`

---

## Preference Learning

### Problem

Early on, distill's proposals will include noise — suggestions the user repeatedly rejects. Over time, the tool should learn what you care about and stop proposing things you don't want.

### Solution

Track accept/reject history in `~/.distill/history/` and feed it back into the scan prompt:

- "The user has rejected 3 proposals about testing workflows — deprioritize testing-related skills"
- "The user consistently accepts git-related proposals — weight those higher"
- Confidence thresholds adjust per category based on acceptance rate

This doesn't require ML — it's prompt engineering with historical context.

---

## Priority Order

Roughly ordered by value and implementation complexity:

| # | Feature | Notes | Done |
|---|---------|-------|------|
| 1 | **`distill convert` (MCP to skills)** | high value, addresses a real pain point now | |
| 2 | **Project-level skills** | natural extension, needed once people use it on multiple projects | |
| 3 | **`distill sync-agents` (AGENTS.md drift detection)** | keeps project instructions current with real workflows | |
| 4 | **Skill deduplication** | maintenance feature, prevents skill sprawl | |
| 5 | **Team sync** | multiplier feature, makes distill valuable for teams | |
| 6 | **`distill publish`** | community feature, needs critical mass first | |
| 7 | **Preference learning** | refinement, improves quality over time | |
