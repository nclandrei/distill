// Review flow — interactive proposal review using stdin/stdout.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::agents::{from_kind, AgentKind};
use crate::config::Config;
use crate::proposals::Proposal;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A user's decision for a single proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewDecision {
    Accept,
    Reject,
    Skip,
}

/// A record of a proposal decision written to the history log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub proposal_filename: String,
    pub decision: String, // "accepted" or "rejected"
    pub decided_at: DateTime<Utc>,
}

/// Summary of a completed review session.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewSummary {
    pub accepted: usize,
    pub rejected: usize,
    pub skipped: usize,
}

// ---------------------------------------------------------------------------
// Core logic (path-based, fully testable without stdin)
// ---------------------------------------------------------------------------

/// Load all `.md` files from `proposals_dir`, parsing each as a `Proposal`.
/// Sets the `filename` field on each parsed proposal.
/// Returns an empty `Vec` (no error) if the directory does not exist.
pub fn load_proposals(proposals_dir: &Path) -> Result<Vec<Proposal>> {
    if !proposals_dir.exists() {
        return Ok(vec![]);
    }

    let mut proposals = Vec::new();

    for entry in fs::read_dir(proposals_dir)?.flatten() {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown.md")
            .to_string();

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read proposal: {}", path.display()))?;

        match Proposal::from_markdown(&content) {
            Ok(mut proposal) => {
                proposal.filename = Some(filename);
                proposals.push(proposal);
            }
            Err(e) => {
                // Skip malformed proposals with a warning rather than aborting.
                eprintln!("Warning: skipping malformed proposal {filename}: {e}");
            }
        }
    }

    Ok(proposals)
}

/// Append a JSON line to `history_dir/decisions.jsonl`.
/// Creates the file and parent directories if they do not yet exist.
pub fn log_decision(history_dir: &Path, entry: &HistoryEntry) -> Result<()> {
    fs::create_dir_all(history_dir).with_context(|| {
        format!(
            "Failed to create history directory: {}",
            history_dir.display()
        )
    })?;

    let decisions_path = history_dir.join("decisions.jsonl");
    let line = serde_json::to_string(entry).context("Failed to serialize history entry")?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&decisions_path)
        .with_context(|| format!("Failed to open {}", decisions_path.display()))?;

    writeln!(file, "{}", line)
        .with_context(|| format!("Failed to write to {}", decisions_path.display()))?;

    Ok(())
}

/// Determine the skill filename for a proposal.
///
/// Priority:
/// 1. `target_skill` frontmatter field — normalised to a slug, with `.md` appended.
/// 2. The proposal's own filename (kept as-is, since it already ends with `.md`).
/// 3. A timestamp-based fallback.
fn skill_filename_for(proposal: &Proposal) -> String {
    if let Some(target) = &proposal.frontmatter.target_skill {
        let slug = target
            .to_lowercase()
            .replace(' ', "-")
            .replace('_', "-");
        if slug.ends_with(".md") {
            slug
        } else {
            format!("{slug}.md")
        }
    } else if let Some(filename) = &proposal.filename {
        filename.clone()
    } else {
        format!("skill-{}.md", Utc::now().timestamp())
    }
}

/// Accept a proposal: write its body as a skill, log the decision, and delete the proposal file.
pub fn accept_proposal(
    proposal: &Proposal,
    skills_dir: &Path,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<()> {
    let skill_file = skill_filename_for(proposal);

    // Write proposal body to skills directory.
    fs::create_dir_all(skills_dir).with_context(|| {
        format!(
            "Failed to create skills directory: {}",
            skills_dir.display()
        )
    })?;
    let skill_path = skills_dir.join(&skill_file);
    fs::write(&skill_path, &proposal.body)
        .with_context(|| format!("Failed to write skill to {}", skill_path.display()))?;

    // Log the decision.
    let proposal_filename = proposal
        .filename
        .clone()
        .unwrap_or_else(|| skill_file.clone());
    let entry = HistoryEntry {
        proposal_filename: proposal_filename.clone(),
        decision: "accepted".to_string(),
        decided_at: Utc::now(),
    };
    log_decision(history_dir, &entry)?;

    // Delete the proposal file.
    let proposal_path = proposals_dir.join(&proposal_filename);
    if proposal_path.exists() {
        fs::remove_file(&proposal_path).with_context(|| {
            format!("Failed to delete proposal: {}", proposal_path.display())
        })?;
    }

    Ok(())
}

/// Reject a proposal: log the decision and delete the proposal file.
pub fn reject_proposal(
    proposal: &Proposal,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<()> {
    let proposal_filename = proposal
        .filename
        .clone()
        .unwrap_or_else(|| format!("unknown-{}.md", Utc::now().timestamp()));

    // Log the decision.
    let entry = HistoryEntry {
        proposal_filename: proposal_filename.clone(),
        decision: "rejected".to_string(),
        decided_at: Utc::now(),
    };
    log_decision(history_dir, &entry)?;

    // Delete the proposal file.
    let proposal_path = proposals_dir.join(&proposal_filename);
    if proposal_path.exists() {
        fs::remove_file(&proposal_path).with_context(|| {
            format!("Failed to delete proposal: {}", proposal_path.display())
        })?;
    }

    Ok(())
}

/// Process a slice of proposals with pre-determined decisions.
///
/// This is the core testable logic — no stdin required.
/// Use `run_review_interactive` for the user-facing flow.
pub fn run_review(
    proposals: &[Proposal],
    decisions: &[ReviewDecision],
    skills_dir: &Path,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<ReviewSummary> {
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut skipped = 0usize;

    for (proposal, decision) in proposals.iter().zip(decisions.iter()) {
        match decision {
            ReviewDecision::Accept => {
                accept_proposal(proposal, skills_dir, history_dir, proposals_dir)?;
                accepted += 1;
            }
            ReviewDecision::Reject => {
                reject_proposal(proposal, history_dir, proposals_dir)?;
                rejected += 1;
            }
            ReviewDecision::Skip => {
                skipped += 1;
            }
        }
    }

    Ok(ReviewSummary {
        accepted,
        rejected,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Interactive I/O helpers
// ---------------------------------------------------------------------------

/// Write a prompt string to stdout (no trailing newline), flush, and read one line from stdin.
fn prompt(prompt_str: &str) -> Result<String> {
    print!("{}", prompt_str);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Print a formatted summary of a single proposal to stdout.
fn display_proposal(index: usize, total: usize, proposal: &Proposal) {
    use crate::proposals::{Confidence, ProposalType};

    let filename = proposal.filename.as_deref().unwrap_or("(unknown)");

    let proposal_type = match proposal.frontmatter.proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    };

    let confidence = match proposal.frontmatter.confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    };

    let target = proposal
        .frontmatter
        .target_skill
        .as_deref()
        .unwrap_or("(new skill)");

    println!();
    println!("─────────────────────────────────────────────────────────");
    println!("Proposal {}/{}: {}", index + 1, total, filename);
    println!("  Type       : {proposal_type}");
    println!("  Confidence : {confidence}");
    println!("  Target     : {target}");
    if !proposal.frontmatter.evidence.is_empty() {
        println!("  Evidence   :");
        for ev in &proposal.frontmatter.evidence {
            println!("    - {} ({})", ev.pattern, ev.session);
        }
    }
    println!();
    println!("--- Content ---");
    println!("{}", proposal.body);
    println!("─────────────────────────────────────────────────────────");
}

/// Read a `ReviewDecision` from stdin, looping until the user provides a valid input.
fn read_decision() -> Result<ReviewDecision> {
    loop {
        let input = prompt("  [a]ccept / [r]eject / [s]kip? ")?;
        match input.to_lowercase().as_str() {
            "a" | "accept" => return Ok(ReviewDecision::Accept),
            "r" | "reject" => return Ok(ReviewDecision::Reject),
            "s" | "skip" | "" => return Ok(ReviewDecision::Skip),
            _ => println!("  Please enter 'a' (accept), 'r' (reject), or 's' (skip)."),
        }
    }
}

/// Load config and sync all skills in `skills_dir` to all enabled agents.
fn sync_after_review(skills_dir: &Path) -> Result<crate::sync::SyncReport> {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Load config to discover enabled agents; fall back to the default if config is absent.
    let config = Config::load().unwrap_or_default();

    let agents: Vec<Box<dyn crate::agents::Agent>> = config
        .agents
        .iter()
        .filter(|a| a.enabled)
        .filter_map(|a| match a.name.as_str() {
            "claude" => Some(from_kind(AgentKind::Claude, home.clone())),
            "codex" => Some(from_kind(AgentKind::Codex, home.clone())),
            _ => None,
        })
        .collect();

    crate::sync::run_sync(skills_dir, &agents)
}

/// Run the full interactive review flow.
///
/// Displays each proposal and prompts the user for a decision via stdin.
/// On completion, prints a summary and syncs any accepted skills to all
/// configured agents.
pub fn run_review_interactive(
    proposals_dir: &Path,
    skills_dir: &Path,
    history_dir: &Path,
) -> Result<ReviewSummary> {
    let proposals = load_proposals(proposals_dir)?;

    if proposals.is_empty() {
        println!("No pending proposals to review.");
        return Ok(ReviewSummary {
            accepted: 0,
            rejected: 0,
            skipped: 0,
        });
    }

    let total = proposals.len();
    println!();
    println!("Found {} proposal(s) to review.", total);

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut skipped = 0usize;

    for (i, proposal) in proposals.iter().enumerate() {
        display_proposal(i, total, proposal);
        let decision = read_decision()?;

        match decision {
            ReviewDecision::Accept => {
                accept_proposal(proposal, skills_dir, history_dir, proposals_dir)?;
                accepted += 1;
                println!("  Accepted.");
            }
            ReviewDecision::Reject => {
                reject_proposal(proposal, history_dir, proposals_dir)?;
                rejected += 1;
                println!("  Rejected.");
            }
            ReviewDecision::Skip => {
                skipped += 1;
                println!("  Skipped.");
            }
        }
    }

    println!();
    println!("Review complete.");
    println!("  Accepted : {accepted}");
    println!("  Rejected : {rejected}");
    println!("  Skipped  : {skipped}");

    // Sync accepted skills to all configured agents.
    if accepted > 0 {
        println!();
        println!("Syncing skills to agents...");
        match sync_after_review(skills_dir) {
            Ok(report) => {
                println!(
                    "  Synced {} operation(s) across agents.",
                    report.synced,
                );
                if !report.errors.is_empty() {
                    for err in &report.errors {
                        eprintln!("  Sync warning: {err}");
                    }
                }
            }
            Err(e) => {
                eprintln!("  Sync failed (skills saved, but not propagated to agents): {e}");
            }
        }
    }

    Ok(ReviewSummary {
        accepted,
        rejected,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{Confidence, Evidence, ProposalFrontmatter, ProposalType};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal valid `Proposal` for testing.
    fn make_proposal(filename: &str, target_skill: Option<&str>, body: &str) -> Proposal {
        Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::New,
                confidence: Confidence::High,
                target_skill: target_skill.map(|s| s.to_string()),
                evidence: vec![Evidence {
                    session: "test-session.jsonl".into(),
                    pattern: "test pattern".into(),
                }],
                created: Utc::now(),
            },
            body: body.to_string(),
            filename: Some(filename.to_string()),
        }
    }

    /// Write a `Proposal` as a `.md` file inside `dir`, using its `filename` field.
    fn write_proposal_file(dir: &Path, proposal: &Proposal) {
        let md = proposal.to_markdown().unwrap();
        let fname = proposal.filename.as_deref().unwrap();
        fs::write(dir.join(fname), md).unwrap();
    }

    // -----------------------------------------------------------------------
    // load_proposals
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_proposals_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let proposal = make_proposal("git-workflow.md", None, "# Git Workflow\nAlways rebase.");
        write_proposal_file(proposals_dir, &proposal);

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].filename, Some("git-workflow.md".to_string()));
        assert!(proposals[0].body.contains("Git Workflow"));
    }

    #[test]
    fn test_load_proposals_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let proposals = load_proposals(dir.path()).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_load_proposals_nonexistent_dir() {
        let proposals = load_proposals(Path::new("/nonexistent/proposals/dir")).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_load_proposals_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let proposal = make_proposal("real.md", None, "# Real\nContent.");
        write_proposal_file(proposals_dir, &proposal);
        fs::write(proposals_dir.join("notes.txt"), "not a proposal").unwrap();
        fs::write(proposals_dir.join("data.json"), "{}").unwrap();

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].filename, Some("real.md".to_string()));
    }

    #[test]
    fn test_load_proposals_sets_filename_field() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let p1 = make_proposal("alpha.md", None, "# Alpha\nContent.");
        let p2 = make_proposal("beta.md", None, "# Beta\nContent.");
        write_proposal_file(proposals_dir, &p1);
        write_proposal_file(proposals_dir, &p2);

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 2);

        let filenames: Vec<&str> = proposals
            .iter()
            .map(|p| p.filename.as_deref().unwrap())
            .collect();
        assert!(filenames.contains(&"alpha.md"));
        assert!(filenames.contains(&"beta.md"));
    }

    // -----------------------------------------------------------------------
    // accept_proposal
    // -----------------------------------------------------------------------

    #[test]
    fn test_accept_proposal_writes_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("new-skill.md", None, "# New Skill\nDo the thing.");
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let skill_path = skills_dir.join("new-skill.md");
        assert!(skill_path.exists(), "skill file should be created");
        let content = fs::read_to_string(&skill_path).unwrap();
        assert!(content.contains("New Skill"));
        assert!(content.contains("Do the thing."));
    }

    #[test]
    fn test_accept_proposal_removes_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("delete-me.md", None, "# Delete Me\nContent.");
        let proposal_path = proposals_dir.join("delete-me.md");
        write_proposal_file(&proposals_dir, &proposal);

        assert!(proposal_path.exists(), "proposal file should exist before accept");
        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();
        assert!(
            !proposal_path.exists(),
            "proposal file should be deleted after accept"
        );
    }

    #[test]
    fn test_accept_proposal_logs_to_history() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("logged.md", None, "# Logged\nContent.");
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let decisions_path = history_dir.join("decisions.jsonl");
        assert!(decisions_path.exists(), "decisions.jsonl should be created");
        let content = fs::read_to_string(&decisions_path).unwrap();
        assert!(content.contains("\"accepted\""), "should log accepted decision");
        assert!(content.contains("logged.md"), "should include proposal filename");
    }

    #[test]
    fn test_accept_proposal_uses_target_skill_filename() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        // target_skill set: skill should be written to that name, not the proposal filename.
        let proposal = make_proposal(
            "improve-123.md",
            Some("git-workflow"),
            "# Git Workflow\nUpdated content.",
        );
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let skill_path = skills_dir.join("git-workflow.md");
        assert!(
            skill_path.exists(),
            "skill should be written to the target_skill path"
        );
        let other_path = skills_dir.join("improve-123.md");
        assert!(
            !other_path.exists(),
            "should NOT write to the proposal filename when target_skill is set"
        );
    }

    // -----------------------------------------------------------------------
    // reject_proposal
    // -----------------------------------------------------------------------

    #[test]
    fn test_reject_proposal_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("reject-me.md", None, "# Reject Me\nContent.");
        let proposal_path = proposals_dir.join("reject-me.md");
        write_proposal_file(&proposals_dir, &proposal);

        assert!(proposal_path.exists(), "proposal file should exist before reject");
        reject_proposal(&proposal, &history_dir, &proposals_dir).unwrap();
        assert!(
            !proposal_path.exists(),
            "proposal file should be deleted after reject"
        );
    }

    #[test]
    fn test_reject_proposal_logs_to_history() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("reject-log.md", None, "# Reject Log\nContent.");
        write_proposal_file(&proposals_dir, &proposal);

        reject_proposal(&proposal, &history_dir, &proposals_dir).unwrap();

        let decisions_path = history_dir.join("decisions.jsonl");
        assert!(decisions_path.exists(), "decisions.jsonl should be created");
        let content = fs::read_to_string(&decisions_path).unwrap();
        assert!(content.contains("\"rejected\""), "should log rejected decision");
        assert!(
            content.contains("reject-log.md"),
            "should include proposal filename"
        );
    }

    // -----------------------------------------------------------------------
    // log_decision
    // -----------------------------------------------------------------------

    #[test]
    fn test_log_decision_appends() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");

        let entry1 = HistoryEntry {
            proposal_filename: "first.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };
        let entry2 = HistoryEntry {
            proposal_filename: "second.md".to_string(),
            decision: "rejected".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry1).unwrap();
        log_decision(&history_dir, &entry2).unwrap();

        let path = history_dir.join("decisions.jsonl");
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 2, "should have exactly 2 lines");
        assert!(lines[0].contains("first.md"));
        assert!(lines[0].contains("accepted"));
        assert!(lines[1].contains("second.md"));
        assert!(lines[1].contains("rejected"));
    }

    #[test]
    fn test_log_decision_creates_history_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Deeply nested path that does not yet exist.
        let history_dir = dir.path().join("a").join("b").join("history");

        let entry = HistoryEntry {
            proposal_filename: "p.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry).unwrap();
        assert!(
            history_dir.join("decisions.jsonl").exists(),
            "decisions.jsonl should be created even in a deeply nested directory"
        );
    }

    #[test]
    fn test_log_decision_entry_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");

        let entry = HistoryEntry {
            proposal_filename: "valid-json.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry).unwrap();

        let path = history_dir.join("decisions.jsonl");
        let content = fs::read_to_string(&path).unwrap();
        for line in content.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .expect("each line should be valid JSON");
            assert_eq!(
                parsed["proposal_filename"].as_str().unwrap(),
                "valid-json.md"
            );
            assert_eq!(parsed["decision"].as_str().unwrap(), "accepted");
        }
    }

    // -----------------------------------------------------------------------
    // run_review (summary counts — no stdin required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_review_summary_counts() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("p1.md", None, "# P1\nContent one.");
        let p2 = make_proposal("p2.md", None, "# P2\nContent two.");
        let p3 = make_proposal("p3.md", None, "# P3\nContent three.");

        for p in [&p1, &p2, &p3] {
            write_proposal_file(&proposals_dir, p);
        }

        let proposals = vec![p1, p2, p3];
        let decisions = vec![
            ReviewDecision::Accept,
            ReviewDecision::Reject,
            ReviewDecision::Skip,
        ];

        let summary =
            run_review(&proposals, &decisions, &skills_dir, &history_dir, &proposals_dir)
                .unwrap();

        assert_eq!(summary.accepted, 1);
        assert_eq!(summary.rejected, 1);
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn test_review_all_accept() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("a1.md", None, "# A1\nContent.");
        let p2 = make_proposal("a2.md", None, "# A2\nContent.");
        write_proposal_file(&proposals_dir, &p1);
        write_proposal_file(&proposals_dir, &p2);

        let proposals = vec![p1, p2];
        let decisions = vec![ReviewDecision::Accept, ReviewDecision::Accept];

        let summary =
            run_review(&proposals, &decisions, &skills_dir, &history_dir, &proposals_dir)
                .unwrap();

        assert_eq!(summary.accepted, 2);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.skipped, 0);

        // Both skill files should exist.
        assert!(skills_dir.join("a1.md").exists());
        assert!(skills_dir.join("a2.md").exists());
    }

    #[test]
    fn test_review_all_reject() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("r1.md", None, "# R1\nContent.");
        let p2 = make_proposal("r2.md", None, "# R2\nContent.");
        write_proposal_file(&proposals_dir, &p1);
        write_proposal_file(&proposals_dir, &p2);

        let proposals = vec![p1, p2];
        let decisions = vec![ReviewDecision::Reject, ReviewDecision::Reject];

        let summary =
            run_review(&proposals, &decisions, &skills_dir, &history_dir, &proposals_dir)
                .unwrap();

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.rejected, 2);
        assert_eq!(summary.skipped, 0);

        // No skill files should have been created.
        assert!(!skills_dir.exists() || fs::read_dir(&skills_dir).unwrap().next().is_none());
    }

    #[test]
    fn test_review_all_skip() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("s1.md", None, "# S1\nContent.");
        // Do not write to disk — skip should not touch the filesystem at all.

        let proposals = vec![p1];
        let decisions = vec![ReviewDecision::Skip];

        let summary =
            run_review(&proposals, &decisions, &skills_dir, &history_dir, &proposals_dir)
                .unwrap();

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.skipped, 1);

        // History should be untouched.
        assert!(!history_dir.join("decisions.jsonl").exists());
    }
}
