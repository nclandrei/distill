use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agents::{Agent, Session, Skill};
use crate::config::Config;
use crate::proposals::{Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalType};
use crate::scanner::reader::{self, LastScan};

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
fn agent_command_for(agent_name: &str) -> (String, Vec<String>) {
    match agent_name {
        "claude" => ("claude".into(), vec!["--print".into()]),
        "codex" => ("codex".into(), vec!["--quiet".into()]),
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
    println!("Loaded {} existing skill(s).", skills.len());

    // Step 4: Build prompt and invoke agent
    let prompt = build_prompt(&sessions, &skills);
    let raw_response = invoke_agent(&scan_config.agent_command, &scan_config.agent_args, &prompt)?;

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
fn build_prompt(sessions: &[Session], existing_skills: &[Skill]) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are a skill extraction engine for the `distill` tool. \
         Analyze the following AI agent sessions and existing skills, then propose new or \
         improved skills.\n\n\
         IMPORTANT: Respond ONLY with valid JSON — an array of proposal objects. \
         No markdown fences, no commentary.\n\n\
         Each proposal object must have these fields:\n\
         - \"type\": one of \"new\", \"improve\", \"edit\", \"remove\"\n\
         - \"confidence\": one of \"high\", \"medium\", \"low\"\n\
         - \"target_skill\": string (filename, required for improve/edit/remove, null for new)\n\
         - \"evidence\": array of {\"session\": \"<path>\", \"pattern\": \"<description>\"}\n\
         - \"body\": string containing the full proposed skill content in markdown\n\n",
    );

    // Include existing skills context
    if existing_skills.is_empty() {
        prompt.push_str("## Existing Skills\n\nNone yet.\n\n");
    } else {
        prompt.push_str("## Existing Skills\n\n");
        for skill in existing_skills {
            prompt.push_str(&format!(
                "### {}\n\n{}\n\n---\n\n",
                skill.name, skill.content
            ));
        }
    }

    // Include sessions
    prompt.push_str("## Recent Sessions\n\n");
    for session in sessions {
        prompt.push_str(&format!(
            "### Session {} ({}, {})\n\n{}\n\n---\n\n",
            session.id,
            session.agent,
            session.timestamp.format("%Y-%m-%d %H:%M"),
            session.content
        ));
    }

    prompt.push_str(
        "Now analyze the sessions above and produce a JSON array of proposals. \
         Focus on repeated patterns that would benefit from being codified as skills.\n",
    );

    prompt
}

/// Invoke the generation agent command and return its stdout.
fn invoke_agent(command: &str, args: &[String], prompt: &str) -> Result<String> {
    let output = Command::new(command)
        .args(args)
        .arg(prompt)
        .output()
        .with_context(|| format!("Failed to execute agent command: {command}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Agent command `{command}` failed with status {}:\n{}",
            output.status,
            stderr
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

/// Parse the agent's JSON response into typed Proposal structs.
pub fn parse_response(raw: &str) -> Result<Vec<Proposal>> {
    let trimmed = raw.trim();

    // Strip markdown code fences if the agent included them
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

    let raw_proposals: Vec<RawProposal> =
        serde_json::from_str(json_str).context("Failed to parse agent response as JSON array")?;

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
        assert_eq!(
            proposals[1].frontmatter.proposal_type,
            ProposalType::Improve
        );
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown proposal type")
        );
    }

    #[test]
    fn test_build_prompt_includes_sessions_and_skills() {
        let sessions = vec![Session {
            id: "s1".into(),
            agent: AgentKind::Claude,
            path: PathBuf::from("/fake/s1.jsonl"),
            timestamp: Utc::now(),
            content: "User ran deploy script".into(),
        }];

        let skills = vec![Skill {
            name: "deploy".into(),
            content: "# Deploy\nRun deploy.sh".into(),
        }];

        let prompt = build_prompt(&sessions, &skills);
        assert!(prompt.contains("Recent Sessions"));
        assert!(prompt.contains("User ran deploy script"));
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
            content: "session".into(),
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
        std::fs::write(
            dir.path().join("git-workflow.md"),
            "# Git Workflow\nDo stuff",
        )
        .unwrap();
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

        // Create a mock agent script that returns valid JSON
        let mock_script = dir.path().join("mock-agent.sh");
        let script_content = "#!/bin/sh\nprintf '%s' '[{\"type\":\"new\",\"confidence\":\"high\",\"target_skill\":null,\"evidence\":[{\"session\":\"test\",\"pattern\":\"test pattern\"}],\"body\":\"# Test Skill\"}]'\n";
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
        assert_eq!(args, vec!["--print"]);
    }

    #[test]
    fn test_agent_command_for_codex() {
        let (cmd, args) = agent_command_for("codex");
        assert_eq!(cmd, "codex");
        assert_eq!(args, vec!["--quiet"]);
    }

    #[test]
    fn test_agent_command_for_unknown() {
        let (cmd, args) = agent_command_for("custom-agent");
        assert_eq!(cmd, "custom-agent");
        assert!(args.is_empty());
    }
}
