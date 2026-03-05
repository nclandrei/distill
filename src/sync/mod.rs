// Skill sync — reads skills from ~/.distill/skills/ and syncs to all agents.

use anyhow::Result;
use std::path::Path;

use crate::agents::{Agent, Skill};

/// Report returned by a sync run.
pub struct SyncReport {
    /// Number of (skill, agent) pairs where `write_skill` was called successfully.
    pub synced: usize,
    /// Number of (skill, agent) pairs that were already present (idempotent skip).
    /// Currently always 0 — tracked at the agent level transparently.
    pub skipped: usize,
    /// Non-fatal errors encountered during the sync.
    pub errors: Vec<String>,
}

/// Read all `.md` files from `skills_dir`, returning a `Skill` for each.
///
/// - `name`    = file stem (e.g. `"git-workflow"` for `git-workflow.md`)
/// - `content` = raw file contents
///
/// Returns an empty `Vec` (not an error) if the directory does not exist.
pub fn load_skills(skills_dir: &Path) -> Result<Vec<Skill>> {
    if !skills_dir.exists() {
        return Ok(vec![]);
    }

    let mut skills = Vec::new();

    for entry in std::fs::read_dir(skills_dir)?.flatten() {
        let path = entry.path();

        // Only process regular .md files (skip directories, etc.)
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(&path)?;

        skills.push(Skill { name, content });
    }

    Ok(skills)
}

/// For each skill in `skills`, call `write_skill` on every agent in `agents`.
///
/// Errors from individual `write_skill` calls are collected as non-fatal strings
/// in `SyncReport::errors` rather than aborting the whole sync.
pub fn sync_skills(skills: &[Skill], agents: &[Box<dyn Agent>]) -> Result<SyncReport> {
    let mut synced = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for skill in skills {
        for agent in agents {
            match agent.write_skill(skill) {
                Ok(()) => synced += 1,
                Err(e) => errors.push(format!(
                    "agent={} skill={}: {}",
                    agent.kind(),
                    skill.name,
                    e
                )),
            }
        }
    }

    Ok(SyncReport {
        synced,
        skipped: 0,
        errors,
    })
}

/// Convenience function: load skills from `skills_dir` then sync them to `agents`.
pub fn run_sync(skills_dir: &Path, agents: &[Box<dyn Agent>]) -> Result<SyncReport> {
    let skills = load_skills(skills_dir)?;
    sync_skills(&skills, agents)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{ClaudeAdapter, CodexAdapter};
    use std::path::PathBuf;

    // ------------------------------------------------------------------
    // load_skills
    // ------------------------------------------------------------------

    #[test]
    fn test_load_skills_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().to_path_buf();

        std::fs::write(
            skills_dir.join("git-workflow.md"),
            "# Git Workflow\nAlways rebase.",
        )
        .unwrap();
        std::fs::write(skills_dir.join("code-review.md"), "# Code Review\nBe kind.").unwrap();

        let skills = load_skills(&skills_dir).unwrap();

        assert_eq!(skills.len(), 2);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"git-workflow"));
        assert!(names.contains(&"code-review"));

        let git = skills.iter().find(|s| s.name == "git-workflow").unwrap();
        assert_eq!(git.content, "# Git Workflow\nAlways rebase.");

        let review = skills.iter().find(|s| s.name == "code-review").unwrap();
        assert_eq!(review.content, "# Code Review\nBe kind.");
    }

    #[test]
    fn test_load_skills_ignores_non_md() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().to_path_buf();

        std::fs::write(skills_dir.join("real.md"), "# Real Skill").unwrap();
        std::fs::write(skills_dir.join("notes.txt"), "some notes").unwrap();
        std::fs::write(skills_dir.join("data.json"), "{}").unwrap();
        std::fs::write(skills_dir.join("readme.MD"), "uppercase ext").unwrap();

        let skills = load_skills(&skills_dir).unwrap();

        // Only "real.md" should be picked up (.MD is a different extension on
        // case-sensitive file systems, and .txt / .json are always ignored).
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "real");
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = load_skills(dir.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_nonexistent_dir() {
        let skills = load_skills(Path::new("/nonexistent/path/skills")).unwrap();
        assert!(skills.is_empty());
    }

    // ------------------------------------------------------------------
    // sync_skills
    // ------------------------------------------------------------------

    #[test]
    fn test_sync_skills_writes_to_all_agents() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let agents: Vec<Box<dyn Agent>> = vec![
            Box::new(ClaudeAdapter::with_home(home.clone())),
            Box::new(CodexAdapter::with_home(home.clone())),
        ];

        let skills = vec![
            Skill {
                name: "testing".into(),
                content: "# Testing\nWrite tests first.".into(),
            },
            Skill {
                name: "debugging".into(),
                content: "# Debugging\nRead the error message.".into(),
            },
        ];

        let report = sync_skills(&skills, &agents).unwrap();

        // 2 skills * 2 agents = 4 successful writes
        assert_eq!(report.synced, 4);
        assert!(report.errors.is_empty());

        // Verify Claude's per-skill files
        let claude_testing =
            std::fs::read_to_string(home.join(".claude/skills/testing/SKILL.md")).unwrap();
        let claude_debugging =
            std::fs::read_to_string(home.join(".claude/skills/debugging/SKILL.md")).unwrap();
        assert_eq!(claude_testing, "# Testing\nWrite tests first.");
        assert_eq!(claude_debugging, "# Debugging\nRead the error message.");

        // Verify Codex's per-skill files
        let codex_testing =
            std::fs::read_to_string(home.join(".agents/skills/testing/SKILL.md")).unwrap();
        let codex_debugging =
            std::fs::read_to_string(home.join(".agents/skills/debugging/SKILL.md")).unwrap();
        assert_eq!(codex_testing, "# Testing\nWrite tests first.");
        assert_eq!(codex_debugging, "# Debugging\nRead the error message.");
    }

    #[test]
    fn test_sync_skills_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let skills = vec![Skill {
            name: "my-skill".into(),
            content: "# My Skill\nDo the right thing.".into(),
        }];

        let agents_first: Vec<Box<dyn Agent>> =
            vec![Box::new(ClaudeAdapter::with_home(home.clone()))];
        sync_skills(&skills, &agents_first).unwrap();

        let after_first =
            std::fs::read_to_string(home.join(".claude/skills/my-skill/SKILL.md")).unwrap();

        let agents_second: Vec<Box<dyn Agent>> =
            vec![Box::new(ClaudeAdapter::with_home(home.clone()))];
        sync_skills(&skills, &agents_second).unwrap();

        let after_second =
            std::fs::read_to_string(home.join(".claude/skills/my-skill/SKILL.md")).unwrap();

        // Content should remain stable on repeated sync.
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn test_sync_skills_empty_skills_list() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        let agents: Vec<Box<dyn Agent>> = vec![Box::new(ClaudeAdapter::with_home(home.clone()))];

        let report = sync_skills(&[], &agents).unwrap();

        assert_eq!(report.synced, 0);
        assert_eq!(report.skipped, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn test_sync_skills_empty_agents_list() {
        let skills = vec![Skill {
            name: "orphan".into(),
            content: "No agents to write to.".into(),
        }];

        let report = sync_skills(&skills, &[]).unwrap();

        assert_eq!(report.synced, 0);
        assert_eq!(report.skipped, 0);
        assert!(report.errors.is_empty());
    }

    // ------------------------------------------------------------------
    // run_sync (end-to-end)
    // ------------------------------------------------------------------

    #[test]
    fn test_run_sync_end_to_end() {
        let skills_dir_tmp = tempfile::tempdir().unwrap();
        let skills_dir = skills_dir_tmp.path().to_path_buf();

        std::fs::write(skills_dir.join("tdd.md"), "# TDD\nRed, green, refactor.").unwrap();
        std::fs::write(skills_dir.join("docs.md"), "# Docs\nWrite docs as you go.").unwrap();

        let home_tmp = tempfile::tempdir().unwrap();
        let home: PathBuf = home_tmp.path().to_path_buf();

        let agents: Vec<Box<dyn Agent>> = vec![
            Box::new(ClaudeAdapter::with_home(home.clone())),
            Box::new(CodexAdapter::with_home(home.clone())),
        ];

        let report = run_sync(&skills_dir, &agents).unwrap();

        // 2 skills * 2 agents = 4 operations
        assert_eq!(report.synced, 4);
        assert!(report.errors.is_empty());

        let claude_tdd = std::fs::read_to_string(home.join(".claude/skills/tdd/SKILL.md")).unwrap();
        let claude_docs =
            std::fs::read_to_string(home.join(".claude/skills/docs/SKILL.md")).unwrap();
        assert_eq!(claude_tdd, "# TDD\nRed, green, refactor.");
        assert_eq!(claude_docs, "# Docs\nWrite docs as you go.");

        let codex_tdd = std::fs::read_to_string(home.join(".agents/skills/tdd/SKILL.md")).unwrap();
        let codex_docs =
            std::fs::read_to_string(home.join(".agents/skills/docs/SKILL.md")).unwrap();
        assert_eq!(codex_tdd, "# TDD\nRed, green, refactor.");
        assert_eq!(codex_docs, "# Docs\nWrite docs as you go.");
    }

    #[test]
    fn test_run_sync_nonexistent_skills_dir() {
        let home_tmp = tempfile::tempdir().unwrap();
        let home = home_tmp.path().to_path_buf();

        let agents: Vec<Box<dyn Agent>> = vec![Box::new(ClaudeAdapter::with_home(home.clone()))];

        let report = run_sync(Path::new("/nonexistent/skills"), &agents).unwrap();

        assert_eq!(report.synced, 0);
        assert!(report.errors.is_empty());
    }
}
