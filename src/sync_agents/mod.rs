use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agents::Agent;
use crate::proposals::{
    Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalTarget, ProposalType,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastSyncAgents {
    pub timestamp: DateTime<Utc>,
}

impl LastSyncAgents {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let value: Self =
            serde_json::from_str(&contents).with_context(|| "Failed to parse last-sync-agents")?;
        Ok(Some(value))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize last-sync-agents watermark")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum ProjectStatus {
    Updated,
    NoChanges,
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct ProjectSyncResult {
    pub project: PathBuf,
    pub status: ProjectStatus,
    pub commit_count: usize,
    pub file_count: usize,
    pub session_count: usize,
    pub proposals_written: usize,
    pub proposals_skipped_pending: usize,
}

#[derive(Debug, Clone)]
pub struct SyncAgentsSummary {
    pub since: DateTime<Utc>,
    pub proposals_written: usize,
    pub proposals_skipped_pending: usize,
    pub results: Vec<ProjectSyncResult>,
}

#[derive(Debug, Clone)]
pub struct SyncAgentsRunConfig {
    pub proposal_agent: String,
    pub proposals_dir: PathBuf,
    pub last_sync_path: PathBuf,
    pub dry_run: bool,
    pub since_override: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Clone)]
struct GitEvidence {
    commits: Vec<String>,
    files: Vec<String>,
}

#[derive(Debug, Clone)]
struct SessionEvidence {
    session: String,
    cwd: PathBuf,
}

#[derive(Debug, Clone)]
struct AgentInvocation {
    command: String,
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeEnvelope {
    is_error: Option<bool>,
    structured_output: Option<serde_json::Value>,
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProposalWrapper {
    proposals: Vec<RawSyncProposal>,
}

#[derive(Debug, Deserialize)]
struct RawSyncTarget {
    kind: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct RawSyncEvidence {
    session: String,
    pattern: String,
}

#[derive(Debug, Deserialize)]
struct RawSyncProposal {
    #[serde(rename = "type")]
    proposal_type: String,
    confidence: String,
    target: RawSyncTarget,
    evidence: Vec<RawSyncEvidence>,
    body: String,
}

pub fn parse_since(value: &str) -> Result<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(value) {
        return Ok(ts.to_utc());
    }

    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let ndt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow::anyhow!("Invalid date value: {value}"))?;
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
    }

    bail!(
        "Invalid --since value '{}'. Use YYYY-MM-DD or RFC3339 (e.g. 2026-03-01T12:00:00Z).",
        value
    );
}

pub fn resolve_projects(projects: &[String]) -> Result<Vec<PathBuf>> {
    let mut deduped = BTreeSet::new();

    for project in projects {
        let raw = project.trim();
        if raw.is_empty() {
            continue;
        }

        let canonical = std::fs::canonicalize(raw)
            .with_context(|| format!("Failed to resolve project path: {raw}"))?;
        if !canonical.is_dir() {
            bail!("Project path is not a directory: {}", canonical.display());
        }

        if !is_git_repo(&canonical)? {
            bail!(
                "Project is not a git repository: {}. Use a repo root path.",
                canonical.display()
            );
        }

        deduped.insert(canonical);
    }

    if deduped.is_empty() {
        bail!("No valid projects were provided.");
    }

    Ok(deduped.into_iter().collect())
}

pub fn run_sync_agents(
    projects: &[PathBuf],
    agents: &[Box<dyn Agent>],
    run_config: &SyncAgentsRunConfig,
) -> Result<SyncAgentsSummary> {
    let since = resolve_since(run_config.since_override, &run_config.last_sync_path)?;

    if !run_config.dry_run {
        std::fs::create_dir_all(&run_config.proposals_dir).with_context(|| {
            format!(
                "Failed to create proposals directory: {}",
                run_config.proposals_dir.display()
            )
        })?;
    }

    let mut existing_targets = pending_file_targets(&run_config.proposals_dir)?;
    let mut results = Vec::new();
    let mut total_written = 0usize;
    let mut total_skipped_pending = 0usize;

    let invocation = agent_invocation(&run_config.proposal_agent);

    for project in projects {
        let agents_path = project.join("AGENTS.md");
        let existing_agents = std::fs::read_to_string(&agents_path).ok();

        let git_evidence = match collect_git_evidence(project, since) {
            Ok(value) => value,
            Err(err) => {
                results.push(ProjectSyncResult {
                    project: project.clone(),
                    status: ProjectStatus::Skipped(format!("git evidence failed: {err}")),
                    commit_count: 0,
                    file_count: 0,
                    session_count: 0,
                    proposals_written: 0,
                    proposals_skipped_pending: 0,
                });
                continue;
            }
        };

        let session_evidence = collect_project_session_evidence(agents, since, project);

        let prompt = build_prompt(
            project,
            &agents_path,
            existing_agents.as_deref(),
            &git_evidence,
            &session_evidence,
        );

        let raw = match invoke_agent(&invocation, &prompt) {
            Ok(value) => value,
            Err(err) => {
                results.push(ProjectSyncResult {
                    project: project.clone(),
                    status: ProjectStatus::Skipped(format!("agent invocation failed: {err}")),
                    commit_count: git_evidence.commits.len(),
                    file_count: git_evidence.files.len(),
                    session_count: session_evidence.len(),
                    proposals_written: 0,
                    proposals_skipped_pending: 0,
                });
                continue;
            }
        };

        let proposals = match parse_agent_response(&raw, &agents_path) {
            Ok(value) => value,
            Err(err) => {
                results.push(ProjectSyncResult {
                    project: project.clone(),
                    status: ProjectStatus::Skipped(format!("invalid agent output: {err}")),
                    commit_count: git_evidence.commits.len(),
                    file_count: git_evidence.files.len(),
                    session_count: session_evidence.len(),
                    proposals_written: 0,
                    proposals_skipped_pending: 0,
                });
                continue;
            }
        };

        if proposals.is_empty() {
            results.push(ProjectSyncResult {
                project: project.clone(),
                status: ProjectStatus::NoChanges,
                commit_count: git_evidence.commits.len(),
                file_count: git_evidence.files.len(),
                session_count: session_evidence.len(),
                proposals_written: 0,
                proposals_skipped_pending: 0,
            });
            continue;
        }

        let project_slug = sanitize_slug(
            project
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("project"),
        );

        let mut written_for_project = 0usize;
        let mut skipped_pending_for_project = 0usize;

        for (index, proposal) in proposals.into_iter().enumerate() {
            let target_path = match proposal.frontmatter.resolved_target() {
                Some(ProposalTarget::File { path }) => path,
                _ => {
                    skipped_pending_for_project += 1;
                    continue;
                }
            };

            if existing_targets.contains(&target_path) {
                skipped_pending_for_project += 1;
                continue;
            }

            if run_config.dry_run {
                written_for_project += 1;
                continue;
            }

            let proposal_filename = format!(
                "{}-agents-{}-{}-{}.md",
                proposal_type_slug(&proposal.frontmatter.proposal_type),
                project_slug,
                Utc::now().format("%Y%m%d-%H%M%S"),
                index,
            );
            let proposal_path = run_config.proposals_dir.join(&proposal_filename);
            let markdown = proposal
                .to_markdown()
                .context("Failed to serialize sync-agents proposal")?;
            std::fs::write(&proposal_path, markdown)
                .with_context(|| format!("Failed to write {}", proposal_path.display()))?;

            existing_targets.insert(target_path);
            written_for_project += 1;
        }

        total_written += written_for_project;
        total_skipped_pending += skipped_pending_for_project;

        let status = if written_for_project > 0 {
            ProjectStatus::Updated
        } else if skipped_pending_for_project > 0 {
            ProjectStatus::Skipped("pending proposal already exists for AGENTS.md".to_string())
        } else {
            ProjectStatus::NoChanges
        };

        results.push(ProjectSyncResult {
            project: project.clone(),
            status,
            commit_count: git_evidence.commits.len(),
            file_count: git_evidence.files.len(),
            session_count: session_evidence.len(),
            proposals_written: written_for_project,
            proposals_skipped_pending: skipped_pending_for_project,
        });
    }

    if !run_config.dry_run {
        LastSyncAgents {
            timestamp: Utc::now(),
        }
        .save(&run_config.last_sync_path)?;
    }

    Ok(SyncAgentsSummary {
        since,
        proposals_written: total_written,
        proposals_skipped_pending: total_skipped_pending,
        results,
    })
}

fn resolve_since(
    since_override: Option<DateTime<Utc>>,
    watermark_path: &Path,
) -> Result<DateTime<Utc>> {
    if let Some(since) = since_override {
        return Ok(since);
    }

    if let Some(last_sync) = LastSyncAgents::load(watermark_path)? {
        return Ok(last_sync.timestamp);
    }

    Ok(Utc::now() - Duration::days(30))
}

fn is_git_repo(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .with_context(|| format!("Failed to run git for {}", path.display()))?;

    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn collect_git_evidence(project: &Path, since: DateTime<Utc>) -> Result<GitEvidence> {
    let since_arg = since.to_rfc3339();

    let commits_output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args([
            "log",
            "--since",
            &since_arg,
            "--pretty=format:%h %s",
            "--max-count",
            "20",
        ])
        .output()
        .with_context(|| format!("Failed to collect git commits for {}", project.display()))?;

    if !commits_output.status.success() {
        bail!("Failed to read git commits for {}", project.display());
    }

    let files_output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args([
            "log",
            "--since",
            &since_arg,
            "--name-only",
            "--pretty=format:",
            "--max-count",
            "40",
        ])
        .output()
        .with_context(|| format!("Failed to collect changed files for {}", project.display()))?;

    if !files_output.status.success() {
        bail!("Failed to read changed files for {}", project.display());
    }

    let commits = String::from_utf8_lossy(&commits_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(20)
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&files_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if seen.insert(line.to_string()) {
            files.push(line.to_string());
        }
        if files.len() >= 30 {
            break;
        }
    }

    Ok(GitEvidence { commits, files })
}

fn collect_project_session_evidence(
    agents: &[Box<dyn Agent>],
    since: DateTime<Utc>,
    project: &Path,
) -> Vec<SessionEvidence> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();

    for agent in agents {
        let sessions = match agent.read_sessions(since) {
            Ok(value) => value,
            Err(_) => continue,
        };

        for session in sessions {
            if !seen.insert(session.id.clone()) {
                continue;
            }

            let Some(cwd) = session_cwd_from_jsonl(&session.path) else {
                continue;
            };

            if !cwd.starts_with(project) {
                continue;
            }

            output.push(SessionEvidence {
                session: session.path.display().to_string(),
                cwd,
            });
        }
    }

    output.sort_by(|a, b| a.session.cmp(&b.session));
    output
}

fn session_cwd_from_jsonl(path: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(path).ok()?;

    for line in content.lines().take(200) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
        if let Some(cwd) = cwd_from_json_value(&value) {
            return Some(PathBuf::from(cwd));
        }
    }

    None
}

fn cwd_from_json_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or_else(|| {
            value
                .get("payload")
                .and_then(|v| v.get("cwd"))
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
        })
}

fn build_prompt(
    project: &Path,
    agents_path: &Path,
    existing_agents: Option<&str>,
    git_evidence: &GitEvidence,
    session_evidence: &[SessionEvidence],
) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are an AGENTS.md update engine for distill.\n\
Return only JSON in this exact shape: {\"proposals\":[...]}\n\
No markdown fences, no explanation.\n\
If there is no strong evidence for an update, return {\"proposals\":[]}.\n\n",
    );

    prompt.push_str("Proposal schema rules:\n");
    prompt.push_str("- type: one of new|edit|improve|remove\n");
    prompt.push_str("- confidence: one of high|medium|low\n");
    prompt.push_str(&format!(
        "- target: must be {{\"kind\":\"file\",\"path\":\"{}\"}}\n",
        agents_path.display()
    ));
    prompt.push_str("- evidence: array of {session, pattern}\n");
    prompt.push_str("- body: full AGENTS.md file content\n\n");

    prompt.push_str(&format!("Project: {}\n", project.display()));
    prompt.push_str(&format!("Target AGENTS.md: {}\n\n", agents_path.display()));

    prompt.push_str("Current AGENTS.md:\n");
    if let Some(content) = existing_agents {
        prompt.push_str(content);
        prompt.push_str("\n\n");
    } else {
        prompt.push_str("(missing)\n\n");
    }

    prompt.push_str("Git evidence (recent commits):\n");
    if git_evidence.commits.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for commit in &git_evidence.commits {
            prompt.push_str(&format!("- {commit}\n"));
        }
    }
    prompt.push_str("\nChanged files:\n");
    if git_evidence.files.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for file in &git_evidence.files {
            prompt.push_str(&format!("- {file}\n"));
        }
    }

    prompt.push_str("\nSession evidence:\n");
    if session_evidence.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for session in session_evidence.iter().take(20) {
            prompt.push_str(&format!(
                "- session={} cwd={}\n",
                session.session,
                session.cwd.display()
            ));
        }
    }

    prompt.push_str("\nDecide whether AGENTS.md should be created/updated/removed based only on evidence above.\n");
    prompt.push_str("Keep style consistent with current file when it exists.\n");

    prompt
}

fn agent_invocation(agent_name: &str) -> AgentInvocation {
    match agent_name {
        "claude" => AgentInvocation {
            command: "claude".to_string(),
            args: vec![
                "--print".to_string(),
                "--no-session-persistence".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
            ],
        },
        "codex" => AgentInvocation {
            command: "codex".to_string(),
            args: vec!["exec".to_string(), "--ephemeral".to_string()],
        },
        other => AgentInvocation {
            command: other.to_string(),
            args: vec![],
        },
    }
}

fn invoke_agent(invocation: &AgentInvocation, prompt: &str) -> Result<String> {
    let mut child = Command::new(&invocation.command)
        .args(&invocation.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to execute {}", invocation.command))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .with_context(|| format!("Failed to write prompt to {}", invocation.command))?;
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("Failed to wait on {}", invocation.command))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Agent command '{}' failed with status {}: {}{}",
            invocation.command,
            output.status,
            stderr.trim(),
            if stdout.trim().is_empty() {
                "".to_string()
            } else {
                format!("\n{}", stdout.trim())
            }
        );
    }

    String::from_utf8(output.stdout).context("Agent output is not valid UTF-8")
}

fn parse_agent_response(raw: &str, expected_agents_path: &Path) -> Result<Vec<Proposal>> {
    let trimmed = raw.trim();

    let root_value = if let Ok(envelope) = serde_json::from_str::<ClaudeEnvelope>(trimmed) {
        if envelope.is_error.unwrap_or(false) {
            let msg = envelope
                .result
                .unwrap_or_else(|| "unknown Claude error".to_string());
            bail!("Claude agent returned an error: {msg}");
        }

        if let Some(structured) = envelope.structured_output {
            structured
        } else if let Some(result) = envelope.result {
            extract_json_value(&result)?
        } else {
            extract_json_value(trimmed)?
        }
    } else {
        extract_json_value(trimmed)?
    };

    let raw_proposals =
        if let Ok(wrapper) = serde_json::from_value::<ProposalWrapper>(root_value.clone()) {
            wrapper.proposals
        } else {
            serde_json::from_value::<Vec<RawSyncProposal>>(root_value)
                .context("Failed to parse sync-agents proposals")?
        };

    let mut proposals = Vec::new();
    for raw_proposal in raw_proposals {
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

        if raw_proposal.target.kind != "file" {
            bail!("sync-agents target.kind must be 'file'");
        }

        let target_path = PathBuf::from(&raw_proposal.target.path);
        if !target_path.is_absolute() {
            bail!("sync-agents target path must be absolute");
        }
        if target_path.file_name().and_then(|n| n.to_str()) != Some("AGENTS.md") {
            bail!("sync-agents target basename must be AGENTS.md");
        }
        if target_path != expected_agents_path {
            bail!(
                "sync-agents target path mismatch (expected {}, got {})",
                expected_agents_path.display(),
                target_path.display()
            );
        }

        if raw_proposal.body.trim().is_empty() {
            bail!("Proposal body must be non-empty");
        }

        let evidence = raw_proposal
            .evidence
            .into_iter()
            .map(|ev| Evidence {
                session: ev.session,
                pattern: ev.pattern,
            })
            .collect::<Vec<_>>();

        proposals.push(Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type,
                confidence,
                target: Some(ProposalTarget::File {
                    path: target_path.display().to_string(),
                }),
                target_skill: None,
                evidence,
                created: Utc::now(),
            },
            body: raw_proposal.body,
            filename: None,
        });
    }

    Ok(proposals)
}

fn extract_json_value(text: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();

    let json_text = if trimmed.starts_with("```") {
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

    serde_json::from_str(json_text).context("Failed to parse agent response as JSON")
}

fn pending_file_targets(proposals_dir: &Path) -> Result<BTreeSet<String>> {
    if !proposals_dir.exists() {
        return Ok(BTreeSet::new());
    }

    let mut targets = BTreeSet::new();
    for entry in std::fs::read_dir(proposals_dir)
        .with_context(|| format!("Failed to read {}", proposals_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let Ok(proposal) = Proposal::from_markdown(&content) else {
            continue;
        };

        if let Some(ProposalTarget::File { path }) = proposal.frontmatter.resolved_target() {
            targets.insert(path);
        }
    }

    Ok(targets)
}

fn proposal_type_slug(proposal_type: &ProposalType) -> &'static str {
    match proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    }
}

fn sanitize_slug(value: &str) -> String {
    let mut out = String::new();
    let mut dash = false;

    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_since_date() {
        let since = parse_since("2026-03-01").unwrap();
        assert_eq!(since.to_rfc3339(), "2026-03-01T00:00:00+00:00");
    }

    #[test]
    fn test_parse_since_rfc3339() {
        let since = parse_since("2026-03-01T12:30:00Z").unwrap();
        assert_eq!(since.to_rfc3339(), "2026-03-01T12:30:00+00:00");
    }

    #[test]
    fn test_parse_since_invalid() {
        assert!(parse_since("03-01-2026").is_err());
    }

    #[test]
    fn test_resolve_projects_dedupes_and_validates_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();

        let init = Command::new("git")
            .arg("init")
            .arg(&project)
            .output()
            .unwrap();
        assert!(init.status.success());

        let projects = vec![project.display().to_string(), project.display().to_string()];
        let resolved = resolve_projects(&projects).unwrap();
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn test_parse_agent_response_file_target() {
        let expected = PathBuf::from("/tmp/example/AGENTS.md");
        let json = r##"{"proposals":[{"type":"edit","confidence":"high","target":{"kind":"file","path":"/tmp/example/AGENTS.md"},"evidence":[{"session":"s1","pattern":"p1"}],"body":"# AGENTS\n\nUpdated"}]}"##;
        let parsed = parse_agent_response(json, &expected).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].frontmatter.proposal_type, ProposalType::Edit);
        assert_eq!(
            parsed[0].frontmatter.resolved_target(),
            Some(ProposalTarget::File {
                path: "/tmp/example/AGENTS.md".to_string()
            })
        );
    }

    #[test]
    fn test_pending_file_targets_reads_targets() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::Edit,
                confidence: Confidence::High,
                target: Some(ProposalTarget::File {
                    path: "/tmp/project/AGENTS.md".to_string(),
                }),
                target_skill: None,
                evidence: vec![],
                created: Utc::now(),
            },
            body: "# AGENTS".to_string(),
            filename: None,
        };

        std::fs::write(
            proposals_dir.join("edit-agents-test.md"),
            proposal.to_markdown().unwrap(),
        )
        .unwrap();

        let targets = pending_file_targets(&proposals_dir).unwrap();
        assert!(targets.contains("/tmp/project/AGENTS.md"));
    }
}
