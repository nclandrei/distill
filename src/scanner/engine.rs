use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agents::{Agent, Session, Skill};
use crate::config::Config;
use crate::proposals::{Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalType};
use crate::scanner::reader::{self, LastScan};

/// JSON Schema for the proposal response.  Passed to agents via
/// `--json-schema` (claude) or `--output-schema` (codex) so the response
/// is validated and deterministic.
///
/// The top-level type must be `object` (API requirement), so we wrap the
/// proposal array in `{"proposals": [...]}`.
const PROPOSAL_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "proposals": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "type":         { "type": "string", "enum": ["new", "improve", "edit", "remove"] },
          "confidence":   { "type": "string", "enum": ["high", "medium", "low"] },
          "target_skill": { "type": ["string", "null"] },
          "evidence": {
            "type": "array",
            "items": {
              "type": "object",
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
  "required": ["proposals"]
}"#;

/// Configuration for the scan engine, allowing dependency injection for testing.
pub struct ScanConfig {
    /// The command to invoke the generation agent (e.g. "claude" or "codex").
    pub agent_command: String,
    /// Arguments to pass for non-interactive output (e.g. ["--print"] for claude).
    pub agent_args: Vec<String>,
    /// Directory containing existing skills.
    pub skills_dir: PathBuf,
    /// Directory to write proposals to.
    pub proposals_dir: PathBuf,
    /// Path to last-scan.json.
    pub last_scan_path: PathBuf,
}

impl ScanConfig {
    /// Build ScanConfig from the user's Config.
    pub fn from_config(config: &Config) -> Self {
        let (command, args) = agent_command_for(&config.proposal_agent);
        Self {
            agent_command: command,
            agent_args: args,
            skills_dir: Config::skills_dir(),
            proposals_dir: Config::proposals_dir(),
            last_scan_path: Config::last_scan_path(),
        }
    }
}

/// Return (command, args) for invoking a generation agent non-interactively.
///
/// The prompt is always delivered via stdin (see [`invoke_agent`]), so these
/// args only configure non-interactive / print-only output and structured JSON.
fn agent_command_for(agent_name: &str) -> (String, Vec<String>) {
    match agent_name {
        // `claude -p` reads the prompt from stdin.
        // `--output-format json` wraps the response in a JSON envelope.
        // `--json-schema` enforces structured output matching our proposal schema.
        "claude" => ("claude".into(), vec![
            "--print".into(),
            "--output-format".into(), "json".into(),
            "--json-schema".into(), PROPOSAL_SCHEMA.into(),
        ]),
        // `codex exec` is the headless CLI; reads prompt from stdin.
        // `--output-schema` requires a file path — written at invocation time.
        "codex" => ("codex".into(), vec!["exec".into()]),
        other => (other.into(), vec![]),
    }
}

/// Run the full scan pipeline.
///
/// 1. Load last-scan watermark to determine `since` timestamp
/// 2. Collect sessions from all agents since that timestamp
/// 3. Load existing skills from disk
/// 4. Build a prompt and invoke the configured generation agent
/// 5. Parse the agent's structured response into proposals
/// 6. Write proposals to the proposals directory
/// 7. Update the last-scan watermark
pub fn run_scan(agents: &[Box<dyn Agent>], scan_config: &ScanConfig) -> Result<Vec<Proposal>> {
    // Step 1: Determine the "since" timestamp
    let last_scan = LastScan::load(&scan_config.last_scan_path)?;
    let since = last_scan
        .as_ref()
        .map(|ls| ls.timestamp)
        .unwrap_or_else(|| Utc::now() - chrono::Duration::days(30));

    // Step 2: Collect sessions
    let sessions = reader::collect_sessions(agents, since)?;
    if sessions.is_empty() {
        println!("No new sessions found since last scan.");
        // Still update the watermark so we don't re-scan the same window
        let watermark = LastScan {
            timestamp: Utc::now(),
            session_ids: vec![],
        };
        watermark.save(&scan_config.last_scan_path)?;
        return Ok(vec![]);
    }

    println!("Found {} session(s) to analyze.", sessions.len());

    // Step 3: Load existing skills
    let skills = load_skills(&scan_config.skills_dir)?;
    println!(
        "Loaded {} existing skill(s).",
        skills.len()
    );

    // Step 4: Build prompt and invoke agent
    let prompt = build_prompt(&sessions, &skills);
    println!(
        "Sending {} session path(s) to `{}` for analysis (prompt: {} bytes)...",
        sessions.len(),
        scan_config.agent_command,
        prompt.len()
    );
    println!("Waiting for agent response (this may take several minutes)...");
    let raw_response = invoke_agent(
        &scan_config.agent_command,
        &scan_config.agent_args,
        &prompt,
    )?;
    println!("Agent responded ({} bytes).", raw_response.len());

    // Step 5: Parse response into proposals
    let proposals = parse_response(&raw_response)?;
    println!("Agent proposed {} skill(s).", proposals.len());

    // Step 6: Write proposals to disk
    std::fs::create_dir_all(&scan_config.proposals_dir)?;
    for (i, proposal) in proposals.iter().enumerate() {
        let filename = proposal_filename(proposal, i);
        let path = scan_config.proposals_dir.join(&filename);
        let markdown = proposal
            .to_markdown()
            .context("Failed to serialize proposal to markdown")?;
        std::fs::write(&path, markdown)
            .with_context(|| format!("Failed to write proposal {}", path.display()))?;
    }

    // Step 7: Update watermark
    let watermark = LastScan {
        timestamp: Utc::now(),
        session_ids: sessions.iter().map(|s| s.id.clone()).collect(),
    };
    watermark.save(&scan_config.last_scan_path)?;

    Ok(proposals)
}

/// Generate a deterministic filename for a proposal.
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

/// Load all skills from the skills directory.
fn load_skills(skills_dir: &Path) -> Result<Vec<Skill>> {
    if !skills_dir.exists() {
        return Ok(vec![]);
    }

    let mut skills = Vec::new();
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read skill {}", path.display()))?;
            skills.push(Skill { name, content });
        }
    }
    Ok(skills)
}

/// Build the prompt sent to the generation agent.
///
/// Instead of inlining session content (which can be enormous), the prompt
/// points the agent at the session file paths and lets it read them directly.
fn build_prompt(sessions: &[Session], existing_skills: &[Skill]) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are a skill extraction engine for the `distill` tool.\n\n\
         Your job: read the AI agent session files listed below, look for repeated \
         patterns, workflows, or techniques, and propose reusable skills.\n\n\
         IMPORTANT: Respond ONLY with valid JSON — an array of proposal objects. \
         No markdown fences, no commentary.\n\n\
         Each proposal object must have these fields:\n\
         - \"type\": one of \"new\", \"improve\", \"edit\", \"remove\"\n\
         - \"confidence\": one of \"high\", \"medium\", \"low\"\n\
         - \"target_skill\": string (filename, required for improve/edit/remove, null for new)\n\
         - \"evidence\": array of {\"session\": \"<path>\", \"pattern\": \"<description>\"}\n\
         - \"body\": string containing the full proposed skill content in markdown\n\n",
    );

    // Existing skills so the agent knows what's already been extracted.
    if existing_skills.is_empty() {
        prompt.push_str("## Existing Skills\n\nNone yet.\n\n");
    } else {
        prompt.push_str("## Existing Skills\n\n");
        for skill in existing_skills {
            prompt.push_str(&format!("### {}\n\n{}\n\n---\n\n", skill.name, skill.content));
        }
    }

    // List session file paths — the agent reads the actual content itself.
    prompt.push_str(&format!(
        "## Session Files ({} total)\n\n\
         Read these JSONL session files and analyze them for patterns:\n\n",
        sessions.len()
    ));
    for session in sessions {
        prompt.push_str(&format!(
            "- {} ({}, {})\n",
            session.path.display(),
            session.agent,
            session.timestamp.format("%Y-%m-%d %H:%M"),
        ));
    }

    prompt.push_str(
        "\nAnalyze these session files and produce a JSON array of skill proposals. \
         Focus on repeated patterns that would benefit from being codified as reusable skills.\n",
    );

    prompt
}

/// Invoke the generation agent command and return its stdout.
///
/// The prompt is piped via stdin rather than passed as a command-line argument
/// to avoid hitting the OS `ARG_MAX` limit (`E2BIG` / "Argument list too long").
fn invoke_agent(command: &str, args: &[String], prompt: &str) -> Result<String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // let agent progress output show in real time
        .spawn()
        .with_context(|| format!("Failed to execute agent command: {command}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .with_context(|| format!("Failed to write prompt to {command} stdin"))?;
        // stdin is dropped here, closing the pipe so the child sees EOF
    }

    // Print a heartbeat every 30 s so the user knows we're not stuck.
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
        eprint!("\r                            \r"); // clear the line
    });

    let output = child
        .wait_with_output()
        .with_context(|| format!("Failed to wait for agent command: {command}"))?;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = heartbeat.join();

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Agent command `{command}` failed with status {}:\n{}",
            output.status,
            stdout.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("Agent output is not valid UTF-8")?;
    Ok(stdout)
}

/// A single raw proposal as returned by the agent (JSON).
#[derive(serde::Deserialize)]
struct RawProposal {
    #[serde(rename = "type")]
    proposal_type: String,
    confidence: String,
    target_skill: Option<String>,
    evidence: Vec<RawEvidence>,
    body: String,
}

#[derive(serde::Deserialize)]
struct RawEvidence {
    session: String,
    pattern: String,
}

/// Claude's `--output-format json` envelope.
#[derive(serde::Deserialize)]
struct ClaudeEnvelope {
    structured_output: Option<serde_json::Value>,
    result: Option<String>,
}

/// Wrapper object returned when using `--json-schema` with our schema.
/// The schema requires `{"proposals": [...]}` because the API mandates a
/// top-level object.
#[derive(serde::Deserialize)]
struct ProposalWrapper {
    proposals: Vec<RawProposal>,
}

/// Parse the agent's JSON response into typed Proposal structs.
///
/// Handles multiple formats:
/// 1. Claude JSON envelope with `structured_output.proposals` (preferred)
/// 2. Claude JSON envelope with `result` text
/// 3. Raw JSON `{"proposals": [...]}` wrapper
/// 4. Raw JSON array `[...]`
pub fn parse_response(raw: &str) -> Result<Vec<Proposal>> {
    let trimmed = raw.trim();

    // Try to extract structured_output from a Claude JSON envelope first.
    let proposals_value: serde_json::Value = if let Ok(envelope) =
        serde_json::from_str::<ClaudeEnvelope>(trimmed)
    {
        if let Some(structured) = envelope.structured_output {
            structured
        } else if let Some(ref text) = envelope.result {
            extract_json_value(text)?
        } else {
            extract_json_value(trimmed)?
        }
    } else {
        extract_json_value(trimmed)?
    };

    // The value is either {"proposals": [...]} (from --json-schema) or [...] (raw).
    let raw_proposals: Vec<RawProposal> =
        if let Ok(wrapper) = serde_json::from_value::<ProposalWrapper>(proposals_value.clone()) {
            wrapper.proposals
        } else {
            serde_json::from_value(proposals_value)
                .context("Failed to parse agent response as proposals")?
        };

    let mut proposals = Vec::new();
    for rp in raw_proposals {
        let proposal_type = match rp.proposal_type.as_str() {
            "new" => ProposalType::New,
            "improve" => ProposalType::Improve,
            "edit" => ProposalType::Edit,
            "remove" => ProposalType::Remove,
            other => bail!("Unknown proposal type: {other}"),
        };
        let confidence = match rp.confidence.as_str() {
            "high" => Confidence::High,
            "medium" => Confidence::Medium,
            "low" => Confidence::Low,
            other => bail!("Unknown confidence level: {other}"),
        };
        let evidence = rp
            .evidence
            .into_iter()
            .map(|e| Evidence {
                session: e.session,
                pattern: e.pattern,
            })
            .collect();

        proposals.push(Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type,
                confidence,
                target_skill: rp.target_skill,
                evidence,
                created: Utc::now(),
            },
            body: rp.body,
            filename: None,
        });
    }

    Ok(proposals)
}

/// Extract a JSON value from raw text that may contain markdown code fences.
fn extract_json_value(text: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();

    let json_str = if trimmed.starts_with("```") {
        let inner = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        inner
            .rfind("```")
            .map(|pos| &inner[..pos])
            .unwrap_or(inner)
            .trim()
    } else {
        trimmed
    };

    serde_json::from_str(json_str).context("Failed to parse agent response as JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use std::path::PathBuf;

    #[test]
    fn test_parse_response_valid_json() {
        let json = "[{\"type\":\"new\",\"confidence\":\"high\",\"target_skill\":null,\"evidence\":[{\"session\":\"~/.claude/sessions/abc.jsonl\",\"pattern\":\"User ran git rebase 5 times\"}],\"body\":\"# Git Rebase Workflow\\n\\nAlways use interactive rebase.\"}]";

        let proposals = parse_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].frontmatter.proposal_type, ProposalType::New);
        assert_eq!(proposals[0].frontmatter.confidence, Confidence::High);
        assert!(proposals[0].frontmatter.target_skill.is_none());
        assert_eq!(proposals[0].frontmatter.evidence.len(), 1);
        assert!(proposals[0].body.contains("Git Rebase Workflow"));
    }

    #[test]
    fn test_parse_response_with_code_fences() {
        let json = "```json\n[\n{\"type\":\"new\",\"confidence\":\"medium\",\"target_skill\":null,\"evidence\":[],\"body\":\"test\"}\n]\n```";
        let proposals = parse_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].frontmatter.confidence, Confidence::Medium);
    }

    #[test]
    fn test_parse_response_multiple_proposals() {
        let json = r#"[
            {
                "type": "new",
                "confidence": "high",
                "target_skill": null,
                "evidence": [],
                "body": "skill 1"
            },
            {
                "type": "improve",
                "confidence": "low",
                "target_skill": "existing-skill.md",
                "evidence": [{"session": "s1", "pattern": "p1"}],
                "body": "improved skill"
            },
            {
                "type": "remove",
                "confidence": "medium",
                "target_skill": "stale-skill.md",
                "evidence": [{"session": "s2", "pattern": "never used"}],
                "body": "This skill is no longer relevant."
            }
        ]"#;

        let proposals = parse_response(json).unwrap();
        assert_eq!(proposals.len(), 3);
        assert_eq!(proposals[0].frontmatter.proposal_type, ProposalType::New);
        assert_eq!(proposals[1].frontmatter.proposal_type, ProposalType::Improve);
        assert_eq!(
            proposals[1].frontmatter.target_skill.as_deref(),
            Some("existing-skill.md")
        );
        assert_eq!(proposals[2].frontmatter.proposal_type, ProposalType::Remove);
    }

    #[test]
    fn test_parse_response_invalid_json() {
        let result = parse_response("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_response_empty_array() {
        let proposals = parse_response("[]").unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_parse_response_unknown_type() {
        let json = r#"[{"type":"unknown","confidence":"high","target_skill":null,"evidence":[],"body":"x"}]"#;
        let result = parse_response(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown proposal type"));
    }

    #[test]
    fn test_build_prompt_includes_sessions_and_skills() {
        let sessions = vec![Session {
            id: "s1".into(),
            agent: AgentKind::Claude,
            path: PathBuf::from("/fake/s1.jsonl"),
            timestamp: Utc::now(),
            content: String::new(),
        }];

        let skills = vec![Skill {
            name: "deploy".into(),
            content: "# Deploy\nRun deploy.sh".into(),
        }];

        let prompt = build_prompt(&sessions, &skills);
        assert!(prompt.contains("Session Files"));
        assert!(prompt.contains("/fake/s1.jsonl"));
        assert!(prompt.contains("Existing Skills"));
        assert!(prompt.contains("deploy"));
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn test_build_prompt_no_existing_skills() {
        let sessions = vec![Session {
            id: "s1".into(),
            agent: AgentKind::Claude,
            path: PathBuf::from("/fake/s1.jsonl"),
            timestamp: Utc::now(),
            content: String::new(),
        }];

        let prompt = build_prompt(&sessions, &[]);
        assert!(prompt.contains("None yet"));
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let skills = load_skills(dir.path()).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_skills_reads_md_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("git-workflow.md"), "# Git Workflow\nDo stuff").unwrap();
        std::fs::write(dir.path().join("not-a-skill.txt"), "ignore me").unwrap();

        let skills = load_skills(dir.path()).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "git-workflow");
        assert!(skills[0].content.contains("Git Workflow"));
    }

    #[test]
    fn test_load_skills_nonexistent_dir() {
        let skills = load_skills(Path::new("/nonexistent/skills")).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_run_scan_writes_proposals_and_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path().join("proposals");
        let skills_dir = dir.path().join("skills");
        let last_scan_path = dir.path().join("last-scan.json");

        std::fs::create_dir_all(&skills_dir).unwrap();

        // Create a mock agent script that reads stdin and returns valid JSON.
        // The prompt is piped via stdin (not as a positional arg).
        let mock_script = dir.path().join("mock-agent.sh");
        let script_content = "#!/bin/sh\ncat > /dev/null\nprintf '%s' '[{\"type\":\"new\",\"confidence\":\"high\",\"target_skill\":null,\"evidence\":[{\"session\":\"test\",\"pattern\":\"test pattern\"}],\"body\":\"# Test Skill\"}]'\n";
        std::fs::write(&mock_script, script_content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Create a mock agent that returns one session
        struct TestAgent {
            sessions: Vec<Session>,
        }
        impl Agent for TestAgent {
            fn kind(&self) -> AgentKind {
                AgentKind::Claude
            }
            fn read_sessions(&self, _since: chrono::DateTime<Utc>) -> Result<Vec<Session>> {
                Ok(self.sessions.clone())
            }
            fn write_skill(&self, _skill: &Skill) -> Result<()> {
                Ok(())
            }
            fn config_dir(&self) -> PathBuf {
                PathBuf::from("/fake")
            }
        }

        let agents: Vec<Box<dyn Agent>> = vec![Box::new(TestAgent {
            sessions: vec![Session {
                id: "test-session-1".into(),
                agent: AgentKind::Claude,
                path: PathBuf::from("/fake/test.jsonl"),
                timestamp: Utc::now(),
                content: "User did something interesting".into(),
            }],
        })];

        let scan_config = ScanConfig {
            agent_command: mock_script.to_string_lossy().to_string(),
            agent_args: vec![],
            skills_dir,
            proposals_dir: proposals_dir.clone(),
            last_scan_path: last_scan_path.clone(),
        };

        let result = run_scan(&agents, &scan_config).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].frontmatter.proposal_type, ProposalType::New);

        // Verify proposals were written to disk
        let entries: Vec<_> = std::fs::read_dir(&proposals_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);

        // Verify watermark was saved
        let watermark = LastScan::load(&last_scan_path).unwrap().unwrap();
        assert_eq!(watermark.session_ids, vec!["test-session-1"]);
    }

    #[test]
    fn test_run_scan_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path().join("proposals");
        let skills_dir = dir.path().join("skills");
        let last_scan_path = dir.path().join("last-scan.json");

        std::fs::create_dir_all(&skills_dir).unwrap();

        struct EmptyAgent;
        impl Agent for EmptyAgent {
            fn kind(&self) -> AgentKind {
                AgentKind::Claude
            }
            fn read_sessions(&self, _since: chrono::DateTime<Utc>) -> Result<Vec<Session>> {
                Ok(vec![])
            }
            fn write_skill(&self, _skill: &Skill) -> Result<()> {
                Ok(())
            }
            fn config_dir(&self) -> PathBuf {
                PathBuf::from("/fake")
            }
        }

        let agents: Vec<Box<dyn Agent>> = vec![Box::new(EmptyAgent)];
        let scan_config = ScanConfig {
            agent_command: "unused".into(),
            agent_args: vec![],
            skills_dir,
            proposals_dir,
            last_scan_path: last_scan_path.clone(),
        };

        let result = run_scan(&agents, &scan_config).unwrap();
        assert!(result.is_empty());

        // Watermark should still be saved
        assert!(LastScan::load(&last_scan_path).unwrap().is_some());
    }

    #[test]
    fn test_proposal_filename_format() {
        let proposal = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::New,
                confidence: Confidence::High,
                target_skill: None,
                evidence: vec![],
                created: Utc::now(),
            },
            body: "test".into(),
            filename: None,
        };

        let name = proposal_filename(&proposal, 0);
        assert!(name.starts_with("new-"));
        assert!(name.ends_with("-0.md"));
    }

    #[test]
    fn test_agent_command_for_claude() {
        let (cmd, args) = agent_command_for("claude");
        assert_eq!(cmd, "claude");
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"--json-schema".to_string()));
    }

    #[test]
    fn test_agent_command_for_codex() {
        let (cmd, args) = agent_command_for("codex");
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
    }

    #[test]
    fn test_agent_command_for_unknown() {
        let (cmd, args) = agent_command_for("custom-agent");
        assert_eq!(cmd, "custom-agent");
        assert!(args.is_empty());
    }

    #[test]
    fn test_parse_response_claude_envelope_with_wrapper() {
        let envelope = r##"{"type":"result","subtype":"success","result":"","structured_output":{"proposals":[{"type":"new","confidence":"high","target_skill":null,"evidence":[{"session":"s1","pattern":"test"}],"body":"# Skill"}]}}"##;
        let proposals = parse_response(envelope).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].frontmatter.proposal_type, ProposalType::New);
    }

    #[test]
    fn test_parse_response_claude_envelope_empty() {
        let envelope = r#"{"type":"result","subtype":"success","result":"","structured_output":{"proposals":[]}}"#;
        let proposals = parse_response(envelope).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_parse_response_raw_array_still_works() {
        // Agents without structured output may return a raw JSON array
        let json = r#"[{"type":"new","confidence":"medium","target_skill":null,"evidence":[],"body":"test"}]"#;
        let proposals = parse_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
    }
}
