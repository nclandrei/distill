---
name: add-coding-agent
description: Add a new Distill-supported coding agent end to end, including registry wiring, session ingestion, proposal-runner support, docs, tests, and cli-verify validation.
---

# Add Coding Agent

## When to use

Use this when Distill needs to support a new coding agent CLI from top to bottom, not just a one-off local experiment.

## Steps

1. Add the agent to the shared registry.
   - Extend `AgentKind` and any shared agent metadata so CLI name, config roots, skill targets, session source, and proposal-runner profile are declared once.
   - Prefer a new adapter only when the agent needs custom session discovery or skill sync behavior.

2. Wire session ingestion through the adapter layer.
   - Reuse file-based discovery for agents with stable on-disk session logs.
   - If the agent exposes official CLI commands for session listing/export, prefer those over reverse-engineering private storage.
   - Ensure Distill can stage compact session summaries for scan prompts.

3. Wire proposal-agent execution through the shared runner.
   - Add the command profile, output extraction rules, and any isolated-home/auth copying needed for non-interactive runs.
   - Keep scanner and `sync-agents` on the same runner path.
   - Lock down permissions for read-only proposal extraction when the upstream CLI supports it.

4. Sync accepted skills to the right target roots.
   - Preserve Distill’s structured `SKILL.md` generation.
   - Add every native agent skill destination that should receive accepted skills.
   - Keep shared compatibility mirrors only when the agent actually needs them.

5. Update user-facing surfaces.
   - Onboarding TUI and onboarding JSON validation/export/apply.
   - Config defaults, status output, README/examples, and any help text that lists supported agents.
   - Repo guidance in `AGENTS.md` when the new workflow should be discoverable by future agents.

6. Add automated coverage.
   - Unit tests for registry/defaults, session discovery parsing, proposal output parsing, and native skill sync paths.
   - E2E tests with mock executables on `PATH` for monitored-session scans and `proposal_agent` runs.
   - Add a `sync-agents` test when the shared runner changes.

## Verification

Run:

```bash
cargo test
make local-checks
```

Then run `cli-verify` in Ghostty/tmux against a temp `HOME` with mock binaries:

```bash
SCRIPT=""
for candidate in \
  "$HOME/.agents/skills/cli-verify/scripts/cli_verify_session.sh" \
  "$HOME/.claude/skills/cli-verify/scripts/cli_verify_session.sh" \
  "$HOME/.codex/skills/cli-verify/scripts/cli_verify_session.sh"
do
  if [ -x "$candidate" ]; then
    SCRIPT="$candidate"
    break
  fi
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
"$SCRIPT" init --repo "$REPO_ROOT" --command 'cd "$REPO_ROOT" && cargo run --quiet --'
```

Verify:

- onboarding detects the new agent when its mock CLI is present
- scan can stage one session from the new agent
- accepting a proposal syncs a skill into the agent’s native skill root

## Pitfalls

- Do not duplicate agent-specific command logic in `scan`, `sync-agents`, and review sync; centralize it first.
- Do not rely on undocumented session storage when the upstream CLI exposes a supported export/list path.
- Do not skip e2e tests for isolated auth/config copying; proposal-agent failures tend to hide there.
