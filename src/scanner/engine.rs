use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::agents::{Agent, AgentKind, Session};
use crate::config::Config;
use crate::preferences::PreferenceProfile;
use crate::proposal_runner::{
    ProposalAgentMode, cleanup_temp_files, extract_json_value, finalize_proposal_output,
    prepare_proposal_command, proposal_agent_command,
};
use crate::proposals::{
    Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalTarget, ProposalType,
    infer_skill_name_from_body,
};
use crate::scanner::reader::{self, LastScan};
use crate::scanner::state::{ReadyWorkflow, ScanState, WorkflowFinding};
use crate::scanner::timeline::{
    SessionDescriptor, TimelineWindowRequest, build_session_timeline, discover_session,
    render_timeline, render_timeline_window,
};
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

const WORKFLOW_DETECTION_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "inspected_files": {
      "type": "array",
      "items": { "type": "string" }
    },
    "session_findings": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "session": { "type": "string" },
          "summary": { "type": "string" },
          "candidates": {
            "type": "array",
            "items": {
              "type": "object",
              "additionalProperties": false,
              "properties": {
                "workflow_key": { "type": "string" },
                "workflow_label": { "type": ["string", "null"] },
                "note": { "type": "string" },
                "start_event": { "type": "integer", "minimum": 1 },
                "end_event": { "type": "integer", "minimum": 1 }
              },
              "required": ["workflow_key", "workflow_label", "note", "start_event", "end_event"]
            }
          }
        },
        "required": ["session", "summary", "candidates"]
      }
    }
  },
  "required": ["inspected_files", "session_findings"]
}"#;

const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 2 * 60 * 60;
const AGENT_POLL_INTERVAL_MS: u64 = 250;
const DEFAULT_SCAN_BATCH_SIZE: usize = 20;
const DEFAULT_SCAN_MAX_RAW_BYTES: u64 = 64 * 1024 * 1024;
const MIN_WORKFLOW_MATCHES_FOR_PROPOSAL: usize = 3;
const MAX_AGENT_TAIL_BYTES: usize = 16 * 1024;
const MAX_AGENT_DIAGNOSTIC_CHARS: usize = 4000;
#[cfg(test)]
const MAX_STAGED_SUMMARY_EXCERPT_CHARS: usize = 500;

pub struct ScanConfig {
    pub agent_command: String,
    pub agent_args: Vec<String>,
    pub skill_dirs: Vec<PathBuf>,
    pub proposals_dir: PathBuf,
    pub last_scan_path: PathBuf,
    pub backlog_path: PathBuf,
    pub state_path: PathBuf,
    pub history_dir: PathBuf,
}

impl ScanConfig {
    pub fn from_config(config: &Config) -> Self {
        let command = proposal_agent_command(&config.proposal_agent);
        Self {
            agent_command: command.command,
            agent_args: command.args,
            skill_dirs: vec![Config::skills_dir(), Config::shared_skills_dir()],
            proposals_dir: Config::proposals_dir(),
            last_scan_path: Config::last_scan_path(),
            backlog_path: Config::scan_backlog_path(),
            state_path: Config::scan_state_path(),
            history_dir: Config::history_dir(),
        }
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
    source_session: Session,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionWorkflowFinding {
    session: PathBuf,
    summary: String,
    candidates: Vec<WorkflowFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedWorkflowResponse {
    inspected_files: Vec<PathBuf>,
    session_findings: Vec<SessionWorkflowFinding>,
}

struct AgentInvocation {
    final_output: String,
    touched_paths: HashSet<PathBuf>,
    mode: ProposalAgentMode,
}

#[derive(Debug, Clone)]
struct ScanDebugArtifacts {
    run_dir: PathBuf,
    current_run_path: PathBuf,
    last_failed_run_path: PathBuf,
    cleanup_on_success: bool,
}

#[derive(Debug, Serialize)]
struct ScanRunStatus {
    state: String,
    started_at: String,
    updated_at: String,
    phase: String,
    scan_pid: u32,
    agent_pid: Option<u32>,
    agent_command: String,
    workspace_root: String,
    batch_size: usize,
    selected_raw_bytes: u64,
    staged_bytes: u64,
    discovered_sessions: usize,
    candidate_sessions: usize,
    skipped_sessions: usize,
    backlog_sessions: usize,
    ready_workflows: usize,
    proposals_written: usize,
    prompt_bytes: usize,
    timeout_secs: Option<u64>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    last_stdout_at: Option<String>,
    last_stderr_at: Option<String>,
    durations_ms: ScanPhaseDurations,
    note: Option<String>,
}

#[derive(Debug, Serialize, Default, Clone)]
struct ScanPhaseDurations {
    discovery: u64,
    selection: u64,
    staging: u64,
    detection_agent: u64,
    proposal_agent: u64,
    finalize: u64,
}

struct LiveScanStatusUpdate<'a> {
    started_at: DateTime<Utc>,
    state: &'a str,
    phase: &'a str,
    command: &'a str,
    args: &'a [String],
    workspace_root: &'a Path,
    batch_size: usize,
    selected_raw_bytes: u64,
    staged_bytes: u64,
    discovered_sessions: usize,
    candidate_sessions: usize,
    skipped_sessions: usize,
    backlog_sessions: usize,
    ready_workflows: usize,
    proposals_written: usize,
    prompt_bytes: usize,
    timeout: Option<Duration>,
    agent_pid: Option<u32>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    last_stdout_at: Option<SystemTime>,
    last_stderr_at: Option<SystemTime>,
    durations: ScanPhaseDurations,
    note: Option<String>,
}

struct AgentRunContext<'a> {
    phase: &'a str,
    timeout: Option<Duration>,
    debug_run_dir: Option<&'a Path>,
    debug_artifacts: Option<&'a ScanDebugArtifacts>,
    batch_size: usize,
    selected_raw_bytes: u64,
    staged_bytes: u64,
    discovered_sessions: usize,
    candidate_sessions: usize,
    skipped_sessions: usize,
    backlog_sessions: usize,
    ready_workflows: usize,
    proposals_written: usize,
    durations: ScanPhaseDurations,
    output_schema: Option<&'a str>,
    candidate_paths: Vec<PathBuf>,
}

struct StreamCapture {
    output_path: PathBuf,
    bytes_captured: Arc<AtomicU64>,
    last_update: Arc<Mutex<Option<SystemTime>>>,
    touched_paths: Arc<Mutex<HashSet<PathBuf>>>,
    join_handle: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl ScanDebugArtifacts {
    fn new(base_dir: PathBuf, cleanup_on_success: bool) -> Result<Self> {
        let run_dir = create_debug_run_dir(&base_dir)?;
        let current_run_path = base_dir.join("current-run.txt");
        let last_failed_run_path = base_dir.join("last-failed-run.txt");
        std::fs::write(&current_run_path, run_dir.display().to_string()).with_context(|| {
            format!(
                "Failed to write current scan debug pointer {}",
                current_run_path.display()
            )
        })?;
        Ok(Self {
            run_dir,
            current_run_path,
            last_failed_run_path,
            cleanup_on_success,
        })
    }

    fn status_path(&self) -> PathBuf {
        self.run_dir.join("scan-status.json")
    }

    fn stdout_path(&self) -> PathBuf {
        self.run_dir.join("agent-stdout.log")
    }

    fn stderr_path(&self) -> PathBuf {
        self.run_dir.join("agent-stderr.log")
    }

    fn write_status(&self, status: &ScanRunStatus) {
        if let Ok(json) = serde_json::to_string_pretty(status) {
            let _ = std::fs::write(self.status_path(), json);
        }
    }

    fn finish_success(&self) {
        self.clear_current_run_pointer();
        if self.cleanup_on_success {
            let _ = std::fs::remove_dir_all(&self.run_dir);
        }
    }

    fn finish_failure(&self, error: &anyhow::Error) {
        let _ = std::fs::write(self.run_dir.join("error.txt"), error.to_string());
        let _ = std::fs::write(
            &self.last_failed_run_path,
            self.run_dir.display().to_string(),
        );
        self.clear_current_run_pointer();
    }

    fn clear_current_run_pointer(&self) {
        let matches_current_run = std::fs::read_to_string(&self.current_run_path)
            .map(|value| value.trim() == self.run_dir.to_string_lossy())
            .unwrap_or(false);
        if matches_current_run {
            let _ = std::fs::remove_file(&self.current_run_path);
        }
    }
}

impl StreamCapture {
    fn spawn<R: Read + Send + 'static>(
        reader: R,
        output_path: PathBuf,
        candidate_paths: Vec<PathBuf>,
    ) -> Self {
        let bytes_captured = Arc::new(AtomicU64::new(0));
        let last_update = Arc::new(Mutex::new(None));
        let touched_paths = Arc::new(Mutex::new(HashSet::new()));
        let bytes_captured_clone = bytes_captured.clone();
        let last_update_clone = last_update.clone();
        let touched_paths_clone = touched_paths.clone();
        let output_path_for_thread = output_path.clone();
        let join_handle = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(reader);
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&output_path_for_thread)?;
            let mut buffer = String::new();
            let mut tail = Vec::new();
            loop {
                buffer.clear();
                let read = reader.read_line(&mut buffer)?;
                if read == 0 {
                    break;
                }
                file.write_all(buffer.as_bytes())?;
                file.flush()?;
                update_touched_paths_from_line(&buffer, &candidate_paths, &touched_paths_clone);
                append_bounded_tail(&mut tail, buffer.as_bytes(), MAX_AGENT_TAIL_BYTES);
                bytes_captured_clone.fetch_add(read as u64, Ordering::Relaxed);
                if let Ok(mut last_update) = last_update_clone.lock() {
                    *last_update = Some(SystemTime::now());
                }
            }
            Ok(tail)
        });
        Self {
            output_path,
            bytes_captured,
            last_update,
            touched_paths,
            join_handle,
        }
    }

    fn finish(self, label: &str) -> Result<(Vec<u8>, HashSet<PathBuf>, PathBuf)> {
        let output_path = self.output_path;
        let touched_paths = self
            .touched_paths
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        match self.join_handle.join() {
            Ok(result) => result
                .with_context(|| format!("Failed to capture agent {label} stream"))
                .map(|tail| (tail, touched_paths, output_path)),
            Err(_) => bail!("Agent {label} capture thread panicked"),
        }
    }
}

fn append_bounded_tail(buffer: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    buffer.extend_from_slice(chunk);
    if buffer.len() > limit {
        let trim = buffer.len() - limit;
        buffer.drain(..trim);
    }
}

fn update_touched_paths_from_line(
    line: &str,
    candidate_paths: &[PathBuf],
    touched_paths: &Arc<Mutex<HashSet<PathBuf>>>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if !looks_like_audit_event(&value) {
        return;
    }

    let mut strings = Vec::new();
    collect_all_strings(&value, &mut strings, 0);
    let haystack = strings.join("\n");
    let normalized_haystack = normalize_path_like_text(&haystack);
    if let Ok(mut touched) = touched_paths.lock() {
        for path in candidate_paths {
            if path_search_variants(path)
                .iter()
                .any(|variant| haystack.contains(variant) || normalized_haystack.contains(variant))
            {
                touched.insert(path.clone());
            }
        }
    }
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
struct RawWorkflowResponse {
    inspected_files: Vec<String>,
    session_findings: Vec<RawSessionWorkflowFinding>,
}

#[derive(Deserialize)]
struct RawSessionWorkflowFinding {
    session: String,
    summary: String,
    candidates: Vec<RawWorkflowCandidate>,
}

#[derive(Deserialize)]
struct RawWorkflowCandidate {
    workflow_key: String,
    workflow_label: Option<String>,
    note: String,
    start_event: usize,
    end_event: usize,
}

fn sort_sessions_newest_first(sessions: &mut [Session]) {
    sessions.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| left.path.cmp(&right.path))
    });
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
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("Failed to create file {}", path.display()));
            }
        }
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

fn agent_timeout_from_env(raw: Option<&str>) -> Result<Option<Duration>> {
    match raw {
        Some(raw) => {
            let secs: u64 = raw.parse().with_context(|| {
                format!(
                    "Failed to parse DISTILL_AGENT_TIMEOUT_SECS={raw:?} as an integer number of seconds"
                )
            })?;
            if secs == 0 {
                return Ok(None);
            }
            Ok(Some(Duration::from_secs(secs)))
        }
        None => Ok(Some(Duration::from_secs(DEFAULT_AGENT_TIMEOUT_SECS))),
    }
}

fn agent_timeout() -> Result<Option<Duration>> {
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

fn scan_max_raw_bytes() -> Result<Option<u64>> {
    match std::env::var("DISTILL_SCAN_MAX_RAW_BYTES") {
        Ok(raw) => {
            let max_raw_bytes: u64 = raw.parse().with_context(|| {
                format!(
                    "Failed to parse DISTILL_SCAN_MAX_RAW_BYTES={raw:?} as a non-negative integer"
                )
            })?;
            if max_raw_bytes == 0 {
                Ok(None)
            } else {
                Ok(Some(max_raw_bytes))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(Some(DEFAULT_SCAN_MAX_RAW_BYTES)),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("DISTILL_SCAN_MAX_RAW_BYTES must be valid Unicode.")
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

fn scan_debug_artifacts() -> Result<ScanDebugArtifacts> {
    let explicit_dir = scan_debug_dir()?;
    let cleanup_on_success = explicit_dir.is_none();
    let base_dir = explicit_dir.unwrap_or_else(|| Config::base_dir().join("scan-debug"));
    ScanDebugArtifacts::new(base_dir, cleanup_on_success)
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

fn format_optional_system_time(value: Option<SystemTime>) -> Option<String> {
    value.map(|time| DateTime::<Utc>::from(time).to_rfc3339())
}

pub fn run_scan(agents: &[Box<dyn Agent>], scan_config: &ScanConfig) -> Result<Vec<Proposal>> {
    let scan_started_at = Utc::now();
    let debug_artifacts = scan_debug_artifacts()?;
    let debug_run_dir = Some(debug_artifacts.run_dir.as_path());
    println!(
        "Live scan debug directory: {}",
        debug_artifacts.run_dir.display()
    );

    let result = (|| -> Result<Vec<Proposal>> {
        let mut phase_durations = ScanPhaseDurations::default();
        let discovery_started = Instant::now();
        let last_scan = LastScan::load(&scan_config.last_scan_path)?;
        let discovery_since = last_scan
            .as_ref()
            .map(|last_scan| last_scan.timestamp)
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
        let batch_size = scan_batch_size()?;
        let max_raw_bytes = scan_max_raw_bytes()?;
        let timeout = agent_timeout()?;

        let collected_sessions = reader::collect_sessions(agents, discovery_since)?;
        let collected_count = collected_sessions.len();
        let candidate_sessions =
            filter_low_signal_sessions(filter_distill_scan_artifacts(collected_sessions));
        let candidate_count = candidate_sessions.len();
        let skipped_internal = collected_count.saturating_sub(candidate_count);

        let mut backlog = ScanBacklog::load(&scan_config.backlog_path)?;
        let seed_newest_first = last_scan.is_none() && backlog.sessions.is_empty();
        backlog.merge_new_sessions(candidate_sessions, seed_newest_first);
        backlog.save(&scan_config.backlog_path)?;
        phase_durations.discovery = discovery_started.elapsed().as_millis() as u64;

        if skipped_internal > 0 {
            println!(
                "Skipped {} low-signal/internal session(s).",
                skipped_internal
            );
        }

        if backlog.sessions.is_empty() {
            println!("No pending sessions found for scan.");
            debug_artifacts.write_status(&ScanRunStatus {
                state: "completed".to_string(),
                started_at: scan_started_at.to_rfc3339(),
                updated_at: Utc::now().to_rfc3339(),
                phase: "idle".to_string(),
                scan_pid: std::process::id(),
                agent_pid: None,
                agent_command: scan_config.agent_command.clone(),
                workspace_root: debug_artifacts.run_dir.display().to_string(),
                batch_size: 0,
                selected_raw_bytes: 0,
                staged_bytes: 0,
                discovered_sessions: collected_count,
                candidate_sessions: candidate_count,
                skipped_sessions: skipped_internal,
                backlog_sessions: 0,
                ready_workflows: 0,
                proposals_written: 0,
                prompt_bytes: 0,
                timeout_secs: timeout.map(|value| value.as_secs()),
                stdout_bytes: 0,
                stderr_bytes: 0,
                last_stdout_at: None,
                last_stderr_at: None,
                durations_ms: phase_durations,
                note: Some("No pending sessions found for scan.".to_string()),
            });
            let watermark = LastScan {
                timestamp: scan_started_at,
                session_ids: vec![],
            };
            watermark.save(&scan_config.last_scan_path)?;
            return Ok(vec![]);
        }

        println!("Found {} session(s) to analyze.", backlog.sessions.len());
        println!("Pending scan backlog: {}", backlog.sessions.len());

        let selection_started = Instant::now();
        let batch = select_session_batch(&backlog.sessions, batch_size, max_raw_bytes)?;
        phase_durations.selection = selection_started.elapsed().as_millis() as u64;
        let selected_raw_bytes = batch.iter().map(|session| session.raw_bytes).sum::<u64>();
        if batch.len() < backlog.sessions.len() {
            println!(
                "Capped this scan to {} pending session(s) totaling {} bytes; future scheduled runs will continue draining the backlog automatically, or rerun `distill scan --now` to speed it up.",
                batch.len(),
                selected_raw_bytes
            );
        }

        let skill_sources = load_existing_skill_sources(&scan_config.skill_dirs)?;
        println!("Loaded {} existing skill(s).", skill_sources.len());

        let preferences = match PreferenceProfile::load(&scan_config.history_dir) {
            Ok(profile) => profile,
            Err(err) => {
                eprintln!(
                    "Warning: failed to load preference history (continuing without it): {err}"
                );
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

        let staging_started = Instant::now();
        let (workspace, staged_bytes) =
            stage_timeline_workspace(&batch, &skill_sources, debug_run_dir)?;
        phase_durations.staging = staging_started.elapsed().as_millis() as u64;
        let prompt = build_workflow_detection_prompt(&workspace.manifest, &preferences);
        write_debug_text(debug_run_dir, "workflow-detection-prompt.txt", &prompt);

        println!(
            "Inspecting {} staged session timeline file(s) with `{}` (prompt: {} bytes)...",
            batch.len(),
            scan_config.agent_command,
            prompt.len()
        );
        println!("Waiting for workflow detection response...");

        let detection_started = Instant::now();
        let invocation = invoke_agent(
            &scan_config.agent_command,
            &scan_config.agent_args,
            &prompt,
            &workspace.root,
            AgentRunContext {
                phase: "detection_agent",
                timeout,
                debug_run_dir,
                debug_artifacts: Some(&debug_artifacts),
                batch_size: batch.len(),
                selected_raw_bytes,
                staged_bytes,
                discovered_sessions: collected_count,
                candidate_sessions: candidate_count,
                skipped_sessions: skipped_internal,
                backlog_sessions: backlog.sessions.len(),
                ready_workflows: 0,
                proposals_written: 0,
                durations: phase_durations.clone(),
                output_schema: Some(WORKFLOW_DETECTION_SCHEMA),
                candidate_paths: workspace
                    .staged_sessions
                    .iter()
                    .map(|session| canonicalize_path(&session.staged_path))
                    .collect(),
            },
        )?;
        phase_durations.detection_agent = detection_started.elapsed().as_millis() as u64;
        println!("Agent responded ({} bytes).", invocation.final_output.len());

        let parsed_workflow = parse_workflow_response(&invocation.final_output, &workspace.root);
        let mut scan_state = ScanState::load(&scan_config.state_path)?;
        let mut written_proposals = Vec::new();

        if let Ok(parsed_workflow) = parsed_workflow {
            validate_workflow_response(&parsed_workflow, &workspace, &invocation)?;
            write_debug_text(
                debug_run_dir,
                "parsed-workflow-response.json",
                &serde_json::to_string_pretty(&serde_json::json!({
                    "inspected_files": parsed_workflow
                        .inspected_files
                        .iter()
                        .map(|path| path.to_string_lossy().to_string())
                        .collect::<Vec<_>>(),
                    "session_findings": parsed_workflow
                        .session_findings
                        .iter()
                        .map(|finding| serde_json::json!({
                            "session": finding.session.to_string_lossy().to_string(),
                            "summary": finding.summary,
                            "candidates": finding.candidates,
                        }))
                        .collect::<Vec<_>>(),
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            );

            let batch_by_original = batch
                .iter()
                .map(|descriptor| {
                    (
                        canonicalize_path(&descriptor.session.path),
                        descriptor.clone(),
                    )
                })
                .collect::<HashMap<_, _>>();
            let staged_to_original = workspace
                .staged_sessions
                .iter()
                .map(|session| {
                    (
                        canonicalize_path(&session.staged_path),
                        canonicalize_path(&session.source_session.path),
                    )
                })
                .collect::<HashMap<_, _>>();

            let mut detected_candidates = 0usize;
            for finding in &parsed_workflow.session_findings {
                detected_candidates += finding.candidates.len();
                let Some(original_path) = staged_to_original.get(&finding.session) else {
                    bail!(
                        "Failed to map staged workflow finding {} back to original session path",
                        finding.session.display()
                    );
                };
                let Some(descriptor) = batch_by_original.get(original_path) else {
                    bail!(
                        "Failed to locate descriptor for workflow finding {}",
                        original_path.display()
                    );
                };
                scan_state.record_session_findings(
                    descriptor,
                    finding.candidates.clone(),
                    Utc::now(),
                );
            }
            println!(
                "Stored {} workflow candidate span(s) across {} session(s).",
                detected_candidates,
                parsed_workflow.session_findings.len()
            );

            let ready_workflows = scan_state.ready_workflows(MIN_WORKFLOW_MATCHES_FOR_PROPOSAL);
            if !ready_workflows.is_empty() {
                println!(
                    "{} workflow group(s) reached the proposal threshold.",
                    ready_workflows.len()
                );
            }

            for workflow in ready_workflows {
                println!(
                    "Running proposal pass for workflow `{}` across {} session(s).",
                    workflow.workflow_key,
                    workflow.matches.len()
                );
                let workflow_raw_bytes = workflow
                    .matches
                    .iter()
                    .filter_map(|item| {
                        let agent = AgentKind::from_name(&item.agent)?;
                        discover_session(&Session {
                            id: item.session_id.clone(),
                            agent,
                            path: item.session_path.clone(),
                            timestamp: item.timestamp,
                            content: String::new(),
                        })
                        .ok()
                        .map(|descriptor| descriptor.raw_bytes)
                    })
                    .sum::<u64>();

                let workflow_staging_started = Instant::now();
                let (workflow_workspace, workflow_staged_bytes) =
                    stage_workflow_workspace(&workflow, &skill_sources, debug_run_dir)?;
                phase_durations.staging = phase_durations
                    .staging
                    .saturating_add(workflow_staging_started.elapsed().as_millis() as u64);
                let workflow_prompt = build_workflow_proposal_prompt(
                    &workflow_workspace.manifest,
                    &preferences,
                    &workflow,
                );
                write_debug_text(
                    debug_run_dir,
                    &format!(
                        "workflow-proposal-prompt-{}.txt",
                        sanitize_filename(&workflow.workflow_key)
                    ),
                    &workflow_prompt,
                );

                let proposal_started = Instant::now();
                let proposal_invocation = invoke_agent(
                    &scan_config.agent_command,
                    &scan_config.agent_args,
                    &workflow_prompt,
                    &workflow_workspace.root,
                    AgentRunContext {
                        phase: "proposal_agent",
                        timeout,
                        debug_run_dir,
                        debug_artifacts: Some(&debug_artifacts),
                        batch_size: workflow.matches.len(),
                        selected_raw_bytes: workflow_raw_bytes,
                        staged_bytes: workflow_staged_bytes,
                        discovered_sessions: collected_count,
                        candidate_sessions: candidate_count,
                        skipped_sessions: skipped_internal,
                        backlog_sessions: backlog.sessions.len(),
                        ready_workflows: 1,
                        proposals_written: written_proposals.len(),
                        durations: phase_durations.clone(),
                        output_schema: Some(PROPOSAL_SCHEMA),
                        candidate_paths: workflow_workspace
                            .staged_sessions
                            .iter()
                            .map(|session| canonicalize_path(&session.staged_path))
                            .collect(),
                    },
                )?;
                phase_durations.proposal_agent = phase_durations
                    .proposal_agent
                    .saturating_add(proposal_started.elapsed().as_millis() as u64);

                let parsed = parse_scan_response(
                    &proposal_invocation.final_output,
                    &workflow_workspace.root,
                )?;
                write_debug_text(
                    debug_run_dir,
                    &format!(
                        "parsed-proposal-response-{}.json",
                        sanitize_filename(&workflow.workflow_key)
                    ),
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

                let proposals = validate_and_finalize_response(
                    &parsed,
                    &workflow_workspace,
                    &proposal_invocation,
                )?;
                scan_state.mark_workflow_attempted(
                    &workflow.workflow_key,
                    workflow.matches.len(),
                    Utc::now(),
                );
                if !proposals.is_empty() {
                    scan_state.mark_workflow_proposed(
                        &workflow.workflow_key,
                        workflow.matches.len(),
                        Utc::now(),
                    );
                }
                write_proposals(
                    &scan_config.proposals_dir,
                    &mut written_proposals,
                    proposals,
                )?;
            }
        } else {
            let parsed = parse_scan_response(&invocation.final_output, &workspace.root)?;
            write_debug_text(
                debug_run_dir,
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
            let proposals = validate_and_finalize_response(&parsed, &workspace, &invocation)?;
            write_proposals(
                &scan_config.proposals_dir,
                &mut written_proposals,
                proposals,
            )?;
        }

        let finalize_started = Instant::now();
        let batch_sessions = batch
            .iter()
            .map(|descriptor| descriptor.session.clone())
            .collect::<Vec<_>>();
        backlog.remove_batch(&batch_sessions);
        backlog.save(&scan_config.backlog_path)?;
        scan_state.save(&scan_config.state_path)?;
        let watermark = LastScan {
            timestamp: scan_started_at,
            session_ids: vec![],
        };
        watermark.save(&scan_config.last_scan_path)?;
        phase_durations.finalize = finalize_started.elapsed().as_millis() as u64;

        debug_artifacts.write_status(&ScanRunStatus {
            state: "completed".to_string(),
            started_at: scan_started_at.to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
            phase: "finalize".to_string(),
            scan_pid: std::process::id(),
            agent_pid: None,
            agent_command: scan_config.agent_command.clone(),
            workspace_root: workspace.root.display().to_string(),
            batch_size: batch.len(),
            selected_raw_bytes,
            staged_bytes,
            discovered_sessions: collected_count,
            candidate_sessions: candidate_count,
            skipped_sessions: skipped_internal,
            backlog_sessions: backlog.sessions.len(),
            ready_workflows: scan_state
                .ready_workflows(MIN_WORKFLOW_MATCHES_FOR_PROPOSAL)
                .len(),
            proposals_written: written_proposals.len(),
            prompt_bytes: 0,
            timeout_secs: timeout.map(|value| value.as_secs()),
            stdout_bytes: 0,
            stderr_bytes: 0,
            last_stdout_at: None,
            last_stderr_at: None,
            durations_ms: phase_durations,
            note: Some("Scan completed successfully.".to_string()),
        });

        println!("Agent proposed {} skill(s).", written_proposals.len());
        Ok(written_proposals)
    })();

    match &result {
        Ok(_) => debug_artifacts.finish_success(),
        Err(err) => debug_artifacts.finish_failure(err),
    }

    result
}

fn write_proposals(
    proposals_dir: &Path,
    written_proposals: &mut Vec<Proposal>,
    mut proposals: Vec<Proposal>,
) -> Result<()> {
    if proposals.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(proposals_dir)?;
    let existing_count = written_proposals.len();
    for (index, proposal) in proposals.iter_mut().enumerate() {
        let filename = proposal_filename(proposal, existing_count + index);
        let path = proposals_dir.join(&filename);
        let markdown = proposal
            .to_markdown()
            .context("Failed to serialize proposal to markdown")?;
        std::fs::write(&path, markdown)
            .with_context(|| format!("Failed to write proposal {}", path.display()))?;
        proposal.filename = Some(filename);
    }
    written_proposals.extend(proposals);
    Ok(())
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

fn select_session_batch(
    backlog: &[Session],
    batch_size: usize,
    max_raw_bytes: Option<u64>,
) -> Result<Vec<SessionDescriptor>> {
    let mut selected = Vec::new();
    let mut selected_bytes = 0u64;

    for session in backlog {
        if selected.len() >= batch_size {
            break;
        }

        let descriptor = discover_session(session)?;
        let would_exceed = max_raw_bytes
            .map(|limit| !selected.is_empty() && selected_bytes + descriptor.raw_bytes > limit)
            .unwrap_or(false);
        if would_exceed {
            break;
        }

        selected_bytes = selected_bytes.saturating_add(descriptor.raw_bytes);
        selected.push(descriptor);
    }

    Ok(selected)
}

fn stage_timeline_workspace(
    batch: &[SessionDescriptor],
    skill_sources: &[SkillSource],
    debug_run_dir: Option<&Path>,
) -> Result<(StagedWorkspace, u64)> {
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
    let mut staged_bytes = 0u64;

    for (index, descriptor) in batch.iter().enumerate() {
        let agent_dir = sessions_root.join(descriptor.session.agent.to_string());
        std::fs::create_dir_all(&agent_dir)?;
        session_roots.insert(
            descriptor.session.agent.to_string(),
            agent_dir.to_string_lossy().to_string(),
        );

        let basename = descriptor
            .session
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session.jsonl");
        let staged_path = agent_dir.join(format!("{:04}-{}", index + 1, basename));
        let timeline = build_session_timeline(descriptor)?;
        let rendered = render_timeline(&timeline);
        staged_bytes = staged_bytes.saturating_add(rendered.len() as u64);
        std::fs::write(&staged_path, rendered).with_context(|| {
            format!(
                "Failed to stage timeline {} from {}",
                staged_path.display(),
                descriptor.session.path.display()
            )
        })?;

        staged_sessions.push(StagedSession {
            source_session: descriptor.session.clone(),
            staged_path: staged_path.clone(),
        });
        manifest_sessions.push(ManifestSession {
            agent: descriptor.session.agent.to_string(),
            session_id: descriptor.session.id.clone(),
            timestamp: descriptor.session.timestamp.to_rfc3339(),
            original_path: descriptor.session.path.to_string_lossy().to_string(),
            staged_path: staged_path.to_string_lossy().to_string(),
        });
    }

    let (staged_skills, manifest_skills) = stage_skill_files(skill_sources, &skills_root)?;
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

    Ok((
        StagedWorkspace {
            root,
            manifest,
            staged_sessions,
            staged_skills,
            cleanup_on_drop,
        },
        staged_bytes,
    ))
}

fn stage_workflow_workspace(
    workflow: &ReadyWorkflow,
    skill_sources: &[SkillSource],
    debug_run_dir: Option<&Path>,
) -> Result<(StagedWorkspace, u64)> {
    let (root, cleanup_on_drop) = if let Some(run_dir) = debug_run_dir {
        let root = run_dir.join(format!(
            "workflow-{}",
            sanitize_filename(&workflow.workflow_key)
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        (root, false)
    } else {
        (
            create_temp_dir_path(&format!(
                "distill-workflow-{}",
                sanitize_filename(&workflow.workflow_key)
            ))?,
            true,
        )
    };

    let sessions_root = root.join("sessions");
    let skills_root = root.join("skills");
    std::fs::create_dir_all(&sessions_root)?;
    std::fs::create_dir_all(&skills_root)?;

    let mut session_roots = BTreeMap::new();
    let mut staged_sessions = Vec::new();
    let mut manifest_sessions = Vec::new();
    let mut staged_bytes = 0u64;

    for (index, workflow_match) in workflow.matches.iter().enumerate() {
        let Some(agent) = AgentKind::from_name(&workflow_match.agent) else {
            bail!("Unknown agent in scan-state.json: {}", workflow_match.agent);
        };
        let session = Session {
            id: workflow_match.session_id.clone(),
            agent,
            path: workflow_match.session_path.clone(),
            timestamp: workflow_match.timestamp,
            content: String::new(),
        };
        let descriptor = discover_session(&session)?;
        let agent_dir = sessions_root.join(descriptor.session.agent.to_string());
        std::fs::create_dir_all(&agent_dir)?;
        session_roots.insert(
            descriptor.session.agent.to_string(),
            agent_dir.to_string_lossy().to_string(),
        );

        let basename = descriptor
            .session
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session.jsonl");
        let staged_path = agent_dir.join(format!("{:04}-{}", index + 1, basename));
        let rendered = render_timeline_window(
            &descriptor,
            &TimelineWindowRequest {
                workflow_key: workflow.workflow_key.clone(),
                workflow_label: workflow_match.finding.workflow_label.clone(),
                note: workflow_match.finding.note.clone(),
                start_event: workflow_match.finding.start_event,
                end_event: workflow_match.finding.end_event,
            },
        )?;
        staged_bytes = staged_bytes.saturating_add(rendered.len() as u64);
        std::fs::write(&staged_path, rendered).with_context(|| {
            format!(
                "Failed to stage workflow window {} from {}",
                staged_path.display(),
                descriptor.session.path.display()
            )
        })?;

        staged_sessions.push(StagedSession {
            source_session: descriptor.session.clone(),
            staged_path: staged_path.clone(),
        });
        manifest_sessions.push(ManifestSession {
            agent: descriptor.session.agent.to_string(),
            session_id: descriptor.session.id.clone(),
            timestamp: descriptor.session.timestamp.to_rfc3339(),
            original_path: descriptor.session.path.to_string_lossy().to_string(),
            staged_path: staged_path.to_string_lossy().to_string(),
        });
    }

    let (staged_skills, manifest_skills) = stage_skill_files(skill_sources, &skills_root)?;
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

    Ok((
        StagedWorkspace {
            root,
            manifest,
            staged_sessions,
            staged_skills,
            cleanup_on_drop,
        },
        staged_bytes,
    ))
}

fn stage_skill_files(
    skill_sources: &[SkillSource],
    skills_root: &Path,
) -> Result<(Vec<StagedSkill>, Vec<ManifestSkill>)> {
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
    Ok((staged_skills, manifest_skills))
}

#[cfg(test)]
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
        stage_session_file_for_scan(session, &staged_path)?;

        staged_sessions.push(StagedSession {
            source_session: session.clone(),
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

#[cfg(test)]
fn stage_session_file_for_scan(session: &Session, destination: &Path) -> Result<()> {
    let raw = crate::agents::read_session_source(session)?;
    let summary = build_staged_session_summary(session, &raw);
    std::fs::write(destination, summary).with_context(|| {
        format!(
            "Failed to write staged session {} from {}",
            destination.display(),
            session.path.display()
        )
    })
}

#[cfg(test)]
#[derive(Default)]
struct SessionDigest {
    user_messages: Vec<String>,
    assistant_messages: Vec<String>,
    tool_calls: Vec<String>,
}

#[cfg(test)]
fn build_staged_session_summary(session: &Session, raw: &str) -> String {
    let digest = extract_session_digest(session.agent, raw);
    let mut lines = vec![
        "# Staged Session Summary".to_string(),
        format!("Agent: {}", session.agent),
        format!("Timestamp: {}", session.timestamp.to_rfc3339()),
        format!("Original path: {}", session.path.display()),
        "".to_string(),
        "This file is a compact Distill summary for skill extraction. It intentionally omits large internal prompts, verbose tool output, and encrypted payloads from the raw session log.".to_string(),
        "".to_string(),
        "## Latest User Requests".to_string(),
    ];

    append_summary_section(
        &mut lines,
        &digest.user_messages,
        "No user messages extracted.",
    );

    lines.push(String::new());
    lines.push("## Latest Assistant Outcomes".to_string());
    append_summary_section(
        &mut lines,
        &digest.assistant_messages,
        "No assistant outcomes extracted.",
    );

    lines.push(String::new());
    lines.push("## Tool Calls".to_string());
    if digest.tool_calls.is_empty() {
        lines.push("- None extracted.".to_string());
    } else {
        let mut counts = BTreeMap::new();
        for tool in &digest.tool_calls {
            *counts.entry(tool.clone()).or_insert(0usize) += 1;
        }
        for (tool, count) in counts {
            lines.push(format!("- {tool} x{count}"));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
fn append_summary_section(lines: &mut Vec<String>, items: &[String], empty_message: &str) {
    let mut recent = items
        .iter()
        .rev()
        .filter_map(|item| {
            let excerpt = summarize_session_excerpt(item);
            if excerpt.is_empty() {
                None
            } else {
                Some(excerpt)
            }
        })
        .take(4)
        .collect::<Vec<_>>();
    recent.reverse();

    if recent.is_empty() {
        lines.push(format!("- {empty_message}"));
        return;
    }

    for item in recent {
        lines.push(format!("- {item}"));
    }
}

#[cfg(test)]
fn extract_session_digest(agent: crate::agents::AgentKind, raw: &str) -> SessionDigest {
    if agent == crate::agents::AgentKind::OpenCode {
        return extract_opencode_session_digest(raw);
    }

    let mut digest = SessionDigest::default();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let event_type = value.get("type").and_then(|item| item.as_str());
        let payload = value.get("payload");

        match event_type {
            Some("response_item") => {
                let Some(payload) = payload else {
                    continue;
                };
                match payload.get("type").and_then(|item| item.as_str()) {
                    Some("message") => {
                        let text = extract_response_message_text(payload);
                        match payload.get("role").and_then(|item| item.as_str()) {
                            Some("user") => digest.user_messages.push(text),
                            Some("assistant") => digest.assistant_messages.push(text),
                            _ => {}
                        }
                    }
                    Some("function_call") => {
                        if let Some(name) = payload.get("name").and_then(|item| item.as_str()) {
                            digest.tool_calls.push(name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            Some("event_msg") => {
                let Some(payload) = payload else {
                    continue;
                };
                match payload.get("type").and_then(|item| item.as_str()) {
                    Some("user_message") => {
                        if let Some(message) = payload.get("message").and_then(|item| item.as_str())
                        {
                            digest.user_messages.push(message.to_string());
                        }
                    }
                    Some("agent_message") => {
                        if let Some(message) = payload.get("message").and_then(|item| item.as_str())
                        {
                            digest.assistant_messages.push(message.to_string());
                        }
                    }
                    Some("task_complete") => {
                        if let Some(message) = payload
                            .get("last_agent_message")
                            .and_then(|item| item.as_str())
                        {
                            digest.assistant_messages.push(message.to_string());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    digest
}

#[cfg(test)]
fn extract_opencode_session_digest(raw: &str) -> SessionDigest {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return SessionDigest::default();
    };

    let mut digest = SessionDigest::default();
    collect_opencode_digest(&value, &mut digest, 0);
    digest
}

#[cfg(test)]
fn collect_opencode_digest(value: &serde_json::Value, digest: &mut SessionDigest, depth: usize) {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }

    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_opencode_digest(item, digest, depth + 1);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(role) = map
                .get("role")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    map.get("info")
                        .and_then(|value| value.get("role"))
                        .and_then(|value| value.as_str())
                })
            {
                let text = extract_opencode_message_text(
                    map.get("content")
                        .or_else(|| map.get("parts"))
                        .or_else(|| map.get("text"))
                        .or_else(|| map.get("message"))
                        .unwrap_or(value),
                );
                if !text.is_empty() {
                    match role {
                        "user" => digest.user_messages.push(text),
                        "assistant" => digest.assistant_messages.push(text),
                        _ => {}
                    }
                }
            }

            if let Some(tool_name) = extract_opencode_tool_name(map) {
                digest.tool_calls.push(tool_name);
            }

            for item in map.values() {
                collect_opencode_digest(item, digest, depth + 1);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn extract_opencode_tool_name(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let type_name = map
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if !type_name.to_ascii_lowercase().contains("tool")
        && !map.contains_key("tool")
        && !map.contains_key("name")
    {
        return None;
    }

    map.get("tool")
        .and_then(|value| value.as_str())
        .or_else(|| map.get("name").and_then(|value| value.as_str()))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
fn extract_opencode_message_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(extract_opencode_message_text)
            .filter(|item| !item.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => {
            let mut parts = Vec::new();
            for key in [
                "text", "message", "content", "parts", "summary", "input", "output",
            ] {
                if let Some(item) = map.get(key) {
                    let text = extract_opencode_message_text(item);
                    if !text.trim().is_empty() {
                        parts.push(text);
                    }
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
fn extract_response_message_text(payload: &serde_json::Value) -> String {
    payload
        .get("content")
        .and_then(|item| item.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
fn summarize_session_excerpt(input: &str) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_STAGED_SUMMARY_EXCERPT_CHARS {
        return normalized;
    }

    let head_len = MAX_STAGED_SUMMARY_EXCERPT_CHARS.saturating_sub(40);
    let head: String = normalized.chars().take(head_len).collect();
    let omitted = normalized.chars().count().saturating_sub(head_len);
    format!("{head} [... omitted {omitted} chars ...]")
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

fn build_workflow_detection_prompt(
    manifest: &ScanManifest,
    preferences: &PreferenceProfile,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are a workflow detection engine for the `distill` tool.\n\n\
         Your job: inspect each staged session timeline file and mark reusable workflow spans that could become skills later.\n\n\
         Each staged session file is already a compact ordered timeline. It keeps meaningful events from the full session, including middle-of-thread commands and tool calls.\n\
         Read every listed session file once. Do not search the workspace. Do not re-parse raw JSONL logs. Do not modify files.\n\
         If you use commands at all, restrict them to direct single-file reads of staged session timelines or staged skill files.\n\
         Do not use Python, jq, shell scripts, or helper programs to assemble JSON.\n\
         Do not make network requests.\n\n\
         Detection rules:\n\
         - Focus on repeated workflows, not topics or projects\n\
         - Ignore project names, ticket ids, and product-specific nouns when naming a workflow\n\
         - A workflow can appear in the middle of a long session; do not bias toward the start or end\n\
         - It is acceptable to return zero candidates for a session if nothing looks reusable\n\
         - Use stable kebab-case for `workflow_key`\n\
         - `start_event` and `end_event` must refer to event numbers from the staged timeline file\n\
         - Prefer 0-2 strong candidates per session; do not invent weak ones\n\n\
         IMPORTANT: Respond ONLY with valid JSON in this exact shape:\n\
         {\"inspected_files\": [...], \"session_findings\": [...]}.\n\
         No markdown fences. No commentary.\n\n\
         Response requirements:\n\
         - `inspected_files`: every candidate session file path exactly once\n\
         - `session_findings`: one object per candidate session file\n\
         - each `session_findings` object must include:\n\
           - `session`: exact staged session path\n\
           - `summary`: one short sentence about the session's main work\n\
           - `candidates`: array of repeated workflow spans, possibly empty\n\
         - each candidate in `candidates` must include:\n\
           - `workflow_key`: stable kebab-case workflow identifier\n\
           - `workflow_label`: short human label or null\n\
           - `note`: why this span looks reusable\n\
           - `start_event`: first event number in the workflow span\n\
           - `end_event`: last event number in the workflow span\n\n",
    );

    prompt.push_str(&format!(
        "## Workspace\n\n- Workspace root: {}\n- Manifest: {}\n\n",
        manifest.workspace_root, manifest.manifest_path
    ));

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
        "Inspect every candidate session file listed above before answering. Then return the JSON object with complete file coverage.\n",
    );
    prompt
}

fn build_workflow_proposal_prompt(
    manifest: &ScanManifest,
    preferences: &PreferenceProfile,
    workflow: &ReadyWorkflow,
) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are a skill proposal engine for the `distill` tool.\n\n\
         Your job: inspect staged workflow-window files for one repeated workflow group and decide whether they justify a reusable skill proposal.\n\n\
         Each staged session file is a focused window cut from the original session around a repeated workflow span. Read every listed file once.\n\
         Do not search the workspace. Do not re-parse raw JSONL logs. Do not modify files.\n\
         If you use commands at all, restrict them to direct single-file reads of staged workflow windows or staged skill files.\n\
         Do not use Python, jq, shell scripts, or helper programs to assemble JSON.\n\
         Do not make network requests.\n\n\
         Output quality bar:\n\
         - Propose only if the workflow is truly reusable across sessions\n\
         - Prefer `improve`/`edit` when an existing skill already overlaps\n\
         - If evidence is still weak, return an empty proposals array, but still cover every file\n\
         - Every proposal body must be concrete and actionable\n\n\
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
         - \"target_skill\": string containing the canonical skill name in kebab-case; for `new`, set it to the new skill's name, and for `improve`/`edit`/`remove`, set it to the existing skill name\n\
         - \"evidence\": array of {\"session\": \"<staged path>\", \"pattern\": \"<description>\"}\n\
         - \"body\": string containing the full proposed skill content in markdown\n\n\
         For each proposal body, use this markdown structure:\n\
         - `# <Skill Name>`\n\
         - `## When to use`\n\
         - `## Steps`\n\
         - `## Verification`\n\
         - `## Pitfalls`\n\
         For `improve` and `edit` proposals targeting an existing skill:\n\
         - treat the proposal body as the full replacement for that skill's `SKILL.md`\n\
         - preserve any existing YAML frontmatter unless the evidence explicitly requires changing it\n\
         - make the smallest complete-file update that fixes the evidence; do not drop unrelated sections or command arguments\n\n",
    );

    prompt.push_str(&format!(
        "## Workflow Group\n\n- workflow_key: {}\n- workflow_label: {}\n\n",
        workflow.workflow_key,
        workflow
            .workflow_label
            .as_deref()
            .unwrap_or("(not provided)")
    ));
    prompt.push_str(&format!(
        "## Workspace\n\n- Workspace root: {}\n- Manifest: {}\n\n",
        manifest.workspace_root, manifest.manifest_path
    ));

    prompt.push_str("## Candidate Workflow Windows\n\n");
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
        "Inspect every candidate workflow window file listed above before answering. Then return the JSON object with complete coverage and any high-signal proposals.\n",
    );
    prompt
}

#[cfg(test)]
fn build_prompt(manifest: &ScanManifest, preferences: &PreferenceProfile) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are a skill extraction engine for the `distill` tool.\n\n\
         Your job: inspect staged AI agent session files and propose reusable skills.\n\n\
         You may inspect staged files, but keep that inspection minimal.\n\
         If you use commands at all, restrict them to direct single-file reads of staged session summaries or staged skill files.\n\
         Do not run searches across the workspace.\n\
         Do not use Python, node, jq, perl, awk, shell scripts, or helper programs to analyze data or build JSON.\n\
         Do not make network requests.\n\
         Synthesize the final JSON directly in your response instead of generating it with tools.\n\
         Do not modify, create, delete, or rename any files.\n\n\
         Output quality bar:\n\
         - Propose only repeated, reusable workflows (not one-off tasks)\n\
         - Prefer `improve`/`edit` when an existing skill already overlaps\n\
         - If evidence is weak, return an empty proposals array, but still cover every session file\n\
         - Every proposal body must be concrete and actionable (no placeholders)\n\n\
         Staged session files are already compact text summaries prepared by Distill.\n\
         Read them directly; do not re-parse them as raw JSONL logs.\n\
         After you have read the listed files once, stop inspecting and answer.\n\n\
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
         - \"target_skill\": string containing the canonical skill name in kebab-case; for `new`, set it to the new skill's name, and for `improve`/`edit`/`remove`, set it to the existing skill name\n\
         - \"evidence\": array of {\"session\": \"<staged path>\", \"pattern\": \"<description>\"}\n\
         - \"body\": string containing the full proposed skill content in markdown\n\n\
         For each proposal body, use this markdown structure:\n\
         - `# <Skill Name>`\n\
         - `## When to use`\n\
         - `## Steps`\n\
         - `## Verification`\n\
         - `## Pitfalls`\n\
         For `improve` and `edit` proposals targeting an existing skill:\n\
         - treat the proposal body as the full replacement for that skill's `SKILL.md`\n\
         - preserve any existing YAML frontmatter unless the evidence explicitly requires changing it\n\
         - make the smallest complete-file update that fixes the evidence; do not drop unrelated sections or command arguments\n\n",
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

fn write_live_scan_status(debug_artifacts: &ScanDebugArtifacts, update: LiveScanStatusUpdate<'_>) {
    let mut agent_command = update.command.to_string();
    if !update.args.is_empty() {
        agent_command.push(' ');
        agent_command.push_str(&update.args.join(" "));
    }

    debug_artifacts.write_status(&ScanRunStatus {
        state: update.state.to_string(),
        started_at: update.started_at.to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
        phase: update.phase.to_string(),
        scan_pid: std::process::id(),
        agent_pid: update.agent_pid,
        agent_command,
        workspace_root: update.workspace_root.display().to_string(),
        batch_size: update.batch_size,
        selected_raw_bytes: update.selected_raw_bytes,
        staged_bytes: update.staged_bytes,
        discovered_sessions: update.discovered_sessions,
        candidate_sessions: update.candidate_sessions,
        skipped_sessions: update.skipped_sessions,
        backlog_sessions: update.backlog_sessions,
        ready_workflows: update.ready_workflows,
        proposals_written: update.proposals_written,
        prompt_bytes: update.prompt_bytes,
        timeout_secs: update.timeout.map(|value| value.as_secs()),
        stdout_bytes: update.stdout_bytes,
        stderr_bytes: update.stderr_bytes,
        last_stdout_at: format_optional_system_time(update.last_stdout_at),
        last_stderr_at: format_optional_system_time(update.last_stderr_at),
        durations_ms: update.durations,
        note: update.note,
    });
}

fn invoke_agent(
    command: &str,
    args: &[String],
    prompt: &str,
    workspace_root: &Path,
    context: AgentRunContext<'_>,
) -> Result<AgentInvocation> {
    invoke_agent_with_timeout(command, args, prompt, workspace_root, context)
}

fn invoke_agent_with_timeout(
    command: &str,
    args: &[String],
    prompt: &str,
    workspace_root: &Path,
    context: AgentRunContext<'_>,
) -> Result<AgentInvocation> {
    let timeout = context.timeout;
    let debug_run_dir = context.debug_run_dir;
    let debug_artifacts = context.debug_artifacts;
    let batch_size = context.batch_size;
    let selected_raw_bytes = context.selected_raw_bytes;
    let staged_bytes = context.staged_bytes;
    let discovered_sessions = context.discovered_sessions;
    let candidate_sessions = context.candidate_sessions;
    let skipped_sessions = context.skipped_sessions;
    let backlog_sessions = context.backlog_sessions;
    let ready_workflows = context.ready_workflows;
    let proposals_written = context.proposals_written;
    let durations = context.durations.clone();
    let prepared = prepare_proposal_command(
        command,
        args,
        workspace_root,
        debug_run_dir,
        context.output_schema,
    )?;
    let effective_args = prepared.args.clone();
    let mut temp_files = prepared.temp_files.clone();
    let mode = prepared.mode;

    let mut child_command = Command::new(command);
    child_command
        .args(&effective_args)
        .current_dir(workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &prepared.env_overrides {
        child_command.env(key, value);
    }

    let mut child = child_command
        .spawn()
        .with_context(|| format!("Failed to execute agent command: {command}"))?;

    let agent_started_at = Utc::now();
    let agent_pid = child.id();
    let stdout_path = debug_artifacts
        .map(|artifacts| artifacts.stdout_path())
        .or_else(|| debug_run_dir.map(|dir| dir.join("agent-stdout.log")))
        .unwrap_or(create_temp_file_path("distill-agent-stdout", "log")?);
    let stderr_path = debug_artifacts
        .map(|artifacts| artifacts.stderr_path())
        .or_else(|| debug_run_dir.map(|dir| dir.join("agent-stderr.log")))
        .unwrap_or(create_temp_file_path("distill-agent-stderr", "log")?);
    if debug_artifacts.is_none() && debug_run_dir.is_none() {
        temp_files.push(stdout_path.clone());
        temp_files.push(stderr_path.clone());
    }
    let stdout_capture = StreamCapture::spawn(
        child
            .stdout
            .take()
            .context("Failed to capture agent stdout pipe")?,
        stdout_path.clone(),
        context.candidate_paths.clone(),
    );
    let stderr_capture = StreamCapture::spawn(
        child
            .stderr
            .take()
            .context("Failed to capture agent stderr pipe")?,
        stderr_path.clone(),
        context.candidate_paths.clone(),
    );
    let stdout_bytes = stdout_capture.bytes_captured.clone();
    let stderr_bytes = stderr_capture.bytes_captured.clone();
    let stdout_last_update = stdout_capture.last_update.clone();
    let stderr_last_update = stderr_capture.last_update.clone();

    if let Some(debug_artifacts) = debug_artifacts {
        write_live_scan_status(
            debug_artifacts,
            LiveScanStatusUpdate {
                started_at: agent_started_at,
                state: "running",
                phase: context.phase,
                command,
                args: &effective_args,
                workspace_root,
                batch_size,
                selected_raw_bytes,
                staged_bytes,
                discovered_sessions,
                candidate_sessions,
                skipped_sessions,
                backlog_sessions,
                ready_workflows,
                proposals_written,
                prompt_bytes: prompt.len(),
                timeout,
                agent_pid: Some(agent_pid),
                stdout_bytes: 0,
                stderr_bytes: 0,
                last_stdout_at: None,
                last_stderr_at: None,
                durations: durations.clone(),
                note: Some("Waiting for agent response".to_string()),
            },
        );
    }

    if let Some(mut stdin) = child.stdin.take()
        && let Err(write_err) = stdin.write_all(prompt.as_bytes())
    {
        let status = child.wait().with_context(|| {
            format!("Failed to wait for agent command after stdin write failure: {command}")
        })?;
        let (stdout_tail, _, stdout_path) = stdout_capture.finish("stdout")?;
        let (stderr_tail, _, stderr_path) = stderr_capture.finish("stderr")?;
        let _ = stdout_path;
        let _ = stderr_path;
        if let Some(debug_artifacts) = debug_artifacts {
            write_live_scan_status(
                debug_artifacts,
                LiveScanStatusUpdate {
                    started_at: agent_started_at,
                    state: "failed",
                    phase: context.phase,
                    command,
                    args: &effective_args,
                    workspace_root,
                    batch_size,
                    selected_raw_bytes,
                    staged_bytes,
                    discovered_sessions,
                    candidate_sessions,
                    skipped_sessions,
                    backlog_sessions,
                    ready_workflows,
                    proposals_written,
                    prompt_bytes: prompt.len(),
                    timeout,
                    agent_pid: Some(agent_pid),
                    stdout_bytes: stdout_bytes.load(Ordering::Relaxed),
                    stderr_bytes: stderr_bytes.load(Ordering::Relaxed),
                    last_stdout_at: Some(SystemTime::now()),
                    last_stderr_at: Some(SystemTime::now()),
                    durations: durations.clone(),
                    note: Some("Failed to write prompt to agent stdin".to_string()),
                },
            );
        }
        if write_err.kind() == std::io::ErrorKind::BrokenPipe {
            let output = std::process::Output {
                status,
                stdout: stdout_tail,
                stderr: stderr_tail,
            };
            persist_agent_debug_output(debug_run_dir, prompt, None);
            cleanup_temp_files(&temp_files);
            return Err(write_err).with_context(|| format_agent_failure(command, &output, prompt));
        }

        cleanup_temp_files(&temp_files);
        return Err(write_err)
            .with_context(|| format!("Failed to write prompt to {command} stdin"));
    }

    let heartbeat_stdout_last_update = stdout_last_update.clone();
    let heartbeat_stderr_last_update = stderr_last_update.clone();
    let heartbeat_args = effective_args.clone();
    let heartbeat_command = command.to_string();
    let heartbeat_workspace_root = workspace_root.to_path_buf();
    let heartbeat_debug_artifacts = debug_artifacts.cloned();
    let prompt_len = prompt.len();
    let phase = context.phase.to_string();
    let heartbeat_stdout_bytes = stdout_bytes.clone();
    let heartbeat_stderr_bytes = stderr_bytes.clone();
    let heartbeat_durations = durations.clone();
    let (heartbeat_tx, heartbeat_rx) = std::sync::mpsc::channel::<()>();
    let heartbeat = std::thread::spawn(move || {
        let mut elapsed = 0u64;
        loop {
            match heartbeat_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    elapsed += 10;
                    eprint!("\r  ...agent working ({elapsed}s)   ");
                    if let Some(debug_artifacts) = heartbeat_debug_artifacts.as_ref() {
                        let last_stdout_at = heartbeat_stdout_last_update
                            .lock()
                            .ok()
                            .and_then(|value| *value);
                        let last_stderr_at = heartbeat_stderr_last_update
                            .lock()
                            .ok()
                            .and_then(|value| *value);
                        write_live_scan_status(
                            debug_artifacts,
                            LiveScanStatusUpdate {
                                started_at: agent_started_at,
                                state: "running",
                                phase: &phase,
                                command: &heartbeat_command,
                                args: &heartbeat_args,
                                workspace_root: &heartbeat_workspace_root,
                                batch_size,
                                selected_raw_bytes,
                                staged_bytes,
                                discovered_sessions,
                                candidate_sessions,
                                skipped_sessions,
                                backlog_sessions,
                                ready_workflows,
                                proposals_written,
                                prompt_bytes: prompt_len,
                                timeout,
                                agent_pid: Some(agent_pid),
                                stdout_bytes: heartbeat_stdout_bytes.load(Ordering::Relaxed),
                                stderr_bytes: heartbeat_stderr_bytes.load(Ordering::Relaxed),
                                last_stdout_at,
                                last_stderr_at,
                                durations: heartbeat_durations.clone(),
                                note: Some(format!("Agent running for {elapsed}s")),
                            },
                        );
                    }
                }
            }
        }
        eprint!("\r                            \r");
    });

    let wait_started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if timeout.is_some_and(|timeout| wait_started.elapsed() >= timeout) {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().with_context(|| {
                        format!("Failed to wait for agent command after timeout: {command}")
                    })?;
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

    let _ = heartbeat_tx.send(());
    let _ = heartbeat.join();
    let (stdout_tail, stdout_touched, stdout_path) = stdout_capture.finish("stdout")?;
    let (stderr_tail, stderr_touched, stderr_path) = stderr_capture.finish("stderr")?;
    let mut touched_paths = stdout_touched;
    touched_paths.extend(stderr_touched);
    let stdout_tail = String::from_utf8_lossy(&stdout_tail).to_string();
    let stderr_tail = String::from_utf8_lossy(&stderr_tail).to_string();
    let stdout = std::fs::read_to_string(&stdout_path)
        .with_context(|| format!("Failed to read {}", stdout_path.display()))?;
    let _ = stderr_path;

    if timed_out {
        persist_agent_debug_output(debug_run_dir, prompt, None);
        if let Some(debug_artifacts) = debug_artifacts {
            write_live_scan_status(
                debug_artifacts,
                LiveScanStatusUpdate {
                    started_at: agent_started_at,
                    state: "timed_out",
                    phase: context.phase,
                    command,
                    args: &effective_args,
                    workspace_root,
                    batch_size,
                    selected_raw_bytes,
                    staged_bytes,
                    discovered_sessions,
                    candidate_sessions,
                    skipped_sessions,
                    backlog_sessions,
                    ready_workflows,
                    proposals_written,
                    prompt_bytes: prompt.len(),
                    timeout,
                    agent_pid: Some(agent_pid),
                    stdout_bytes: stdout_bytes.load(Ordering::Relaxed),
                    stderr_bytes: stderr_bytes.load(Ordering::Relaxed),
                    last_stdout_at: stdout_last_update.lock().ok().and_then(|value| *value),
                    last_stderr_at: stderr_last_update.lock().ok().and_then(|value| *value),
                    durations: durations.clone(),
                    note: Some("Agent timed out before producing a final response".to_string()),
                },
            );
        }
        let stderr = sanitize_agent_diagnostics(&stderr_tail, prompt);
        let stdout = sanitize_agent_diagnostics(&stdout_tail, prompt);
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
            "Agent command `{command}` timed out after {}s. Verify the CLI is installed and authenticated, raise DISTILL_AGENT_TIMEOUT_SECS for slower runs, or set DISTILL_AGENT_TIMEOUT_SECS=0 to disable the timeout entirely.{details}",
            timeout
                .expect("timeout should exist when timeout branch is reached")
                .as_secs()
        );
    }

    if !status.success() {
        persist_agent_debug_output(debug_run_dir, prompt, None);
        if let Some(debug_artifacts) = debug_artifacts {
            write_live_scan_status(
                debug_artifacts,
                LiveScanStatusUpdate {
                    started_at: agent_started_at,
                    state: "failed",
                    phase: context.phase,
                    command,
                    args: &effective_args,
                    workspace_root,
                    batch_size,
                    selected_raw_bytes,
                    staged_bytes,
                    discovered_sessions,
                    candidate_sessions,
                    skipped_sessions,
                    backlog_sessions,
                    ready_workflows,
                    proposals_written,
                    prompt_bytes: prompt.len(),
                    timeout,
                    agent_pid: Some(agent_pid),
                    stdout_bytes: stdout_bytes.load(Ordering::Relaxed),
                    stderr_bytes: stderr_bytes.load(Ordering::Relaxed),
                    last_stdout_at: stdout_last_update.lock().ok().and_then(|value| *value),
                    last_stderr_at: stderr_last_update.lock().ok().and_then(|value| *value),
                    durations: durations.clone(),
                    note: Some(format!("Agent exited with status {status}")),
                },
            );
        }
        cleanup_temp_files(&temp_files);
        let output = std::process::Output {
            status,
            stdout: stdout_tail.into_bytes(),
            stderr: stderr_tail.into_bytes(),
        };
        bail!("{}", format_agent_failure(command, &output, prompt));
    }

    let final_output =
        finalize_proposal_output(mode, &stdout, prepared.sidecar_output_path.as_deref())?;

    persist_agent_debug_output(debug_run_dir, prompt, Some(&final_output));
    if let Some(debug_artifacts) = debug_artifacts {
        write_live_scan_status(
            debug_artifacts,
            LiveScanStatusUpdate {
                started_at: agent_started_at,
                state: "completed",
                phase: context.phase,
                command,
                args: &effective_args,
                workspace_root,
                batch_size,
                selected_raw_bytes,
                staged_bytes,
                discovered_sessions,
                candidate_sessions,
                skipped_sessions,
                backlog_sessions,
                ready_workflows,
                proposals_written,
                prompt_bytes: prompt.len(),
                timeout,
                agent_pid: Some(agent_pid),
                stdout_bytes: stdout_bytes.load(Ordering::Relaxed),
                stderr_bytes: stderr_bytes.load(Ordering::Relaxed),
                last_stdout_at: stdout_last_update.lock().ok().and_then(|value| *value),
                last_stderr_at: stderr_last_update.lock().ok().and_then(|value| *value),
                durations: durations.clone(),
                note: Some("Agent completed successfully".to_string()),
            },
        );
    }
    cleanup_temp_files(&temp_files);

    Ok(AgentInvocation {
        final_output,
        touched_paths,
        mode,
    })
}

fn persist_agent_debug_output(
    debug_run_dir: Option<&Path>,
    prompt: &str,
    final_output: Option<&str>,
) {
    write_debug_text(debug_run_dir, "prompt.txt", prompt);
    if let Some(final_output) = final_output {
        write_debug_text(debug_run_dir, "agent-final-output.txt", final_output);
    }
}

fn parse_scan_response(raw: &str, workspace_root: &Path) -> Result<ParsedScanResponse> {
    let response_value = extract_json_value(raw)?;
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

fn parse_workflow_response(raw: &str, workspace_root: &Path) -> Result<ParsedWorkflowResponse> {
    let response_value = extract_json_value(raw)?;
    let raw_response: RawWorkflowResponse =
        serde_json::from_value(response_value).context("Failed to parse workflow response")?;

    let inspected_files = raw_response
        .inspected_files
        .into_iter()
        .map(|path| normalize_reported_path(&path, workspace_root))
        .collect::<Vec<_>>();

    let session_findings = raw_response
        .session_findings
        .into_iter()
        .map(|finding| SessionWorkflowFinding {
            session: normalize_reported_path(&finding.session, workspace_root),
            summary: finding.summary,
            candidates: finding
                .candidates
                .into_iter()
                .map(|candidate| WorkflowFinding {
                    workflow_key: candidate.workflow_key.trim().to_string(),
                    workflow_label: candidate
                        .workflow_label
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty()),
                    note: candidate.note.trim().to_string(),
                    start_event: candidate.start_event,
                    end_event: candidate.end_event,
                })
                .collect::<Vec<_>>(),
        })
        .collect::<Vec<_>>();

    Ok(ParsedWorkflowResponse {
        inspected_files,
        session_findings,
    })
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

    let target_skill = raw_proposal
        .target_skill
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    let target_skill_is_present = target_skill.as_deref().is_some();
    let should_derive_new_target = proposal_type == ProposalType::New;
    match proposal_type {
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
            target: target_skill
                .or_else(|| {
                    if should_derive_new_target {
                        infer_skill_name_from_body(&raw_proposal.body)
                    } else {
                        None
                    }
                })
                .map(|name| ProposalTarget::Skill { name }),
            target_skill: None,
            evidence,
            created: Utc::now(),
        },
        body: raw_proposal.body,
        filename: None,
    })
}

fn validate_workflow_response(
    parsed: &ParsedWorkflowResponse,
    workspace: &StagedWorkspace,
    invocation: &AgentInvocation,
) -> Result<()> {
    let expected_paths = workspace
        .staged_sessions
        .iter()
        .map(|session| canonicalize_path(&session.staged_path))
        .collect::<Vec<_>>();

    validate_full_coverage("inspected_files", &parsed.inspected_files, &expected_paths)?;

    let finding_sessions = parsed
        .session_findings
        .iter()
        .map(|finding| finding.session.clone())
        .collect::<Vec<_>>();
    validate_full_coverage("session_findings", &finding_sessions, &expected_paths)?;

    for finding in &parsed.session_findings {
        if finding.summary.trim().is_empty() {
            bail!(
                "Every session finding must include a non-empty summary (missing for {})",
                finding.session.display()
            );
        }
        for candidate in &finding.candidates {
            if candidate.workflow_key.trim().is_empty() {
                bail!(
                    "Workflow candidates must include a non-empty workflow_key ({})",
                    finding.session.display()
                );
            }
            if candidate.note.trim().is_empty() {
                bail!(
                    "Workflow candidates must include a non-empty note ({})",
                    finding.session.display()
                );
            }
            if candidate.start_event == 0 || candidate.end_event == 0 {
                bail!(
                    "Workflow candidate ranges must use 1-based event numbers ({})",
                    finding.session.display()
                );
            }
            if candidate.end_event < candidate.start_event {
                bail!(
                    "Workflow candidate end_event must be >= start_event ({})",
                    finding.session.display()
                );
            }
        }
    }

    if matches!(
        invocation.mode,
        ProposalAgentMode::Claude | ProposalAgentMode::Codex
    ) {
        validate_touched_paths(&invocation.touched_paths, &expected_paths)?;
    }

    Ok(())
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

    if matches!(
        invocation.mode,
        ProposalAgentMode::Claude | ProposalAgentMode::Codex
    ) {
        validate_touched_paths(&invocation.touched_paths, &expected_paths)?;
    }

    let staged_to_original = workspace
        .staged_sessions
        .iter()
        .map(|session| {
            (
                canonicalize_path(&session.staged_path),
                session.source_session.path.to_string_lossy().to_string(),
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

fn validate_touched_paths(touched: &HashSet<PathBuf>, expected_paths: &[PathBuf]) -> Result<()> {
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

#[cfg(test)]
#[allow(dead_code)]
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

#[cfg(test)]
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
        let normalized_haystack = normalize_path_like_text(&haystack);
        for path in candidate_paths {
            if path_search_variants(path)
                .iter()
                .any(|variant| haystack.contains(variant) || normalized_haystack.contains(variant))
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

    let mut normalized = expanded.clone();
    for variant in expanded {
        let collapsed = normalize_path_like_text(&variant);
        if !normalized.contains(&collapsed) {
            normalized.push(collapsed);
        }
    }

    normalized
}

fn normalize_path_like_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut previous_was_slash = false;
    for ch in text.chars() {
        if ch == '/' {
            if !previous_was_slash {
                normalized.push(ch);
            }
            previous_was_slash = true;
        } else {
            previous_was_slash = false;
            normalized.push(ch);
        }
    }

    normalized
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
    use std::collections::HashSet;
    use std::fs;

    fn sample_session(id: &str, agent: AgentKind, hours_ago: i64) -> Session {
        Session {
            id: id.to_string(),
            agent,
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
            content: String::new(),
        }
    }

    fn temp_session_file(
        dir: &tempfile::TempDir,
        name: &str,
        bytes: usize,
        hours_ago: i64,
    ) -> Session {
        let path = dir.path().join(name);
        fs::write(&path, "x".repeat(bytes)).unwrap();
        Session {
            id: name.to_string(),
            agent: AgentKind::Codex,
            path,
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
    fn test_select_session_batch_respects_count_and_raw_byte_cap() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = vec![
            temp_session_file(&dir, "one.jsonl", 10, 3),
            temp_session_file(&dir, "two.jsonl", 20, 2),
            temp_session_file(&dir, "three.jsonl", 30, 1),
        ];

        let selected = select_session_batch(&sessions, 3, Some(35)).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].session.id, "one.jsonl");
        assert_eq!(selected[1].session.id, "two.jsonl");

        let selected = select_session_batch(&sessions, 1, Some(100)).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].session.id, "one.jsonl");
    }

    #[test]
    fn test_select_session_batch_keeps_single_oversized_first_session() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = vec![
            temp_session_file(&dir, "huge.jsonl", 200, 2),
            temp_session_file(&dir, "small.jsonl", 10, 1),
        ];

        let selected = select_session_batch(&sessions, 5, Some(64)).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].session.id, "huge.jsonl");
    }

    #[test]
    fn test_agent_timeout_defaults_to_two_hours() {
        assert_eq!(
            agent_timeout_from_env(None).unwrap(),
            Some(Duration::from_secs(DEFAULT_AGENT_TIMEOUT_SECS))
        );
    }

    #[test]
    fn test_agent_timeout_accepts_zero_to_disable() {
        assert_eq!(agent_timeout_from_env(Some("0")).unwrap(), None);
    }

    #[test]
    fn test_agent_timeout_parses_positive_seconds() {
        assert_eq!(
            agent_timeout_from_env(Some("3600")).unwrap(),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn test_scan_debug_artifacts_cleanup_success_run() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = ScanDebugArtifacts::new(dir.path().to_path_buf(), true).unwrap();
        let run_dir = artifacts.run_dir.clone();
        let current_run_path = artifacts.current_run_path.clone();

        assert!(run_dir.is_dir());
        assert!(current_run_path.is_file());

        artifacts.finish_success();

        assert!(!run_dir.exists());
        assert!(!current_run_path.exists());
    }

    #[test]
    fn test_scan_debug_artifacts_preserve_failure_run() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = ScanDebugArtifacts::new(dir.path().to_path_buf(), true).unwrap();
        let run_dir = artifacts.run_dir.clone();
        let current_run_path = artifacts.current_run_path.clone();
        let last_failed_run_path = artifacts.last_failed_run_path.clone();

        artifacts.finish_failure(&anyhow::anyhow!("boom"));

        assert!(run_dir.is_dir());
        assert!(!current_run_path.exists());
        assert_eq!(
            std::fs::read_to_string(&last_failed_run_path)
                .unwrap()
                .trim(),
            run_dir.to_string_lossy()
        );
        assert_eq!(
            std::fs::read_to_string(run_dir.join("error.txt")).unwrap(),
            "boom"
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
        assert!(prompt.contains("Do not run searches across the workspace."));
        assert!(
            prompt
                .contains("After you have read the listed files once, stop inspecting and answer.")
        );
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
                    source_session: sample_session("one", AgentKind::Claude, 2),
                    staged_path: PathBuf::from("/tmp/workspace/sessions/claude/one.jsonl"),
                },
                StagedSession {
                    source_session: sample_session("two", AgentKind::Claude, 1),
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
                touched_paths: HashSet::new(),
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
    fn test_touched_paths_from_audit_log_matches_private_prefix_and_double_slashes() {
        let path =
            PathBuf::from("/private/var/folders/test/workflow/sessions/codex/0001-example.jsonl");
        let log = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "command": "/bin/zsh -lc \"sed -n '1,120p' '/var/folders/test//workflow/sessions/codex/0001-example.jsonl'\""
            }
        })
        .to_string();

        let touched = touched_paths_from_audit_log(&log, std::slice::from_ref(&path));
        assert!(touched.contains(&path));
        assert_eq!(touched.len(), 1);
    }

    #[test]
    fn test_stage_scan_workspace_copies_sessions_and_skills() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.jsonl");
        let skill_path = dir.path().join("review.md");
        std::fs::write(
            &session_path,
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "Please improve the review flow."}]
                }
            })
            .to_string(),
        )
        .unwrap();
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
        let staged_session =
            std::fs::read_to_string(&workspace.staged_sessions[0].staged_path).unwrap();
        assert!(staged_session.contains("# Staged Session Summary"));
        assert!(staged_session.contains("Please improve the review flow."));
        assert_eq!(workspace.staged_skills.len(), 1);
        assert!(workspace.staged_skills[0].staged_path.is_file());
        assert_eq!(
            std::fs::read_to_string(&workspace.staged_skills[0].staged_path).unwrap(),
            "# Review"
        );
    }

    #[test]
    fn test_stage_scan_workspace_truncates_large_session_fields() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.jsonl");
        let huge = "x".repeat(MAX_STAGED_SUMMARY_EXCERPT_CHARS + 500);
        let line = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": huge}]
            }
        });
        std::fs::write(&session_path, format!("{line}\n")).unwrap();

        let session = Session {
            id: "session-1".to_string(),
            agent: AgentKind::Codex,
            path: session_path,
            timestamp: Utc::now(),
            content: String::new(),
        };

        let workspace = stage_scan_workspace(&[session], &[], None).unwrap();
        let staged = std::fs::read_to_string(&workspace.staged_sessions[0].staged_path).unwrap();

        assert!(staged.contains("[... omitted"));
        assert!(staged.contains("# Staged Session Summary"));
        assert!(!staged.contains(&"x".repeat(MAX_STAGED_SUMMARY_EXCERPT_CHARS + 500)));
    }

    #[test]
    fn test_build_staged_session_summary_extracts_opencode_export_messages_and_tools() {
        let session = Session {
            id: "sess-1".to_string(),
            agent: AgentKind::OpenCode,
            path: PathBuf::from("/tmp/home/.local/share/opencode/sessions/sess-1.json"),
            timestamp: Utc::now(),
            content: String::new(),
        };
        let raw = r#"{
          "messages": [
            {
              "role": "user",
              "content": [{"text": "Add OpenCode support from top to bottom."}]
            },
            {
              "role": "assistant",
              "content": [{"text": "I will inspect the agent registry and scan runner."}]
            },
            {
              "type": "tool_call",
              "tool": "read"
            }
          ]
        }"#;

        let summary = build_staged_session_summary(&session, raw);
        assert!(summary.contains("Add OpenCode support from top to bottom."));
        assert!(summary.contains("inspect the agent registry"));
        assert!(summary.contains("- read x1"));
    }

    #[test]
    fn test_build_staged_session_summary_extracts_current_opencode_export_shape() {
        let session = Session {
            id: "sess-2".to_string(),
            agent: AgentKind::OpenCode,
            path: PathBuf::from("/tmp/home/.local/share/opencode/sessions/sess-2.json"),
            timestamp: Utc::now(),
            content: String::new(),
        };
        let raw = r#"{
          "messages": [
            {
              "info": {
                "role": "user"
              },
              "parts": [
                {
                  "type": "text",
                  "text": "Remember marker DISTILL_REAL_SCAN_20260311."
                }
              ]
            },
            {
              "info": {
                "role": "assistant"
              },
              "parts": [
                {
                  "type": "text",
                  "text": "ACK"
                }
              ]
            }
          ]
        }"#;

        let summary = build_staged_session_summary(&session, raw);
        assert!(summary.contains("DISTILL_REAL_SCAN_20260311"));
        assert!(summary.contains("ACK"));
    }
}
