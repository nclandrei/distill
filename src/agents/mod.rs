use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a single session from an AI agent
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub id: String,
    pub agent: AgentKind,
    pub path: PathBuf,
    pub timestamp: DateTime<Utc>,
    pub content: String,
}

/// Represents a skill (markdown file)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub content: String,
}

/// The supported agent types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentKind::Claude => write!(f, "claude"),
            AgentKind::Codex => write!(f, "codex"),
        }
    }
}

/// Trait that all agent adapters must implement
pub trait Agent {
    /// Return which kind of agent this is
    fn kind(&self) -> AgentKind;

    /// Read sessions since the given timestamp
    fn read_sessions(&self, since: DateTime<Utc>) -> Result<Vec<Session>>;

    /// Write a skill to the agent's expected location
    fn write_skill(&self, skill: &Skill) -> Result<()>;

    /// Return the base directory for this agent's config
    fn config_dir(&self) -> PathBuf;

    /// Check if this agent is installed (config dir exists)
    fn is_installed(&self) -> bool {
        self.config_dir().exists()
    }
}

/// Claude Code adapter — reads from ~/.claude/, writes skills to ~/.claude/CLAUDE.md
pub struct ClaudeAdapter {
    pub home: PathBuf,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        Self { home }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }
}

impl Agent for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn read_sessions(&self, _since: DateTime<Utc>) -> Result<Vec<Session>> {
        // TODO: implement actual session reading from ~/.claude/projects/
        Ok(vec![])
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self.home.join(".claude").join("CLAUDE.md");
        // For now, append the skill content
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        let marker = format!("<!-- distill:skill:{} -->", skill.name);
        if existing.contains(&marker) {
            // Skill already synced — skip (idempotent)
            return Ok(());
        }
        let new_content = format!("{existing}\n{marker}\n{}\n", skill.content);
        std::fs::write(&target, new_content)?;
        Ok(())
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".claude")
    }
}

/// Codex adapter — reads from ~/.codex/, writes skills to ~/.codex/instructions.md
pub struct CodexAdapter {
    pub home: PathBuf,
}

impl CodexAdapter {
    pub fn new() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        Self { home }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self { home }
    }
}

impl Agent for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn read_sessions(&self, _since: DateTime<Utc>) -> Result<Vec<Session>> {
        // TODO: implement actual session reading from ~/.codex/
        Ok(vec![])
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self.home.join(".codex").join("instructions.md");
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        let marker = format!("<!-- distill:skill:{} -->", skill.name);
        if existing.contains(&marker) {
            return Ok(());
        }
        let new_content = format!("{existing}\n{marker}\n{}\n", skill.content);
        std::fs::write(&target, new_content)?;
        Ok(())
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".codex")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_kind_display() {
        assert_eq!(AgentKind::Claude.to_string(), "claude");
        assert_eq!(AgentKind::Codex.to_string(), "codex");
    }

    #[test]
    fn test_agent_kind_serde_roundtrip() {
        let kind = AgentKind::Claude;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"claude\"");
        let parsed: AgentKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn test_claude_adapter_config_dir() {
        let adapter = ClaudeAdapter::with_home(PathBuf::from("/tmp/fakehome"));
        assert_eq!(adapter.config_dir(), PathBuf::from("/tmp/fakehome/.claude"));
    }

    #[test]
    fn test_codex_adapter_config_dir() {
        let adapter = CodexAdapter::with_home(PathBuf::from("/tmp/fakehome"));
        assert_eq!(adapter.config_dir(), PathBuf::from("/tmp/fakehome/.codex"));
    }

    #[test]
    fn test_claude_write_skill_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let adapter = ClaudeAdapter::with_home(home.clone());
        let skill = Skill {
            name: "test-skill".into(),
            content: "# Test Skill\nDo the thing.".into(),
        };

        adapter.write_skill(&skill).unwrap();
        let first = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();

        adapter.write_skill(&skill).unwrap();
        let second = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();

        // Idempotent: second write should not duplicate
        assert_eq!(first, second);
    }

    #[test]
    fn test_codex_write_skill_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let adapter = CodexAdapter::with_home(home.clone());
        let skill = Skill {
            name: "test-skill".into(),
            content: "# Test Skill\nDo the thing.".into(),
        };

        adapter.write_skill(&skill).unwrap();
        let first = std::fs::read_to_string(home.join(".codex/instructions.md")).unwrap();

        adapter.write_skill(&skill).unwrap();
        let second = std::fs::read_to_string(home.join(".codex/instructions.md")).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_session_serde_roundtrip() {
        let session = Session {
            id: "abc123".into(),
            agent: AgentKind::Claude,
            path: PathBuf::from("/home/user/.claude/sessions/abc123.jsonl"),
            timestamp: Utc::now(),
            content: "session content".into(),
        };
        let json = serde_json::to_string(&session).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.id, parsed.id);
        assert_eq!(session.agent, parsed.agent);
    }
}
