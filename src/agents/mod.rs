use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    OpenCode,
}

impl AgentKind {
    /// Return all variants of AgentKind
    pub fn all() -> Vec<AgentKind> {
        vec![AgentKind::Claude, AgentKind::Codex, AgentKind::OpenCode]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }

    /// Return the expected CLI name for this agent.
    pub fn command_name(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentKind::Claude => write!(f, "claude"),
            AgentKind::Codex => write!(f, "codex"),
            AgentKind::OpenCode => write!(f, "opencode"),
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

    /// Check if this agent's CLI is installed and available on PATH.
    fn is_installed(&self) -> bool {
        find_agent_command(self.kind()).is_some()
    }
}

/// Factory function: create the correct adapter for a given AgentKind.
pub fn from_kind(kind: AgentKind, home: PathBuf) -> Box<dyn Agent> {
    match kind {
        AgentKind::Claude => Box::new(ClaudeAdapter { home }),
        AgentKind::Codex => Box::new(CodexAdapter { home }),
        AgentKind::OpenCode => Box::new(OpenCodeAdapter { home }),
    }
}

pub fn from_name(name: &str, home: PathBuf) -> Option<Box<dyn Agent>> {
    AgentKind::from_name(name).map(|kind| from_kind(kind, home))
}

pub fn read_session_source(session: &Session) -> Result<String> {
    match session.agent {
        AgentKind::Claude | AgentKind::Codex => std::fs::read_to_string(&session.path)
            .with_context(|| format!("Failed to read session {}", session.path.display())),
        AgentKind::OpenCode => OpenCodeAdapter::new().export_session(&session.id),
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

fn command_on_path(command: &str, path_env: Option<&OsStr>) -> Option<PathBuf> {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }

    let path_env = path_env?;
    for entry in std::env::split_paths(path_env) {
        let path = entry.join(command);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn find_agent_command_in_path(kind: AgentKind, path_env: Option<&OsStr>) -> Option<PathBuf> {
    command_on_path(kind.command_name(), path_env)
}

pub fn find_agent_command(kind: AgentKind) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH");
    find_agent_command_in_path(kind, path_env.as_deref())
}

fn extract_leading_frontmatter(input: &str) -> Option<&str> {
    if !input.starts_with("---\n") {
        return None;
    }

    let after_first = &input[4..];
    if let Some(end) = after_first.find("\n---\n") {
        return Some(&input[..(4 + end + 5)]);
    }
    if let Some(end) = after_first.find("\n---") {
        return Some(&input[..(4 + end + 4)]);
    }

    None
}

fn infer_skill_description(content: &str) -> Option<String> {
    let mut in_when_to_use = false;
    let mut paragraph = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if in_when_to_use {
                break;
            }
            in_when_to_use = heading.eq_ignore_ascii_case("when to use");
            continue;
        }

        if !in_when_to_use {
            continue;
        }

        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }

        paragraph.push(trimmed);
    }

    if !paragraph.is_empty() {
        return Some(paragraph.join(" "));
    }

    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
}

fn ensure_structured_skill_content(skill: &Skill) -> Result<String> {
    if extract_leading_frontmatter(&skill.content).is_some() {
        return Ok(skill.content.clone());
    }

    #[derive(Serialize)]
    struct SkillFrontmatter<'a> {
        name: &'a str,
        description: &'a str,
    }

    let description = infer_skill_description(&skill.content).unwrap_or_else(|| {
        format!(
            "Instructions and workflow for {}.",
            skill.name.replace('-', " ")
        )
    });
    let frontmatter = SkillFrontmatter {
        name: &skill.name,
        description: &description,
    };
    let yaml = serde_yaml::to_string(&frontmatter)?;
    let body = skill.content.trim_start_matches('\n');
    Ok(format!("---\n{yaml}---\n\n{body}"))
}

/// Convert a `std::time::SystemTime` to `DateTime<Utc>`.
fn system_time_to_utc(st: std::time::SystemTime) -> DateTime<Utc> {
    let duration = st.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    DateTime::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .unwrap_or_else(Utc::now)
}

/// Read metadata for a single `.jsonl` session file without parsing its body.
///
/// The session `timestamp` is set to the file's modification time.
/// The session `id` is the full file path so it remains unique across
/// different projects that happen to use the same basename.
/// Distill keeps the file path so the scanner can later render clipped
/// excerpts from the log.
fn read_jsonl_session(path: &std::path::Path, kind: AgentKind) -> Result<Session> {
    let id = path.to_string_lossy().to_string();
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

fn parse_rfc3339_timestamp(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(raw) = value.as_str() {
        return DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|timestamp| timestamp.to_utc());
    }

    if let Some(raw) = value.as_i64() {
        return DateTime::from_timestamp(raw, 0);
    }

    None
}

fn sanitize_virtual_filename(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.to_string()
    }
}

fn opencode_virtual_session_path(home: &Path, session_id: &str) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("opencode")
        .join("sessions")
        .join(format!("{}.json", sanitize_virtual_filename(session_id)))
}

fn parse_opencode_session_list(raw: &str, home: &Path) -> Result<Vec<Session>> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("Failed to parse OpenCode session list JSON")?;
    let items = value
        .as_array()
        .or_else(|| value.get("sessions").and_then(|value| value.as_array()))
        .or_else(|| value.get("items").and_then(|value| value.as_array()))
        .or_else(|| value.get("data").and_then(|value| value.as_array()))
        .context("OpenCode session list JSON did not contain a session array")?;

    let mut sessions = Vec::new();
    for item in items {
        let Some(id) = item
            .get("id")
            .and_then(|value| value.as_str())
            .or_else(|| item.get("sessionId").and_then(|value| value.as_str()))
        else {
            continue;
        };

        let timestamp = [
            "updatedAt",
            "updated_at",
            "lastMessageAt",
            "last_message_at",
            "createdAt",
            "created_at",
            "timestamp",
        ]
        .iter()
        .find_map(|field| item.get(*field).and_then(parse_rfc3339_timestamp))
        .unwrap_or_else(Utc::now);

        sessions.push(Session {
            id: id.to_string(),
            agent: AgentKind::OpenCode,
            path: opencode_virtual_session_path(home, id),
            timestamp,
            content: String::new(),
        });
    }

    Ok(sessions)
}

fn format_command_failure(command: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(":\n{}", stderr.trim()),
        (true, false) => format!(":\n{}", stdout.trim()),
        (false, false) => format!(":\n{}\n{}", stderr.trim(), stdout.trim()),
    };
    format!(
        "Agent command `{command}` failed with status {}{}",
        output.status, details
    )
}

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

/// Claude Code adapter — reads from ~/.claude/, writes skills to
/// ~/.claude/skills/<skill-name>/SKILL.md
pub struct ClaudeAdapter {
    pub home: PathBuf,
}

impl ClaudeAdapter {
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
        let projects_dir = self.config_dir().join("projects");
        let files = collect_jsonl_files(&projects_dir);
        let mut sessions = Vec::new();
        for path in files {
            match read_jsonl_session(&path, AgentKind::Claude) {
                Ok(session) if session.timestamp >= since => sessions.push(session),
                Ok(_) => {}  // filtered out by `since`
                Err(_) => {} // skip unreadable files silently
            }
        }
        Ok(sessions)
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self
            .config_dir()
            .join("skills")
            .join(&skill.name)
            .join("SKILL.md");
        let content = ensure_structured_skill_content(skill)?;
        // Ensure the parent directory exists
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        if existing == content {
            // Skill already synced — skip unnecessary rewrite
            return Ok(());
        }
        std::fs::write(&target, content)?;
        Ok(())
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".claude")
    }
}

// ---------------------------------------------------------------------------
// CodexAdapter
// ---------------------------------------------------------------------------

/// Codex adapter — reads from ~/.codex/, writes skills to
/// ~/.codex/skills/<skill-name>/SKILL.md and mirrors them to
/// ~/.agents/skills/<skill-name>/SKILL.md for shared compatibility.
pub struct CodexAdapter {
    pub home: PathBuf,
}

impl CodexAdapter {
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
        let sessions_dir = self.config_dir().join("sessions");
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
        let content = ensure_structured_skill_content(skill)?;
        let targets = [
            self.config_dir()
                .join("skills")
                .join(&skill.name)
                .join("SKILL.md"),
            self.home
                .join(".agents")
                .join("skills")
                .join(&skill.name)
                .join("SKILL.md"),
        ];

        for target in targets {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let existing = std::fs::read_to_string(&target).unwrap_or_default();
            if existing != content {
                std::fs::write(&target, &content)?;
            }
        }
        Ok(())
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".codex")
    }
}

// ---------------------------------------------------------------------------
// OpenCodeAdapter
// ---------------------------------------------------------------------------

/// OpenCode adapter — discovers sessions through the official CLI and writes
/// skills to ~/.config/opencode/skills/<skill-name>/SKILL.md.
pub struct OpenCodeAdapter {
    pub home: PathBuf,
}

impl OpenCodeAdapter {
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

    fn run_json_command(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(self.kind().command_name())
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .output()
            .with_context(|| {
                format!(
                    "Failed to execute OpenCode command: {} {}",
                    self.kind().command_name(),
                    args.join(" ")
                )
            })?;

        if !output.status.success() {
            bail!(
                "{}",
                format_command_failure(self.kind().command_name(), &output)
            );
        }

        String::from_utf8(output.stdout).context("OpenCode output is not valid UTF-8")
    }

    fn session_list(&self) -> Result<Vec<Session>> {
        parse_opencode_session_list(
            &self.run_json_command(&["session", "list", "--format", "json"])?,
            &self.home,
        )
    }

    fn export_session(&self, session_id: &str) -> Result<String> {
        self.run_json_command(&["export", session_id, "--format", "json"])
    }
}

impl Agent for OpenCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn read_sessions(&self, since: DateTime<Utc>) -> Result<Vec<Session>> {
        Ok(self
            .session_list()?
            .into_iter()
            .filter(|session| session.timestamp >= since)
            .collect())
    }

    fn write_skill(&self, skill: &Skill) -> Result<()> {
        let target = self
            .config_dir()
            .join("skills")
            .join(&skill.name)
            .join("SKILL.md");
        let content = ensure_structured_skill_content(skill)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::read_to_string(&target).unwrap_or_default();
        if existing != content {
            std::fs::write(&target, content)?;
        }
        Ok(())
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".config").join("opencode")
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
        assert_eq!(AgentKind::OpenCode.to_string(), "opencode");
    }

    #[test]
    fn test_agent_kind_command_name() {
        assert_eq!(AgentKind::Claude.command_name(), "claude");
        assert_eq!(AgentKind::Codex.command_name(), "codex");
        assert_eq!(AgentKind::OpenCode.command_name(), "opencode");
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
        let first =
            std::fs::read_to_string(home.join(".claude/skills/test-skill/SKILL.md")).unwrap();

        adapter.write_skill(&skill).unwrap();
        let second =
            std::fs::read_to_string(home.join(".claude/skills/test-skill/SKILL.md")).unwrap();

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
        let first_codex =
            std::fs::read_to_string(home.join(".codex/skills/test-skill/SKILL.md")).unwrap();
        let first_shared =
            std::fs::read_to_string(home.join(".agents/skills/test-skill/SKILL.md")).unwrap();

        adapter.write_skill(&skill).unwrap();
        let second_codex =
            std::fs::read_to_string(home.join(".codex/skills/test-skill/SKILL.md")).unwrap();
        let second_shared =
            std::fs::read_to_string(home.join(".agents/skills/test-skill/SKILL.md")).unwrap();

        assert_eq!(first_codex, second_codex);
        assert_eq!(first_shared, second_shared);
        assert_eq!(first_codex, first_shared);
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
    fn test_agent_kind_all_returns_supported_variants() {
        let all = AgentKind::all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&AgentKind::Claude));
        assert!(all.contains(&AgentKind::Codex));
        assert!(all.contains(&AgentKind::OpenCode));
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

    #[test]
    fn test_from_kind_returns_opencode_adapter() {
        let home = PathBuf::from("/tmp/fakehome");
        let agent = from_kind(AgentKind::OpenCode, home.clone());
        assert_eq!(agent.kind(), AgentKind::OpenCode);
        assert_eq!(agent.config_dir(), home.join(".config/opencode"));
    }

    #[test]
    fn test_find_agent_command_in_path_returns_none_when_missing() {
        let path = std::ffi::OsString::from("/definitely/not/present");
        assert!(find_agent_command_in_path(AgentKind::Claude, Some(&path)).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_find_agent_command_in_path_finds_binary() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let found =
            find_agent_command_in_path(AgentKind::Claude, Some(dir.path().as_os_str())).unwrap();
        assert_eq!(found, claude);
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

    #[cfg(unix)]
    #[test]
    fn test_opencode_read_sessions_missing_list_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = OpenCodeAdapter::with_home(home);
        let err = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to execute OpenCode command")
        );
    }

    // --- read_sessions: returns sessions from .jsonl files ---

    #[test]
    fn test_claude_read_sessions_returns_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let projects = home.join(".claude").join("projects").join("my-project");

        write_jsonl(&projects.join("session-alpha.jsonl"), r#"{"role":"user"}"#);
        write_jsonl(
            &projects.join("session-beta.jsonl"),
            r#"{"role":"assistant"}"#,
        );

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.ends_with("session-alpha.jsonl")));
        assert!(ids.iter().any(|id| id.ends_with("session-beta.jsonl")));
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
        assert!(sessions[0].id.ends_with("sess-1.jsonl"));
        assert_eq!(sessions[0].agent, AgentKind::Codex);
    }

    #[cfg(unix)]
    #[test]
    fn test_opencode_read_sessions_uses_cli_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let bin_dir = home.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let opencode = bin_dir.join("opencode");
        std::fs::write(
            &opencode,
            "#!/bin/sh\nif [ \"$1\" = \"session\" ] && [ \"$2\" = \"list\" ]; then\n  printf '%s' '[{\"id\":\"sess-1\",\"updatedAt\":\"2026-03-10T12:00:00Z\"}]'\n  exit 0\nfi\nexit 41\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let original_path = std::env::var_os("PATH");
        unsafe {
            std::env::set_var(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    original_path
                        .as_deref()
                        .map(|value| value.to_string_lossy().to_string())
                        .unwrap_or_default()
                ),
            );
        }

        let adapter = OpenCodeAdapter::with_home(home.clone());
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-1");
        assert_eq!(sessions[0].agent, AgentKind::OpenCode);
        assert_eq!(
            sessions[0].path,
            home.join(".local/share/opencode/sessions/sess-1.json")
        );

        unsafe {
            if let Some(path) = original_path {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
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
        assert!(sessions[0].id.ends_with("real.jsonl"));
    }

    // --- read_sessions: nested project sub-directories are walked ---

    #[test]
    fn test_claude_read_sessions_walks_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();

        write_jsonl(&home.join(".claude/projects/proj-a/s1.jsonl"), r#"{"a":1}"#);
        write_jsonl(
            &home.join(".claude/projects/proj-b/sub/s2.jsonl"),
            r#"{"b":2}"#,
        );

        let adapter = ClaudeAdapter::with_home(home);
        let sessions = adapter.read_sessions(DateTime::UNIX_EPOCH).unwrap();

        assert_eq!(sessions.len(), 2);
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.ends_with("s1.jsonl")));
        assert!(ids.iter().any(|id| id.ends_with("s2.jsonl")));
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
        assert!(sessions[0].id.ends_with("fresh.jsonl"));
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
        let written =
            std::fs::read_to_string(home.join(".claude/skills/auto-dir/SKILL.md")).unwrap();
        assert!(
            written.starts_with("---\nname: auto-dir\ndescription: created automatically\n---\n\n")
        );
        assert!(written.ends_with("created automatically"));
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
        let codex_written =
            std::fs::read_to_string(home.join(".codex/skills/auto-dir/SKILL.md")).unwrap();
        let shared_written =
            std::fs::read_to_string(home.join(".agents/skills/auto-dir/SKILL.md")).unwrap();
        assert!(
            codex_written
                .starts_with("---\nname: auto-dir\ndescription: created automatically\n---\n\n")
        );
        assert!(
            shared_written
                .starts_with("---\nname: auto-dir\ndescription: created automatically\n---\n\n")
        );
        assert!(codex_written.ends_with("created automatically"));
        assert!(shared_written.ends_with("created automatically"));
    }

    #[test]
    fn test_opencode_write_skill_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = OpenCodeAdapter::with_home(home.clone());
        let skill = Skill {
            name: "auto-dir".into(),
            content: "created automatically".into(),
        };
        adapter.write_skill(&skill).unwrap();
        let written =
            std::fs::read_to_string(home.join(".config/opencode/skills/auto-dir/SKILL.md"))
                .unwrap();
        assert!(
            written.starts_with("---\nname: auto-dir\ndescription: created automatically\n---\n\n")
        );
        assert!(written.ends_with("created automatically"));
    }

    // --- write_skill: multiple distinct skills are all written ---

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

        let written_a =
            std::fs::read_to_string(home.join(".claude/skills/skill-a/SKILL.md")).unwrap();
        let written_b =
            std::fs::read_to_string(home.join(".claude/skills/skill-b/SKILL.md")).unwrap();
        assert!(written_a.starts_with("---\nname: skill-a\ndescription: Content A\n---\n"));
        assert!(written_b.starts_with("---\nname: skill-b\ndescription: Content B\n---\n"));
        assert!(written_a.ends_with("Content A"));
        assert!(written_b.ends_with("Content B"));
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

        let codex_written_a =
            std::fs::read_to_string(home.join(".codex/skills/alpha/SKILL.md")).unwrap();
        let codex_written_b =
            std::fs::read_to_string(home.join(".codex/skills/beta/SKILL.md")).unwrap();
        let shared_written_a =
            std::fs::read_to_string(home.join(".agents/skills/alpha/SKILL.md")).unwrap();
        let shared_written_b =
            std::fs::read_to_string(home.join(".agents/skills/beta/SKILL.md")).unwrap();
        assert!(codex_written_a.starts_with("---\nname: alpha\ndescription: Alpha content\n---\n"));
        assert!(codex_written_b.starts_with("---\nname: beta\ndescription: Beta content\n---\n"));
        assert_eq!(codex_written_a, shared_written_a);
        assert_eq!(codex_written_b, shared_written_b);
    }

    #[test]
    fn test_claude_write_skill_preserves_existing_structured_content() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = ClaudeAdapter::with_home(home.clone());
        let structured = "---\nname: review\ndescription: Inspect changes.\n---\n\n# Review\n\nLook for regressions.\n";
        let skill = Skill {
            name: "review".into(),
            content: structured.into(),
        };

        adapter.write_skill(&skill).unwrap();

        let written = std::fs::read_to_string(home.join(".claude/skills/review/SKILL.md")).unwrap();
        assert_eq!(written, structured);
    }

    #[test]
    fn test_codex_write_skill_uses_when_to_use_text_for_generated_description() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let adapter = CodexAdapter::with_home(home.clone());
        let skill = Skill {
            name: "sync-agents-md".into(),
            content: "# Sync AGENTS.md\n## When to use\nUse when a repository keeps an `AGENTS.md` guide and you need to refresh it from repo evidence.\n\n## Steps\n1. Inspect files.\n".into(),
        };

        adapter.write_skill(&skill).unwrap();

        let written =
            std::fs::read_to_string(home.join(".codex/skills/sync-agents-md/SKILL.md")).unwrap();
        assert!(written.starts_with("---\nname: sync-agents-md\ndescription: Use when a repository keeps an `AGENTS.md` guide and you need to refresh it from repo evidence.\n---\n\n"));
        assert!(written.contains("# Sync AGENTS.md"));
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

    #[test]
    fn test_parse_opencode_session_list_accepts_nested_sessions_array() {
        let home = PathBuf::from("/tmp/home");
        let sessions = parse_opencode_session_list(
            r#"{"sessions":[{"id":"sess-2","createdAt":"2026-03-10T12:00:00Z"}]}"#,
            &home,
        )
        .unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-2");
        assert_eq!(
            sessions[0].path,
            PathBuf::from("/tmp/home/.local/share/opencode/sessions/sess-2.json")
        );
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
