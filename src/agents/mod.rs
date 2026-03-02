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

impl AgentKind {
    /// Return all variants of AgentKind
    pub fn all() -> Vec<AgentKind> {
        vec![AgentKind::Claude, AgentKind::Codex]
    }
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

/// Factory function: create the correct adapter for a given AgentKind.
pub fn from_kind(kind: AgentKind, home: PathBuf) -> Box<dyn Agent> {
    match kind {
        AgentKind::Claude => Box::new(ClaudeAdapter { home }),
        AgentKind::Codex => Box::new(CodexAdapter { home }),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Recursively collect all `.jsonl` files under `root`.
/// Returns an empty vec (no error) when `root` does not exist.
fn collect_jsonl_files(root: &std::path::Path) -> Vec<PathBuf> {
    if !root.exists() {
        return vec![];
    }
    let mut results = Vec::new();
    collect_jsonl_recursive(root, &mut results);
    results
}

fn collect_jsonl_recursive(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_recursive(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Convert a `std::time::SystemTime` to `DateTime<Utc>`.
fn system_time_to_utc(st: std::time::SystemTime) -> DateTime<Utc> {
    let duration = st
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    DateTime::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .unwrap_or_else(Utc::now)
}

/// Read metadata for a single `.jsonl` session file (without reading its content).
///
/// The session `timestamp` is set to the file's modification time.
/// The session `id` is the file stem (filename without extension).
/// The agent is given the file *path* so it can read the content itself.
fn read_jsonl_session(path: &std::path::Path, kind: AgentKind) -> Result<Session> {
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(system_time_to_utc)
        .unwrap_or_else(|_| Utc::now());
    Ok(Session {
        id,
        agent: kind,
        path: path.to_path_buf(),
        timestamp: mtime,
        content: String::new(),
    })
}

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

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

    fn read_sessions(&self, since: DateTime<Utc>) -> Result<Vec<Session>> {
        let projects_dir = self.home.join(".claude").join("projects");
        let files = collect_jsonl_files(&projects_dir);
        let mut sessions = Vec::new();
        for path in files {
            match read_jsonl_session(&path, AgentKind::Claude) {
                Ok(session) if session.timestamp >= since => sessions.push(session),
                Ok(_) => {} // filtered out by `since`
                Err(_) => {} // skip unreadable files silently
            }
        }
        Ok(sessions)
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self.home.join(".claude").join("CLAUDE.md");
        // Ensure the parent directory exists
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
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

// ---------------------------------------------------------------------------
// CodexAdapter
// ---------------------------------------------------------------------------

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

    fn read_sessions(&self, since: DateTime<Utc>) -> Result<Vec<Session>> {
        let sessions_dir = self.home.join(".codex").join("sessions");
        let files = collect_jsonl_files(&sessions_dir);
        let mut sessions = Vec::new();
        for path in files {
            match read_jsonl_session(&path, AgentKind::Codex) {
                Ok(session) if session.timestamp >= since => sessions.push(session),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        Ok(sessions)
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self.home.join(".codex").join("instructions.md");
        // Ensure the parent directory exists
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Create a `.jsonl` file at `path` with the given content and, optionally,
    /// set its modification time to `mtime_offset` seconds before now.
    fn write_jsonl(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    // ------------------------------------------------------------------
    // Pre-existing tests (kept intact)
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // New tests
    // ------------------------------------------------------------------

    // --- AgentKind::all ---

    #[test]
    fn test_agent_kind_all_returns_both_variants() {
        let all = AgentKind::all();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&AgentKind::Claude));
        assert!(all.contains(&AgentKind::Codex));
    }

    // --- from_kind factory ---

    #[test]
    fn test_from_kind_returns_claude_adapter() {
        let home = PathBuf::from("/tmp/fakehome");
        let agent = from_kind(AgentKind::Claude, home.clone());
        assert_eq!(agent.kind(), AgentKind::Claude);
        assert_eq!(agent.config_dir(), home.join(".claude"));
    }

    #[test]
    fn test_from_kind_returns_codex_adapter() {
        let home = PathBuf::from("/tmp/fakehome");
        let agent = from_kind(AgentKind::Codex, home.clone());
        assert_eq!(agent.kind(), AgentKind::Codex);
        assert_eq!(agent.config_dir(), home.join(".codex"));
    }

    // --- read_sessions: directory does not exist ---

    #[test]
    fn test_claude_read_sessions_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // ~/.claude/projects/ is intentionally not created
        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_codex_read_sessions_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // ~/.codex/sessions/ is intentionally not created
        let adapter = CodexAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();
        assert!(sessions.is_empty());
    }

    // --- read_sessions: returns sessions from .jsonl files ---

    #[test]
    fn test_claude_read_sessions_returns_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects").join("my-project");

        write_jsonl(&projects.join("session-alpha.jsonl"), r#"{"role":"user"}"#);
        write_jsonl(&projects.join("session-beta.jsonl"), r#"{"role":"assistant"}"#);

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"session-alpha"));
        assert!(ids.contains(&"session-beta"));
        for s in &sessions {
            assert_eq!(s.agent, AgentKind::Claude);
        }
    }

    #[test]
    fn test_codex_read_sessions_returns_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let sessions_dir = home.join(".codex").join("sessions");

        write_jsonl(&sessions_dir.join("sess-1.jsonl"), r#"{"msg":"hello"}"#);

        let adapter = CodexAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
        assert_eq!(sessions[0].agent, AgentKind::Codex);
    }

    // --- read_sessions: non-.jsonl files are ignored ---

    #[test]
    fn test_read_sessions_ignores_non_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects");
        std::fs::create_dir_all(&projects).unwrap();

        std::fs::write(projects.join("notes.txt"), "some text").unwrap();
        std::fs::write(projects.join("data.json"), "{}").unwrap();
        write_jsonl(&projects.join("real.jsonl"), r#"{"ok":true}"#);

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "real");
    }

    // --- read_sessions: nested project sub-directories are walked ---

    #[test]
    fn test_claude_read_sessions_walks_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        write_jsonl(
            &home.join(".claude/projects/proj-a/s1.jsonl"),
            r#"{"a":1}"#,
        );
        write_jsonl(
            &home.join(".claude/projects/proj-b/sub/s2.jsonl"),
            r#"{"b":2}"#,
        );

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"s1"));
        assert!(ids.contains(&"s2"));
    }

    // --- read_sessions: `since` filter removes old files ---
    //
    // NOTE: We write a file, then set `since` to "now".  Any file whose mtime
    // is strictly before `since` must be excluded.  Because we cannot reliably
    // back-date files on all CI platforms without the `filetime` crate, we use
    // a different strategy: write the file first, record a timestamp, then set
    // `since` to a moment in the future (far enough that the file's real mtime
    // is always earlier).

    #[test]
    fn test_read_sessions_since_filter_excludes_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects");

        write_jsonl(&projects.join("old.jsonl"), r#"{"old":true}"#);

        // Set `since` to 1 hour in the future so the file is always filtered out.
        let far_future = Utc::now() + chrono::Duration::hours(1);

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(far_future).unwrap();

        assert!(
            sessions.is_empty(),
            "Expected no sessions, got: {:?}",
            sessions.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_read_sessions_since_filter_includes_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects");

        // Set `since` to 1 hour in the past so newly written files pass.
        let one_hour_ago = Utc::now() - chrono::Duration::hours(1);

        write_jsonl(&projects.join("fresh.jsonl"), r#"{"new":true}"#);

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(one_hour_ago).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "fresh");
    }

    // --- write_skill: creates parent directory if missing ---

    #[test]
    fn test_claude_write_skill_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Intentionally do NOT create ~/.claude beforehand
        let adapter = ClaudeAdapter::with_home(home.clone());
        let skill = Skill {
            name: "auto-dir".into(),
            content: "created automatically".into(),
        };
        adapter.write_skill(&skill).unwrap();
        let written = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(written.contains("<!-- distill:skill:auto-dir -->"));
    }

    #[test]
    fn test_codex_write_skill_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Intentionally do NOT create ~/.codex beforehand
        let adapter = CodexAdapter::with_home(home.clone());
        let skill = Skill {
            name: "auto-dir".into(),
            content: "created automatically".into(),
        };
        adapter.write_skill(&skill).unwrap();
        let written =
            std::fs::read_to_string(home.join(".codex/instructions.md")).unwrap();
        assert!(written.contains("<!-- distill:skill:auto-dir -->"));
    }

    // --- write_skill: multiple distinct skills are all appended ---

    #[test]
    fn test_write_multiple_distinct_skills_appends_all() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = ClaudeAdapter::with_home(home.clone());

        let skill_a = Skill {
            name: "skill-a".into(),
            content: "Content A".into(),
        };
        let skill_b = Skill {
            name: "skill-b".into(),
            content: "Content B".into(),
        };

        adapter.write_skill(&skill_a).unwrap();
        adapter.write_skill(&skill_b).unwrap();

        let written = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(written.contains("<!-- distill:skill:skill-a -->"));
        assert!(written.contains("Content A"));
        assert!(written.contains("<!-- distill:skill:skill-b -->"));
        assert!(written.contains("Content B"));
    }

    #[test]
    fn test_codex_write_multiple_distinct_skills_appends_all() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = CodexAdapter::with_home(home.clone());

        let skill_a = Skill {
            name: "alpha".into(),
            content: "Alpha content".into(),
        };
        let skill_b = Skill {
            name: "beta".into(),
            content: "Beta content".into(),
        };

        adapter.write_skill(&skill_a).unwrap();
        adapter.write_skill(&skill_b).unwrap();

        let written =
            std::fs::read_to_string(home.join(".codex/instructions.md")).unwrap();
        assert!(written.contains("<!-- distill:skill:alpha -->"));
        assert!(written.contains("Alpha content"));
        assert!(written.contains("<!-- distill:skill:beta -->"));
        assert!(written.contains("Beta content"));
    }

    // --- session content is not read (agent reads files itself) ---

    #[test]
    fn test_session_content_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects");
        let raw = "{\"role\":\"user\",\"content\":\"hello world\"}\n{\"role\":\"assistant\",\"content\":\"hi\"}";
        write_jsonl(&projects.join("chat.jsonl"), raw);

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].content.is_empty());
    }

    // --- session path matches the actual file path ---

    #[test]
    fn test_session_path_matches_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects");
        let file_path = projects.join("my-session.jsonl");
        write_jsonl(&file_path, "{}");

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].path, file_path);
    }
}
