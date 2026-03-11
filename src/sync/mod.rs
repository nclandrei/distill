// Skill sync — reads skills from ~/.distill/skills/ and syncs to all agents.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, PartialEq)]
pub struct SkillSource {
    pub skill: Skill,
    pub source_path: PathBuf,
}

fn read_skill_source_from_entry(path: &Path) -> Result<Option<SkillSource>> {
    if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md") {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let content = std::fs::read_to_string(path)?;
        return Ok(Some(SkillSource {
            skill: Skill { name, content },
            source_path: path.to_path_buf(),
        }));
    }

    if path.is_dir() {
        let skill_file = path.join("SKILL.md");
        if skill_file.is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string();
            let content = std::fs::read_to_string(&skill_file)?;
            return Ok(Some(SkillSource {
                skill: Skill { name, content },
                source_path: skill_file,
            }));
        }
    }

    Ok(None)
}

/// Read all skills from a set of directories, supporting both Distill's flat
/// `*.md` layout and shared agent skill directories (`<name>/SKILL.md`).
///
/// When the same skill name appears in multiple roots, the first root wins.
pub fn load_skill_sources_from_dirs(skills_dirs: &[PathBuf]) -> Result<Vec<SkillSource>> {
    let mut skills_by_name = BTreeMap::new();

    for skills_dir in skills_dirs {
        if !skills_dir.exists() {
            continue;
        }

        let mut entries = std::fs::read_dir(skills_dir)?.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_string());

        for entry in entries {
            if let Some(skill_source) = read_skill_source_from_entry(&entry.path())? {
                skills_by_name
                    .entry(skill_source.skill.name.clone())
                    .or_insert(skill_source);
            }
        }
    }

    Ok(skills_by_name.into_values().collect())
}

pub fn load_skills_from_dirs(skills_dirs: &[PathBuf]) -> Result<Vec<Skill>> {
    Ok(load_skill_sources_from_dirs(skills_dirs)?
        .into_iter()
        .map(|source| source.skill)
        .collect())
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

/// Convenience function: load skills from multiple roots then sync them to `agents`.
pub fn run_sync_from_dirs(
    skills_dirs: &[PathBuf],
    agents: &[Box<dyn Agent>],
) -> Result<SyncReport> {
    let skills = load_skills_from_dirs(skills_dirs)?;
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
    // load_skills_from_dirs
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

        let skills = load_skills_from_dirs(&[skills_dir]).unwrap();

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

        let skills = load_skills_from_dirs(&[skills_dir]).unwrap();

        // Only "real.md" should be picked up (.MD is a different extension on
        // case-sensitive file systems, and .txt / .json are always ignored).
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "real");
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = load_skills_from_dirs(&[dir.path().to_path_buf()]).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_nonexistent_dir() {
        let skills =
            load_skills_from_dirs(&[Path::new("/nonexistent/path/skills").to_path_buf()]).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_supports_skill_directories() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().to_path_buf();

        let review_dir = skills_dir.join("review");
        std::fs::create_dir_all(&review_dir).unwrap();
        std::fs::write(
            review_dir.join("SKILL.md"),
            "# Review\nLook for regressions.",
        )
        .unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "review");
        assert_eq!(skills[0].content, "# Review\nLook for regressions.");
    }

    #[test]
    fn test_load_skill_sources_tracks_origin_paths() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().to_path_buf();
        let skill_file = skills_dir.join("debugging.md");
        std::fs::write(&skill_file, "# Debugging\nTrace the failure.").unwrap();

        let sources = load_skill_sources_from_dirs(&[skills_dir]).unwrap();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].skill.name, "debugging");
        assert_eq!(sources[0].source_path, skill_file);
    }

    #[cfg(unix)]
    #[test]
    fn test_load_skills_follows_symlinked_skill_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let source_dir = dir.path().join("source").join("jj");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# Jj\nLand changes.").unwrap();
        symlink(&source_dir, skills_dir.join("jj")).unwrap();

        let skills = load_skills_from_dirs(&[skills_dir]).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "jj");
        assert_eq!(skills[0].content, "# Jj\nLand changes.");
    }

    #[test]
    fn test_load_skills_from_dirs_dedupes_by_name_preferring_first_root() {
        let dir = tempfile::tempdir().unwrap();
        let primary_dir = dir.path().join("primary");
        let shared_dir = dir.path().join("shared");
        std::fs::create_dir_all(&primary_dir).unwrap();
        std::fs::create_dir_all(shared_dir.join("debugging")).unwrap();

        std::fs::write(
            primary_dir.join("debugging.md"),
            "# Debugging\nPrimary copy.",
        )
        .unwrap();
        std::fs::write(
            shared_dir.join("debugging").join("SKILL.md"),
            "# Debugging\nShared copy.",
        )
        .unwrap();
        std::fs::create_dir_all(shared_dir.join("review")).unwrap();
        std::fs::write(
            shared_dir.join("review").join("SKILL.md"),
            "# Review\nShared only.",
        )
        .unwrap();

        let skills = load_skills_from_dirs(&[primary_dir, shared_dir]).unwrap();

        assert_eq!(skills.len(), 2);
        let debugging = skills
            .iter()
            .find(|skill| skill.name == "debugging")
            .unwrap();
        let review = skills.iter().find(|skill| skill.name == "review").unwrap();
        assert_eq!(debugging.content, "# Debugging\nPrimary copy.");
        assert_eq!(review.content, "# Review\nShared only.");
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

        let report = run_sync_from_dirs(&[skills_dir], &agents).unwrap();

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
    fn test_run_sync_from_dirs_includes_shared_directory_skills() {
        let dir = tempfile::tempdir().unwrap();
        let local_dir = dir.path().join("local");
        let shared_dir = dir.path().join("shared");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::create_dir_all(shared_dir.join("jj")).unwrap();

        std::fs::write(
            shared_dir.join("jj").join("SKILL.md"),
            "# Jj\nLand carefully.",
        )
        .unwrap();

        let agents: Vec<Box<dyn Agent>> = vec![
            Box::new(ClaudeAdapter::with_home(home.clone())),
            Box::new(CodexAdapter::with_home(home.clone())),
        ];

        let report = run_sync_from_dirs(&[local_dir, shared_dir], &agents).unwrap();

        assert_eq!(report.errors, Vec::<String>::new());
        let claude = std::fs::read_to_string(home.join(".claude/skills/jj/SKILL.md")).unwrap();
        let codex = std::fs::read_to_string(home.join(".agents/skills/jj/SKILL.md")).unwrap();
        assert_eq!(claude, "# Jj\nLand carefully.");
        assert_eq!(codex, "# Jj\nLand carefully.");
    }

    #[test]
    fn test_run_sync_nonexistent_skills_dir() {
        let home_tmp = tempfile::tempdir().unwrap();
        let home = home_tmp.path().to_path_buf();

        let agents: Vec<Box<dyn Agent>> = vec![Box::new(ClaudeAdapter::with_home(home.clone()))];

        let report =
            run_sync_from_dirs(&[Path::new("/nonexistent/skills").to_path_buf()], &agents).unwrap();

        assert_eq!(report.synced, 0);
        assert!(report.errors.is_empty());
    }
}
