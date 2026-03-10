use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use crate::agents::AgentKind;
use crate::agents::{Agent, Session};
use crate::config::Config;
use crate::preferences::PreferenceProfile;
use crate::proposals::{
    Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalTarget, ProposalType,
};
use crate::scanner::reader::{self, LastScan};
use crate::sync::SkillSource;

const PROPOSAL_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "inspected_files": {
      "type": "array",
      "items": { "type": "string" }
    },
    "file_findings": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "session": { "type": "string" },
          "summary": { "type": "string" }
        },
        "required": ["session", "summary"]
      }
    },
    "proposals": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "type":         { "type": "string", "enum": ["new", "improve", "edit", "remove"] },
          "confidence":   { "type": "string", "enum": ["high", "medium", "low"] },
          "target_skill": { "type": ["string", "null"] },
          "evidence": {
            "type": "array",
            "items": {
              "type": "object",
              "additionalProperties": false,
              "properties": {
                "session": { "type": "string" },
                "pattern": { "type": "string" }
              },
              "required": ["session", "pattern"]
            }
          },
          "body": { "type": "string" }
        },
        "required": ["type", "confidence", "target_skill", "evidence", "body"]
      }
    }
  },
  "required": ["inspected_files", "file_findings", "proposals"]
}"#;

const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 15 * 60;
const AGENT_POLL_INTERVAL_MS: u64 = 250;
const DEFAULT_SCAN_BATCH_SIZE: usize = 200;
const MAX_AGENT_DIAGNOSTIC_CHARS: usize = 4000;

pub struct ScanConfig {
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub skill_dirs: Vec<PathBuf>,
    pub proposals_dir: PathBuf,
    pub last_scan_path: PathBuf,
    pub backlog_path: PathBuf,
    pub history_dir: PathBuf,
}

impl ScanConfig {
    pub fn from_config(config: &Config) -> Self {
        let (command, args) = agent_command_for(&config.proposal_agent);
        Self {
            agent_command: command,
            agent_args: args,
            skill_dirs: vec![Config::skills_dir(), Config::shared_skills_dir()],
            proposals_dir: Config::proposals_dir(),
            last_scan_path: Config::last_scan_path(),
            backlog_path: Config::scan_backlog_path(),
            history_dir: Config::history_dir(),
        }
    }
}

fn agent_command_for(agent_name: &str) -> (String, Vec<String>) {
    match agent_name {
        "claude" => (
            "claude".into(),
            vec![
                "--print".into(),
                "--no-session-persistence".into(),
                "--verbose".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--permission-mode".into(),
                "bypassPermissions".into(),
                "--tools".into(),
                "Read,Grep,Glob,LS".into(),
            ],
        ),
        "codex" => ("codex".into(), vec!["exec".into(), "--ephemeral".into()]),
        other => (other.into(), vec![]),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ScanBacklog {
    #[serde(default)]
    sessions: Vec<Session>,
}

impl ScanBacklog {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        if self.sessions.is_empty() {
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("Failed to remove {}", path.display()))?;
            }
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize scan-backlog.json")?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }

    fn merge_new_sessions(&mut self, mut new_sessions: Vec<Session>, seed_newest_first: bool) {
        let seen_paths: HashSet<PathBuf> = self
            .sessions
            .iter()
            .map(|session| session.path.clone())
            .collect();
        new_sessions.retain(|session| !seen_paths.contains(&session.path));
        sort_sessions_newest_first(&mut new_sessions);

        if self.sessions.is_empty() || seed_newest_first {
            self.sessions = new_sessions;
        } else {
            self.sessions.extend(new_sessions);
        }
    }

    fn batch(&self, batch_size: usize) -> Vec<Session> {
        self.sessions.iter().take(batch_size).cloned().collect()
    }

    fn remove_batch(&mut self, batch: &[Session]) {
        let batch_paths: HashSet<PathBuf> =
            batch.iter().map(|session| session.path.clone()).collect();
        self.sessions
            .retain(|session| !batch_paths.contains(&session.path));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScanManifest {
    workspace_root: String,
    manifest_path: String,
    session_roots: BTreeMap<String, String>,
    candidate_sessions: Vec<ManifestSession>,
    existing_skills: Vec<ManifestSkill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestSession {
    agent: String,
    session_id: String,
    timestamp: String,
    original_path: String,
    staged_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestSkill {
    name: String,
    original_path: String,
    staged_path: String,
}

#[derive(Debug, Clone)]
struct StagedSession {
    session: Session,
    staged_path: PathBuf,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
struct StagedSkill {
    staged_path: PathBuf,
}

struct StagedWorkspace {
    root: PathBuf,
    manifest: ScanManifest,
    staged_sessions: Vec<StagedSession>,
    #[cfg_attr(not(test), allow(dead_code))]
    staged_skills: Vec<StagedSkill>,
    cleanup_on_drop: bool,
}

impl Drop for StagedWorkspace {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFinding {
    session: PathBuf,
    summary: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ParsedScanResponse {
    inspected_files: Vec<PathBuf>,
    file_findings: Vec<FileFinding>,
    proposals: Vec<Proposal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProposalAgentMode {
    Codex,
    Claude,
    Generic,
}

struct AgentInvocation {
    final_output: String,
    audit_log: String,
    mode: ProposalAgentMode,
}

#[derive(Deserialize)]
struct RawScanResponse {
    inspected_files: Vec<String>,
    file_findings: Vec<RawFileFinding>,
    proposals: Vec<RawProposal>,
}

#[derive(Deserialize)]
struct RawFileFinding {
    session: String,
    summary: String,
}

#[derive(Deserialize)]
struct RawProposal {
    #[serde(rename = "type")]
    proposal_type: String,
    confidence: String,
    target_skill: Option<String>,
    evidence: Vec<RawEvidence>,
    body: String,
}

#[derive(Deserialize)]
struct RawEvidence {
    session: String,
    pattern: String,
}

#[derive(Deserialize)]
struct ClaudeEnvelope {
    is_error: Option<bool>,
    structured_output: Option<serde_json::Value>,
    result: Option<String>,
}

fn sort_sessions_newest_first(sessions: &mut [Session]) {
    sessions.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn is_codex_exec(command: &str, args: &[String]) -> bool {
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    command_name == "codex" && args.first().is_some_and(|arg| arg == "exec")
}

fn is_claude_cli(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        == "claude"
}

fn proposal_agent_mode(command: &str, args: &[String]) -> ProposalAgentMode {
    if is_codex_exec(command, args) {
        ProposalAgentMode::Codex
    } else if is_claude_cli(command) {
        ProposalAgentMode::Claude
    } else {
        ProposalAgentMode::Generic
    }
}

fn create_temp_file_path(prefix: &str, extension: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = tmp_dir.join(format!("{prefix}-{pid}-{nanos}-{attempt}.{extension}"));
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("Failed to create temporary file {}", path.display())
                });
            }
        }
    }
}

fn create_temp_dir_path(prefix: &str) -> Result<PathBuf> {
    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = tmp_dir.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create directory {}", path.display()));
            }
        }
    }
}

fn cleanup_temp_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn clipped_multiline(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    let mut chars = trimmed.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}\n...[truncated]")
    } else {
        clipped
    }
}

fn sanitize_agent_diagnostics(text: &str, prompt: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }

    let prompt_placeholder = format!("[distill prompt omitted: {} bytes]", prompt.len());
    let sanitized = if prompt.is_empty() {
        text.trim().to_string()
    } else {
        text.trim().replace(prompt, &prompt_placeholder)
    };

    clipped_multiline(&sanitized, MAX_AGENT_DIAGNOSTIC_CHARS)
}

fn format_agent_failure(command: &str, output: &std::process::Output, prompt: &str) -> String {
    let stdout = sanitize_agent_diagnostics(&String::from_utf8_lossy(&output.stdout), prompt);
    let stderr = sanitize_agent_diagnostics(&String::from_utf8_lossy(&output.stderr), prompt);
    let details = match (stderr.is_empty(), stdout.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(":\n{stderr}"),
        (true, false) => format!(":\n{stdout}"),
        (false, false) => format!(":\n{stderr}\n{stdout}"),
    };
    format!(
        "Agent command `{command}` failed with status {}{}",
        output.status, details
    )
}

fn agent_timeout_from_env(raw: Option<&str>) -> Result<Duration> {
    match raw {
        Some(raw) => {
            let secs: u64 = raw.parse().with_context(|| {
                format!(
                    "Failed to parse DISTILL_AGENT_TIMEOUT_SECS={raw:?} as an integer number of seconds"
                )
            })?;
            if secs == 0 {
                bail!("DISTILL_AGENT_TIMEOUT_SECS must be greater than 0.");
            }
            Ok(Duration::from_secs(secs))
        }
        None => Ok(Duration::from_secs(DEFAULT_AGENT_TIMEOUT_SECS)),
    }
}

fn agent_timeout() -> Result<Duration> {
    match std::env::var("DISTILL_AGENT_TIMEOUT_SECS") {
        Ok(raw) => agent_timeout_from_env(Some(&raw)),
        Err(std::env::VarError::NotPresent) => agent_timeout_from_env(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("DISTILL_AGENT_TIMEOUT_SECS must be valid Unicode.")
        }
    }
}

fn scan_batch_size() -> Result<usize> {
    match std::env::var("DISTILL_SCAN_BATCH_SIZE") {
        Ok(raw) => {
            let batch_size: usize = raw.parse().with_context(|| {
                format!("Failed to parse DISTILL_SCAN_BATCH_SIZE={raw:?} as a positive integer")
            })?;
            if batch_size == 0 {
                bail!("DISTILL_SCAN_BATCH_SIZE must be greater than 0.");
            }
            Ok(batch_size)
        }
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_SCAN_BATCH_SIZE),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("DISTILL_SCAN_BATCH_SIZE must be valid Unicode.")
        }
    }
}

fn scan_debug_dir() -> Result<Option<PathBuf>> {
    match std::env::var("DISTILL_SCAN_DEBUG_DIR") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(trimmed)))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("DISTILL_SCAN_DEBUG_DIR must be valid Unicode.")
        }
    }
}

fn create_debug_run_dir(base_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(base_dir)?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let path = base_dir.join(format!("scan-{timestamp}-{pid}-{attempt}"));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create directory {}", path.display()));
            }
        }
    }
}

fn write_debug_text(run_dir: Option<&Path>, filename: &str, contents: &str) {
    let Some(run_dir) = run_dir else {
        return;
    };

    let _ = std::fs::write(run_dir.join(filename), contents);
}

pub fn run_scan(agents: &[Box<dyn Agent>], scan_config: &ScanConfig) -> Result<Vec<Proposal>> {
    let scan_started_at = Utc::now();
    let last_scan = LastScan::load(&scan_config.last_scan_path)?;
    let discovery_since = last_scan
        .as_ref()
        .map(|last_scan| last_scan.timestamp)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
    let batch_size = scan_batch_size()?;
    let debug_run_dir = scan_debug_dir()?
        .map(|dir| create_debug_run_dir(&dir))
        .transpose()?;

    let collected_sessions = reader::collect_sessions(agents, discovery_since)?;
    let collected_count = collected_sessions.len();
    let candidate_sessions =
        filter_low_signal_sessions(filter_distill_scan_artifacts(collected_sessions));
    let skipped_internal = collected_count.saturating_sub(candidate_sessions.len());

    let mut backlog = ScanBacklog::load(&scan_config.backlog_path)?;
    let seed_newest_first = last_scan.is_none() && backlog.sessions.is_empty();
    backlog.merge_new_sessions(candidate_sessions, seed_newest_first);
    backlog.save(&scan_config.backlog_path)?;

    if skipped_internal > 0 {
        println!(
            "Skipped {} low-signal/internal session(s).",
            skipped_internal
        );
    }

    if backlog.sessions.is_empty() {
        println!("No pending sessions found for scan.");
        let watermark = LastScan {
            timestamp: scan_started_at,
            session_ids: vec![],
        };
        watermark.save(&scan_config.last_scan_path)?;
        return Ok(vec![]);
    }

    println!("Found {} session(s) to analyze.", backlog.sessions.len());
    println!("Pending scan backlog: {}", backlog.sessions.len());

    let batch = backlog.batch(batch_size);
    if batch.len() < backlog.sessions.len() {
        println!(
            "Capped this scan to {} newest pending session(s); rerun scan to continue draining the backlog.",
            batch.len()
        );
    }

    let skill_sources = load_existing_skill_sources(&scan_config.skill_dirs)?;
    println!("Loaded {} existing skill(s).", skill_sources.len());

    let preferences = match PreferenceProfile::load(&scan_config.history_dir) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("Warning: failed to load preference history (continuing without it): {err}");
            PreferenceProfile::default()
        }
    };
    if preferences.reviewed > 0 {
        println!(
            "Loaded preference history: {} reviewed decision(s), {} strong signal(s).",
            preferences.reviewed,
            preferences.signal_count()
        );
    }

    let workspace = stage_scan_workspace(&batch, &skill_sources, debug_run_dir.as_deref())?;
    let prompt = build_prompt(&workspace.manifest, &preferences);
    write_debug_text(debug_run_dir.as_deref(), "prompt.txt", &prompt);

    println!(
        "Inspecting {} staged session file(s) with `{}` (prompt: {} bytes)...",
        batch.len(),
        scan_config.agent_command,
        prompt.len()
    );
    println!("Waiting for agent response (this may take several minutes)...");

    let invocation = invoke_agent(
        &scan_config.agent_command,
        &scan_config.agent_args,
        &prompt,
        &workspace.root,
        debug_run_dir.as_deref(),
    )?;
    println!("Agent responded ({} bytes).", invocation.final_output.len());

    let parsed = parse_scan_response(&invocation.final_output, &workspace.root)?;
    write_debug_text(
        debug_run_dir.as_deref(),
        "parsed-response.json",
        &serde_json::to_string_pretty(&serde_json::json!({
            "inspected_files": parsed
                .inspected_files
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            "file_findings": parsed
                .file_findings
                .iter()
                .map(|finding| serde_json::json!({
                    "session": finding.session.to_string_lossy().to_string(),
                    "summary": finding.summary,
                }))
                .collect::<Vec<_>>(),
            "proposals": parsed
                .proposals
                .iter()
                .map(|proposal| serde_json::json!({
                    "type": format!("{:?}", proposal.frontmatter.proposal_type).to_lowercase(),
                    "confidence": format!("{:?}", proposal.frontmatter.confidence).to_lowercase(),
                    "target_skill": match proposal.frontmatter.resolved_target() {
                        Some(ProposalTarget::Skill { name }) => Some(name),
                        _ => None,
                    },
                    "evidence": proposal.frontmatter.evidence,
                    "body": proposal.body,
                }))
                .collect::<Vec<_>>(),
        }))
        .unwrap_or_else(|_| "{}".to_string()),
    );

    let mut proposals = validate_and_finalize_response(&parsed, &workspace, &invocation)?;
    println!("Agent proposed {} skill(s).", proposals.len());

    std::fs::create_dir_all(&scan_config.proposals_dir)?;
    for (index, proposal) in proposals.iter_mut().enumerate() {
        let filename = proposal_filename(proposal, index);
        let path = scan_config.proposals_dir.join(&filename);
        let markdown = proposal
            .to_markdown()
            .context("Failed to serialize proposal to markdown")?;
        std::fs::write(&path, markdown)
            .with_context(|| format!("Failed to write proposal {}", path.display()))?;
        proposal.filename = Some(filename);
    }

    backlog.remove_batch(&batch);
    backlog.save(&scan_config.backlog_path)?;

    let watermark = LastScan {
        timestamp: scan_started_at,
        session_ids: vec![],
    };
    watermark.save(&scan_config.last_scan_path)?;

    Ok(proposals)
}

fn filter_distill_scan_artifacts(sessions: Vec<Session>) -> Vec<Session> {
    sessions
        .into_iter()
        .filter(|session| !is_distill_scan_artifact(&session.path))
        .collect()
}

fn filter_low_signal_sessions(sessions: Vec<Session>) -> Vec<Session> {
    sessions
        .into_iter()
        .filter(|session| !is_low_signal_session_artifact(&session.path))
        .collect()
}

fn is_distill_scan_artifact(path: &Path) -> bool {
    let filename_looks_like_rollout = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-"));
    if !filename_looks_like_rollout {
        return false;
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };

    content.contains("You are a skill extraction engine for the `distill` tool.")
        && content.contains("Inspect every candidate session file listed below before answering")
}

fn is_low_signal_session_artifact(path: &Path) -> bool {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    filename.starts_with("agent-aprompt_suggestion-")
        || filename.starts_with("agent-prompt_suggestion-")
        || filename.starts_with("agent-acompact-")
        || filename.starts_with("agent-compact-")
}

fn proposal_filename(proposal: &Proposal, index: usize) -> String {
    let type_prefix = match proposal.frontmatter.proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    };
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    format!("{type_prefix}-{timestamp}-{index}.md")
}

fn load_existing_skill_sources(skill_dirs: &[PathBuf]) -> Result<Vec<SkillSource>> {
    crate::sync::load_skill_sources_from_dirs(skill_dirs)
}

fn stage_scan_workspace(
    batch: &[Session],
    skill_sources: &[SkillSource],
    debug_run_dir: Option<&Path>,
) -> Result<StagedWorkspace> {
    let (root, cleanup_on_drop) = if let Some(run_dir) = debug_run_dir {
        let root = run_dir.join("workspace");
        std::fs::create_dir_all(&root)?;
        (root, false)
    } else {
        (create_temp_dir_path("distill-scan-workspace")?, true)
    };

    let sessions_root = root.join("sessions");
    let skills_root = root.join("skills");
    std::fs::create_dir_all(&sessions_root)?;
    std::fs::create_dir_all(&skills_root)?;

    let mut session_roots = BTreeMap::new();
    let mut staged_sessions = Vec::new();
    let mut manifest_sessions = Vec::new();
    for (index, session) in batch.iter().enumerate() {
        let agent_dir = sessions_root.join(session.agent.to_string());
        std::fs::create_dir_all(&agent_dir)?;
        session_roots.insert(
            session.agent.to_string(),
            agent_dir.to_string_lossy().to_string(),
        );

        let basename = session
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session.jsonl");
        let staged_path = agent_dir.join(format!("{:04}-{}", index + 1, basename));
        std::fs::copy(&session.path, &staged_path).with_context(|| {
            format!(
                "Failed to copy session {} into staged workspace",
                session.path.display()
            )
        })?;

        staged_sessions.push(StagedSession {
            session: session.clone(),
            staged_path: staged_path.clone(),
        });
        manifest_sessions.push(ManifestSession {
            agent: session.agent.to_string(),
            session_id: session.id.clone(),
            timestamp: session.timestamp.to_rfc3339(),
            original_path: session.path.to_string_lossy().to_string(),
            staged_path: staged_path.to_string_lossy().to_string(),
        });
    }

    let mut staged_skills = Vec::new();
    let mut manifest_skills = Vec::new();
    for (index, skill_source) in skill_sources.iter().enumerate() {
        let staged_path = skills_root.join(format!(
            "{:04}-{}.md",
            index + 1,
            sanitize_filename(&skill_source.skill.name)
        ));
        std::fs::write(&staged_path, &skill_source.skill.content).with_context(|| {
            format!(
                "Failed to stage skill {} from {}",
                skill_source.skill.name,
                skill_source.source_path.display()
            )
        })?;
        staged_skills.push(StagedSkill {
            staged_path: staged_path.clone(),
        });
        manifest_skills.push(ManifestSkill {
            name: skill_source.skill.name.clone(),
            original_path: skill_source.source_path.to_string_lossy().to_string(),
            staged_path: staged_path.to_string_lossy().to_string(),
        });
    }

    let manifest_path = root.join("manifest.json");
    let manifest = ScanManifest {
        workspace_root: root.to_string_lossy().to_string(),
        manifest_path: manifest_path.to_string_lossy().to_string(),
        session_roots,
        candidate_sessions: manifest_sessions,
        existing_skills: manifest_skills,
    };
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;

    Ok(StagedWorkspace {
        root,
        manifest,
        staged_sessions,
        staged_skills,
        cleanup_on_drop,
    })
}

fn sanitize_filename(raw: &str) -> String {
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
        "skill".to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_prompt(manifest: &ScanManifest, preferences: &PreferenceProfile) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are a skill extraction engine for the `distill` tool.\n\n\
         Your job: inspect staged AI agent session files and propose reusable skills.\n\n\
         You are allowed to inspect staged files using read-only tools/commands.\n\
         Do not modify, create, delete, or rename any files.\n\n\
         Output quality bar:\n\
         - Propose only repeated, reusable workflows (not one-off tasks)\n\
         - Prefer `improve`/`edit` when an existing skill already overlaps\n\
         - If evidence is weak, return an empty proposals array, but still cover every session file\n\
         - Every proposal body must be concrete and actionable (no placeholders)\n\n\
         IMPORTANT: Respond ONLY with valid JSON in this exact wrapper shape:\n\
         {\"inspected_files\": [...], \"file_findings\": [...], \"proposals\": [...]}.\n\
         No markdown fences. No commentary.\n\n\
         Response requirements:\n\
         - `inspected_files`: every candidate session file path exactly once\n\
         - `file_findings`: one object per candidate session file with fields `session` and `summary`\n\
         - `proposals`: reusable skill proposals only\n\
         - Use the exact staged session path for `inspected_files`, `file_findings.session`, and every `evidence.session`\n\n\
         Each proposal object in `proposals` must have these fields:\n\
         - \"type\": one of \"new\", \"improve\", \"edit\", \"remove\"\n\
         - \"confidence\": one of \"high\", \"medium\", \"low\"\n\
         - \"target_skill\": string (required for improve/edit/remove, null for new)\n\
         - \"evidence\": array of {\"session\": \"<staged path>\", \"pattern\": \"<description>\"}\n\
         - \"body\": string containing the full proposed skill content in markdown\n\n\
         For each proposal body, use this markdown structure:\n\
         - `# <Skill Name>`\n\
         - `## When to use`\n\
         - `## Steps`\n\
         - `## Verification`\n\
         - `## Pitfalls`\n\n",
    );

    prompt.push_str(&format!(
        "## Workspace\n\n- Workspace root: {}\n- Manifest: {}\n\n",
        manifest.workspace_root, manifest.manifest_path
    ));

    prompt.push_str("## Session Roots By Agent\n\n");
    if manifest.session_roots.is_empty() {
        prompt.push_str("None.\n\n");
    } else {
        for (agent, root) in &manifest.session_roots {
            prompt.push_str(&format!("- {agent}: {root}\n"));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Candidate Session Files\n\n");
    for candidate in &manifest.candidate_sessions {
        prompt.push_str(&format!(
            "- staged: {}\n  agent: {}\n  timestamp: {}\n  original: {}\n",
            candidate.staged_path, candidate.agent, candidate.timestamp, candidate.original_path
        ));
    }
    prompt.push('\n');

    prompt.push_str("## Existing Skills\n\n");
    if manifest.existing_skills.is_empty() {
        prompt.push_str("None yet.\n\n");
    } else {
        for skill in &manifest.existing_skills {
            prompt.push_str(&format!(
                "- {} => staged: {} (original: {})\n",
                skill.name, skill.staged_path, skill.original_path
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str(&preferences.to_prompt_block());
    prompt.push_str(
        "Inspect every candidate session file listed above before answering. \
         Then return the JSON object with complete coverage and any high-signal proposals.\n",
    );

    prompt
}

fn invoke_agent(
    command: &str,
    args: &[String],
    prompt: &str,
    workspace_root: &Path,
    debug_run_dir: Option<&Path>,
) -> Result<AgentInvocation> {
    invoke_agent_with_timeout(
        command,
        args,
        prompt,
        workspace_root,
        agent_timeout()?,
        debug_run_dir,
    )
}

fn invoke_agent_with_timeout(
    command: &str,
    args: &[String],
    prompt: &str,
    workspace_root: &Path,
    timeout: Duration,
    debug_run_dir: Option<&Path>,
) -> Result<AgentInvocation> {
    let mode = proposal_agent_mode(command, args);
    let mut effective_args = args.to_vec();
    let mut temp_files = vec![];
    let mut codex_output_path = None;

    match mode {
        ProposalAgentMode::Codex => {
            if !effective_args.iter().any(|arg| arg == "--json") {
                effective_args.push("--json".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--skip-git-repo-check")
            {
                effective_args.push("--skip-git-repo-check".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--sandbox" || arg == "-s")
            {
                effective_args.push("--sandbox".into());
                effective_args.push("read-only".into());
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "-C" || arg == "--cd")
            {
                effective_args.push("-C".into());
                effective_args.push(workspace_root.to_string_lossy().to_string());
            }
            if !effective_args.iter().any(|arg| arg == "--output-schema") {
                let schema_path = create_temp_file_path("distill-codex-schema", "json")?;
                std::fs::write(&schema_path, PROPOSAL_SCHEMA).with_context(|| {
                    format!(
                        "Failed to write Codex schema file {}",
                        schema_path.display()
                    )
                })?;
                effective_args.push("--output-schema".into());
                effective_args.push(schema_path.to_string_lossy().to_string());
                temp_files.push(schema_path);
            }
            if !effective_args
                .iter()
                .any(|arg| arg == "--output-last-message" || arg == "-o")
            {
                let last_message_path = create_temp_file_path("distill-codex-last-message", "txt")?;
                effective_args.push("--output-last-message".into());
                effective_args.push(last_message_path.to_string_lossy().to_string());
                codex_output_path = Some(last_message_path.clone());
                temp_files.push(last_message_path);
            }
        }
        ProposalAgentMode::Claude => {
            if !effective_args.iter().any(|arg| arg == "--add-dir") {
                effective_args.push("--add-dir".into());
                effective_args.push(workspace_root.to_string_lossy().to_string());
            }
        }
        ProposalAgentMode::Generic => {}
    }

    let mut child = Command::new(command)
        .args(&effective_args)
        .current_dir(workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to execute agent command: {command}"))?;

    if let Some(mut stdin) = child.stdin.take()
        && let Err(write_err) = stdin.write_all(prompt.as_bytes())
    {
        if write_err.kind() == std::io::ErrorKind::BrokenPipe {
            return match child.wait_with_output() {
                Ok(output) if !output.status.success() => {
                    persist_agent_debug_output(
                        debug_run_dir,
                        prompt,
                        &String::from_utf8_lossy(&output.stdout),
                        &String::from_utf8_lossy(&output.stderr),
                        None,
                    );
                    cleanup_temp_files(&temp_files);
                    Err(write_err).with_context(|| format_agent_failure(command, &output, prompt))
                }
                Ok(output) => {
                    persist_agent_debug_output(
                        debug_run_dir,
                        prompt,
                        &String::from_utf8_lossy(&output.stdout),
                        &String::from_utf8_lossy(&output.stderr),
                        None,
                    );
                    cleanup_temp_files(&temp_files);
                    Err(write_err)
                        .with_context(|| format!("Failed to write prompt to {command} stdin"))
                }
                Err(wait_err) => {
                    cleanup_temp_files(&temp_files);
                    Err(wait_err).with_context(|| {
                        format!(
                            "Failed to write prompt to {command} stdin, then failed to collect agent output"
                        )
                    })
                }
            };
        }

        cleanup_temp_files(&temp_files);
        return Err(write_err)
            .with_context(|| format!("Failed to write prompt to {command} stdin"));
    }

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let heartbeat = std::thread::spawn(move || {
        let mut elapsed = 0u64;
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(10));
            if stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            elapsed += 10;
            eprint!("\r  ...agent working ({elapsed}s)   ");
        }
        eprint!("\r                            \r");
    });

    let wait_started = Instant::now();
    let output = loop {
        match child.try_wait() {
            Ok(Some(_status)) => match child.wait_with_output() {
                Ok(output) => break output,
                Err(err) => {
                    cleanup_temp_files(&temp_files);
                    return Err(err).with_context(|| {
                        format!("Failed to collect output from agent command: {command}")
                    });
                }
            },
            Ok(None) => {
                if wait_started.elapsed() >= timeout {
                    let _ = child.kill();
                    let timed_out_output = child.wait_with_output().ok();
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = heartbeat.join();

                    let stderr = timed_out_output
                        .as_ref()
                        .map(|output| String::from_utf8_lossy(&output.stderr).to_string())
                        .unwrap_or_default();
                    let stdout = timed_out_output
                        .as_ref()
                        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
                        .unwrap_or_default();

                    persist_agent_debug_output(debug_run_dir, prompt, &stdout, &stderr, None);

                    let stderr = sanitize_agent_diagnostics(&stderr, prompt);
                    let stdout = sanitize_agent_diagnostics(&stdout, prompt);
                    let details = match (stderr.is_empty(), stdout.is_empty()) {
                        (true, true) => String::new(),
                        (false, true) => format!("\nAgent stderr before timeout:\n{stderr}"),
                        (true, false) => format!("\nAgent stdout before timeout:\n{stdout}"),
                        (false, false) => format!(
                            "\nAgent stderr before timeout:\n{stderr}\nAgent stdout before timeout:\n{stdout}"
                        ),
                    };

                    cleanup_temp_files(&temp_files);
                    bail!(
                        "Agent command `{command}` timed out after {}s. Verify the CLI is installed and authenticated, or raise DISTILL_AGENT_TIMEOUT_SECS for slower runs.{details}",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(Duration::from_millis(AGENT_POLL_INTERVAL_MS));
            }
            Err(err) => {
                cleanup_temp_files(&temp_files);
                return Err(err)
                    .with_context(|| format!("Failed to wait for agent command: {command}"));
            }
        }
    };

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = heartbeat.join();

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        persist_agent_debug_output(debug_run_dir, prompt, &stdout, &stderr, None);
        cleanup_temp_files(&temp_files);
        bail!("{}", format_agent_failure(command, &output, prompt));
    }

    let stdout = String::from_utf8(output.stdout).context("Agent stdout is not valid UTF-8")?;
    let stderr = String::from_utf8(output.stderr).context("Agent stderr is not valid UTF-8")?;

    let final_output = match mode {
        ProposalAgentMode::Codex => {
            let stdout_fallback = stdout.clone();
            if let Some(path) = codex_output_path {
                match std::fs::read_to_string(&path) {
                    Ok(contents) if !contents.trim().is_empty() => contents,
                    _ => stdout_fallback,
                }
            } else {
                stdout_fallback
            }
        }
        ProposalAgentMode::Claude => extract_claude_stream_output(&stdout)?,
        ProposalAgentMode::Generic => stdout.clone(),
    };

    persist_agent_debug_output(debug_run_dir, prompt, &stdout, &stderr, Some(&final_output));
    cleanup_temp_files(&temp_files);

    Ok(AgentInvocation {
        final_output,
        audit_log: match mode {
            ProposalAgentMode::Generic => String::new(),
            _ => stdout,
        },
        mode,
    })
}

fn persist_agent_debug_output(
    debug_run_dir: Option<&Path>,
    prompt: &str,
    stdout: &str,
    stderr: &str,
    final_output: Option<&str>,
) {
    write_debug_text(debug_run_dir, "prompt.txt", prompt);
    write_debug_text(debug_run_dir, "agent-stdout.log", stdout);
    write_debug_text(debug_run_dir, "agent-stderr.log", stderr);
    if let Some(final_output) = final_output {
        write_debug_text(debug_run_dir, "agent-final-output.txt", final_output);
    }
}

fn extract_claude_stream_output(stdout: &str) -> Result<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("Claude agent returned no output");
    }

    let mut candidates = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if value
            .get("is_error")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let message = value
                .get("result")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown Claude error");
            bail!("Claude agent returned an error: {message}");
        }

        if let Some(structured) = value.get("structured_output") {
            candidates.push(structured.to_string());
        }

        collect_text_candidates(&value, &mut candidates, 0);
    }

    for candidate in candidates.into_iter().rev() {
        if extract_json_value(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    if extract_json_value(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }

    bail!("Failed to extract structured JSON from Claude stream output")
}

fn collect_text_candidates(value: &serde_json::Value, out: &mut Vec<String>, depth: usize) {
    const MAX_DEPTH: usize = 4;
    if depth > MAX_DEPTH {
        return;
    }

    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_text_candidates(item, out, depth + 1);
            }
        }
        serde_json::Value::Object(map) => {
            for key in ["result", "text", "content", "message", "structured_output"] {
                if let Some(next) = map.get(key) {
                    collect_text_candidates(next, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn parse_scan_response(raw: &str, workspace_root: &Path) -> Result<ParsedScanResponse> {
    let response_value = extract_response_value(raw)?;
    let raw_response: RawScanResponse =
        serde_json::from_value(response_value).context("Failed to parse agent response")?;

    let inspected_files = raw_response
        .inspected_files
        .into_iter()
        .map(|path| normalize_reported_path(&path, workspace_root))
        .collect::<Vec<_>>();

    let file_findings = raw_response
        .file_findings
        .into_iter()
        .map(|finding| FileFinding {
            session: normalize_reported_path(&finding.session, workspace_root),
            summary: finding.summary,
        })
        .collect::<Vec<_>>();

    let mut proposals = Vec::new();
    for raw_proposal in raw_response.proposals {
        proposals.push(convert_raw_proposal(raw_proposal, workspace_root)?);
    }

    Ok(ParsedScanResponse {
        inspected_files,
        file_findings,
        proposals,
    })
}

fn extract_response_value(raw: &str) -> Result<serde_json::Value> {
    let trimmed = raw.trim();
    if let Ok(envelope) = serde_json::from_str::<ClaudeEnvelope>(trimmed) {
        if envelope.is_error.unwrap_or(false) {
            let message = envelope
                .result
                .unwrap_or_else(|| "unknown Claude error".to_string());
            bail!("Claude agent returned an error: {message}");
        }

        if let Some(structured) = envelope.structured_output {
            return Ok(structured);
        }
        if let Some(text) = envelope.result {
            return extract_json_value(&text);
        }
    }

    extract_json_value(trimmed)
}

fn extract_json_value(text: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();
    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner
            .rfind("```")
            .map(|position| &inner[..position])
            .unwrap_or(inner)
            .trim()
    } else {
        trimmed
    };

    serde_json::from_str(json_str).context("Failed to parse agent response as JSON")
}

fn normalize_reported_path(raw_path: &str, workspace_root: &Path) -> PathBuf {
    let path = PathBuf::from(raw_path);
    let absolute = if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    };
    canonicalize_path(&absolute)
}

fn canonicalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn convert_raw_proposal(raw_proposal: RawProposal, workspace_root: &Path) -> Result<Proposal> {
    let proposal_type = match raw_proposal.proposal_type.as_str() {
        "new" => ProposalType::New,
        "improve" => ProposalType::Improve,
        "edit" => ProposalType::Edit,
        "remove" => ProposalType::Remove,
        other => bail!("Unknown proposal type: {other}"),
    };
    let confidence = match raw_proposal.confidence.as_str() {
        "high" => Confidence::High,
        "medium" => Confidence::Medium,
        "low" => Confidence::Low,
        other => bail!("Unknown confidence level: {other}"),
    };

    let target_skill_is_present = raw_proposal
        .target_skill
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty());
    match proposal_type {
        ProposalType::New if target_skill_is_present => {
            bail!("Proposal type `new` must set `target_skill` to null");
        }
        ProposalType::Improve | ProposalType::Edit | ProposalType::Remove
            if !target_skill_is_present =>
        {
            bail!(
                "Proposal type `{}` requires a non-empty `target_skill`",
                raw_proposal.proposal_type
            );
        }
        _ => {}
    }

    if raw_proposal.body.trim().is_empty() {
        bail!("Proposal body must be non-empty");
    }

    let evidence = raw_proposal
        .evidence
        .into_iter()
        .map(|evidence| Evidence {
            session: normalize_reported_path(&evidence.session, workspace_root)
                .to_string_lossy()
                .to_string(),
            pattern: evidence.pattern,
        })
        .collect::<Vec<_>>();

    Ok(Proposal {
        frontmatter: ProposalFrontmatter {
            proposal_type,
            confidence,
            target: raw_proposal
                .target_skill
                .map(|name| ProposalTarget::Skill { name }),
            target_skill: None,
            evidence,
            created: Utc::now(),
        },
        body: raw_proposal.body,
        filename: None,
    })
}

fn validate_and_finalize_response(
    parsed: &ParsedScanResponse,
    workspace: &StagedWorkspace,
    invocation: &AgentInvocation,
) -> Result<Vec<Proposal>> {
    let expected_paths = workspace
        .staged_sessions
        .iter()
        .map(|session| canonicalize_path(&session.staged_path))
        .collect::<Vec<_>>();
    let expected_set = expected_paths.iter().cloned().collect::<HashSet<_>>();

    validate_full_coverage("inspected_files", &parsed.inspected_files, &expected_paths)?;

    let finding_sessions = parsed
        .file_findings
        .iter()
        .map(|finding| finding.session.clone())
        .collect::<Vec<_>>();
    validate_full_coverage("file_findings", &finding_sessions, &expected_paths)?;

    for finding in &parsed.file_findings {
        if finding.summary.trim().is_empty() {
            bail!(
                "Every file finding must include a non-empty summary (missing for {})",
                finding.session.display()
            );
        }
    }

    for proposal in &parsed.proposals {
        for evidence in &proposal.frontmatter.evidence {
            let evidence_path = PathBuf::from(&evidence.session);
            if !expected_set.contains(&evidence_path) {
                bail!(
                    "Proposal evidence referenced non-batch file {}",
                    evidence_path.display()
                );
            }
        }
    }

    if invocation.mode != ProposalAgentMode::Generic {
        validate_audit_trail(&invocation.audit_log, &expected_paths)?;
    }

    let staged_to_original = workspace
        .staged_sessions
        .iter()
        .map(|session| {
            (
                canonicalize_path(&session.staged_path),
                session.session.path.to_string_lossy().to_string(),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut proposals = parsed.proposals.clone();
    for proposal in &mut proposals {
        for evidence in &mut proposal.frontmatter.evidence {
            let staged_path = PathBuf::from(&evidence.session);
            let Some(original) = staged_to_original.get(&staged_path) else {
                bail!(
                    "Failed to map staged evidence path {} back to original session path",
                    staged_path.display()
                );
            };
            evidence.session = original.clone();
        }
    }

    Ok(proposals)
}

fn validate_full_coverage(label: &str, actual: &[PathBuf], expected: &[PathBuf]) -> Result<()> {
    let actual_set = actual.iter().cloned().collect::<HashSet<_>>();
    let expected_set = expected.iter().cloned().collect::<HashSet<_>>();

    if actual.len() != expected.len() {
        bail!(
            "{label} must contain exactly {} file(s), found {}",
            expected.len(),
            actual.len()
        );
    }

    let missing = expected
        .iter()
        .filter(|path| !actual_set.contains(*path))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("{label} omitted staged file(s): {}", missing.join(", "));
    }

    let unexpected = actual
        .iter()
        .filter(|path| !expected_set.contains(*path))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        bail!(
            "{label} referenced non-batch file(s): {}",
            unexpected.join(", ")
        );
    }

    Ok(())
}

fn validate_audit_trail(audit_log: &str, expected_paths: &[PathBuf]) -> Result<()> {
    let touched = touched_paths_from_audit_log(audit_log, expected_paths);
    let missing = expected_paths
        .iter()
        .filter(|path| !touched.contains(*path))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "Agent audit trail did not show read activity for staged file(s): {}",
            missing.join(", ")
        );
    }

    Ok(())
}

fn touched_paths_from_audit_log(audit_log: &str, candidate_paths: &[PathBuf]) -> HashSet<PathBuf> {
    let mut touched = HashSet::new();
    for line in audit_log.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if !looks_like_audit_event(&value) {
            continue;
        }

        let mut strings = Vec::new();
        collect_all_strings(&value, &mut strings, 0);
        let haystack = strings.join("\n");
        for path in candidate_paths {
            if path_search_variants(path)
                .iter()
                .any(|variant| haystack.contains(variant))
            {
                touched.insert(path.clone());
            }
        }
    }

    touched
}

fn path_search_variants(path: &Path) -> Vec<String> {
    let mut variants = vec![path.to_string_lossy().to_string()];
    let canonical = canonicalize_path(path).to_string_lossy().to_string();
    if !variants.contains(&canonical) {
        variants.push(canonical);
    }

    let mut expanded = variants.clone();
    for variant in variants {
        if let Some(stripped) = variant.strip_prefix("/private") {
            let stripped = stripped.to_string();
            if !expanded.contains(&stripped) {
                expanded.push(stripped);
            }
        } else if variant.starts_with("/var/") || variant == "/var" {
            let prefixed = format!("/private{variant}");
            if !expanded.contains(&prefixed) {
                expanded.push(prefixed);
            }
        }
    }

    expanded
}

fn looks_like_audit_event(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(item) = map.get("item")
                && looks_like_audit_event(item)
            {
                return true;
            }

            let event_type = map
                .get("type")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if contains_audit_keyword(event_type) {
                return true;
            }

            let item_type = map
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            contains_audit_keyword(item_type)
        }
        _ => false,
    }
}

fn contains_audit_keyword(raw: &str) -> bool {
    let lowered = raw.to_ascii_lowercase();
    ["command", "tool", "bash", "read", "grep", "glob", "ls"]
        .iter()
        .any(|needle| lowered.contains(needle))
}

fn collect_all_strings(value: &serde_json::Value, out: &mut Vec<String>, depth: usize) {
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return;
    }

    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_all_strings(item, out, depth + 1);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_all_strings(value, out, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Skill;

    fn sample_session(id: &str, agent: AgentKind, hours_ago: i64) -> Session {
        Session {
            id: id.to_string(),
            agent,
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
            content: String::new(),
        }
    }

    #[test]
    fn test_backlog_seeds_newest_first_on_first_scan() {
        let mut backlog = ScanBacklog::default();
        backlog.merge_new_sessions(
            vec![
                sample_session("older", AgentKind::Claude, 10),
                sample_session("newer", AgentKind::Claude, 1),
            ],
            true,
        );

        assert_eq!(backlog.sessions.len(), 2);
        assert_eq!(backlog.sessions[0].id, "newer");
        assert_eq!(backlog.sessions[1].id, "older");
    }

    #[test]
    fn test_backlog_appends_new_sessions_after_existing_queue() {
        let mut backlog = ScanBacklog {
            sessions: vec![sample_session("existing", AgentKind::Claude, 5)],
        };

        backlog.merge_new_sessions(
            vec![
                sample_session("newer", AgentKind::Claude, 1),
                sample_session("older-new", AgentKind::Claude, 3),
            ],
            false,
        );

        assert_eq!(
            backlog
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["existing", "newer", "older-new"]
        );
    }

    #[test]
    fn test_build_prompt_references_manifest_and_not_excerpts() {
        let manifest = ScanManifest {
            workspace_root: "/tmp/workspace".to_string(),
            manifest_path: "/tmp/workspace/manifest.json".to_string(),
            session_roots: BTreeMap::from([(
                "claude".to_string(),
                "/tmp/workspace/sessions/claude".to_string(),
            )]),
            candidate_sessions: vec![ManifestSession {
                agent: "claude".to_string(),
                session_id: "session-1".to_string(),
                timestamp: "2026-03-10T12:00:00Z".to_string(),
                original_path: "/Users/me/.claude/projects/demo/session-1.jsonl".to_string(),
                staged_path: "/tmp/workspace/sessions/claude/0001-session-1.jsonl".to_string(),
            }],
            existing_skills: vec![ManifestSkill {
                name: "review".to_string(),
                original_path: "/Users/me/.agents/skills/review/SKILL.md".to_string(),
                staged_path: "/tmp/workspace/skills/0001-review.md".to_string(),
            }],
        };

        let prompt = build_prompt(&manifest, &PreferenceProfile::default());
        assert!(prompt.contains("Workspace root: /tmp/workspace"));
        assert!(prompt.contains("/tmp/workspace/manifest.json"));
        assert!(prompt.contains("Inspect every candidate session file"));
        assert!(!prompt.contains("Session Excerpts"));
        assert!(!prompt.contains("Do NOT execute tools/commands"));
    }

    #[test]
    fn test_parse_scan_response_normalizes_relative_paths() {
        let raw = r##"{
          "inspected_files": ["sessions/claude/0001-demo.jsonl"],
          "file_findings": [{"session":"sessions/claude/0001-demo.jsonl","summary":"Repeated testing workflow."}],
          "proposals": [{
            "type":"new",
            "confidence":"high",
            "target_skill":null,
            "evidence":[{"session":"sessions/claude/0001-demo.jsonl","pattern":"Repeated testing workflow."}],
            "body":"# Test Skill"
          }]
        }"##;

        let parsed = parse_scan_response(raw, Path::new("/tmp/workspace")).unwrap();
        assert_eq!(
            parsed.inspected_files,
            vec![PathBuf::from(
                "/tmp/workspace/sessions/claude/0001-demo.jsonl"
            )]
        );
        assert_eq!(
            parsed.proposals[0].frontmatter.evidence[0].session,
            "/tmp/workspace/sessions/claude/0001-demo.jsonl"
        );
    }

    #[test]
    fn test_validate_and_finalize_response_rejects_missing_coverage() {
        let workspace = StagedWorkspace {
            root: PathBuf::from("/tmp/workspace"),
            manifest: ScanManifest {
                workspace_root: "/tmp/workspace".to_string(),
                manifest_path: "/tmp/workspace/manifest.json".to_string(),
                session_roots: BTreeMap::new(),
                candidate_sessions: vec![],
                existing_skills: vec![],
            },
            staged_sessions: vec![
                StagedSession {
                    session: sample_session("one", AgentKind::Claude, 2),
                    staged_path: PathBuf::from("/tmp/workspace/sessions/claude/one.jsonl"),
                },
                StagedSession {
                    session: sample_session("two", AgentKind::Claude, 1),
                    staged_path: PathBuf::from("/tmp/workspace/sessions/claude/two.jsonl"),
                },
            ],
            staged_skills: vec![],
            cleanup_on_drop: false,
        };

        let parsed = ParsedScanResponse {
            inspected_files: vec![PathBuf::from("/tmp/workspace/sessions/claude/one.jsonl")],
            file_findings: vec![FileFinding {
                session: PathBuf::from("/tmp/workspace/sessions/claude/one.jsonl"),
                summary: "Only one file covered.".to_string(),
            }],
            proposals: vec![],
        };

        let err = validate_and_finalize_response(
            &parsed,
            &workspace,
            &AgentInvocation {
                final_output: String::new(),
                audit_log: String::new(),
                mode: ProposalAgentMode::Generic,
            },
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("inspected_files"));
        assert!(err.contains("exactly 2"));
    }

    #[test]
    fn test_touched_paths_from_audit_log_ignores_non_tool_messages() {
        let path = PathBuf::from("/tmp/workspace/sessions/claude/one.jsonl");
        let log = format!(
            "{}\n{}",
            serde_json::json!({
                "type": "assistant",
                "text": path.to_string_lossy().to_string()
            }),
            serde_json::json!({
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "command": format!("sed -n '1,20p' {}", path.display())
                }
            })
        );

        let touched = touched_paths_from_audit_log(&log, std::slice::from_ref(&path));
        assert!(touched.contains(&path));
        assert_eq!(touched.len(), 1);
    }

    #[test]
    fn test_stage_scan_workspace_copies_sessions_and_skills() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.jsonl");
        let skill_path = dir.path().join("review.md");
        std::fs::write(&session_path, "{\"role\":\"user\"}").unwrap();
        std::fs::write(&skill_path, "# Review").unwrap();

        let session = Session {
            id: "session-1".to_string(),
            agent: AgentKind::Claude,
            path: session_path.clone(),
            timestamp: Utc::now(),
            content: String::new(),
        };
        let skill_source = SkillSource {
            skill: Skill {
                name: "review".to_string(),
                content: "# Review".to_string(),
            },
            source_path: skill_path.clone(),
        };

        let workspace = stage_scan_workspace(&[session], &[skill_source], None).unwrap();
        assert_eq!(workspace.staged_sessions.len(), 1);
        assert!(workspace.staged_sessions[0].staged_path.is_file());
        assert_eq!(
            std::fs::read_to_string(&workspace.staged_sessions[0].staged_path).unwrap(),
            "{\"role\":\"user\"}"
        );
        assert_eq!(workspace.staged_skills.len(), 1);
        assert!(workspace.staged_skills[0].staged_path.is_file());
        assert_eq!(
            std::fs::read_to_string(&workspace.staged_skills[0].staged_path).unwrap(),
            "# Review"
        );
    }

    #[test]
    fn test_extract_claude_stream_output_uses_last_json_candidate() {
        let stdout = r#"{"type":"tool_use","tool":"Read","path":"/tmp/workspace/sessions/claude/0001.jsonl"}
{"type":"assistant","text":"thinking"}
{"type":"result","result":"{\"inspected_files\":[\"/tmp/workspace/sessions/claude/0001.jsonl\"],\"file_findings\":[{\"session\":\"/tmp/workspace/sessions/claude/0001.jsonl\",\"summary\":\"Repeated workflow.\"}],\"proposals\":[]}"}"#;

        let output = extract_claude_stream_output(stdout).unwrap();
        assert!(output.contains("\"inspected_files\""));
    }
}
