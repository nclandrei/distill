use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use crate::agents::{Agent, AgentKind, from_kind};
use crate::config::Config;
use crate::proposals::{Confidence, Evidence, Proposal, ProposalTarget, ProposalType};
use crate::review::{self, ReviewDecision};

const JSON_STDIO_SENTINEL: &str = "-";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum JsonDecision {
    Accept,
    Reject,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ReviewProposalSpec {
    filename: String,
    #[serde(rename = "type")]
    proposal_type: ProposalType,
    confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<ProposalTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_skill: Option<String>,
    created: DateTime<Utc>,
    #[serde(default)]
    evidence: Vec<Evidence>,
    body: String,
    #[serde(default)]
    decision: Option<JsonDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ReviewSpec {
    #[serde(default = "review_format_version")]
    format_version: u32,
    generated_at: DateTime<Utc>,
    proposals: Vec<ReviewProposalSpec>,
}

fn review_format_version() -> u32 {
    1
}

pub fn run(write_json: Option<&Path>, apply_json: Option<&Path>) -> Result<()> {
    let proposals_dir = Config::proposals_dir();
    let skills_dir = Config::skills_dir();
    let history_dir = Config::history_dir();

    match (write_json, apply_json) {
        (Some(path), None) => write_pending_json(path, &proposals_dir),
        (None, Some(path)) => apply_review_json(path, &proposals_dir, &skills_dir, &history_dir),
        (None, None) => run_interactive(&proposals_dir, &skills_dir, &history_dir),
        (Some(_), Some(_)) => unreachable!("clap enforces flag conflicts"),
    }
}

fn run_interactive(proposals_dir: &Path, skills_dir: &Path, history_dir: &Path) -> Result<()> {
    // Quick early-exit: avoid entering the interactive loop when there is
    // nothing to review.
    let proposals = review::load_proposals(proposals_dir)?;
    if proposals.is_empty() {
        println!("No pending proposals to review.");
        return Ok(());
    }

    review::run_review_interactive(proposals_dir, skills_dir, history_dir)?;
    Ok(())
}

fn write_pending_json(path: &Path, proposals_dir: &Path) -> Result<()> {
    let proposals = review::load_proposals(proposals_dir)?;
    let spec = ReviewSpec {
        format_version: review_format_version(),
        generated_at: Utc::now(),
        proposals: proposals.iter().map(proposal_to_spec).collect(),
    };
    let json = serde_json::to_string_pretty(&spec).context("Failed to serialize review JSON")?;
    write_text(path, &json)?;

    if !is_stdio(path) {
        println!(
            "Wrote {} pending proposal(s) to {}",
            spec.proposals.len(),
            path.display()
        );
    }
    Ok(())
}

fn apply_review_json(
    path: &Path,
    proposals_dir: &Path,
    skills_dir: &Path,
    history_dir: &Path,
) -> Result<()> {
    let raw = read_text(path)?;
    let spec: ReviewSpec = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse review JSON from {}", display_path(path)))?;
    validate_spec(&spec)?;

    let proposals = review::load_proposals(proposals_dir)?;
    if proposals.is_empty() {
        println!("No pending proposals to review.");
        return Ok(());
    }

    let mut decisions_by_filename = HashMap::new();
    for proposal in &spec.proposals {
        let Some(decision) = &proposal.decision else {
            continue;
        };
        if decisions_by_filename
            .insert(proposal.filename.clone(), decision.clone())
            .is_some()
        {
            bail!(
                "Review JSON has multiple decisions for '{}'.",
                proposal.filename
            );
        }
    }

    let mut decisions = Vec::with_capacity(proposals.len());
    for proposal in &proposals {
        let filename = proposal
            .filename
            .as_deref()
            .context("Proposal is missing a filename")?;
        let decision = decisions_by_filename
            .remove(filename)
            .unwrap_or(JsonDecision::Skip);
        decisions.push(decision_to_review(decision));
    }

    let summary = review::run_review(
        &proposals,
        &decisions,
        skills_dir,
        history_dir,
        proposals_dir,
    )?;

    println!("Review decisions applied from JSON.");
    println!("  Accepted : {}", summary.accepted);
    println!("  Rejected : {}", summary.rejected);
    println!("  Skipped  : {}", summary.skipped);

    if !decisions_by_filename.is_empty() {
        let mut stale = decisions_by_filename.keys().cloned().collect::<Vec<_>>();
        stale.sort();
        eprintln!(
            "Warning: {} JSON decision(s) did not match pending proposals: {}",
            stale.len(),
            stale.join(", ")
        );
    }

    if summary.accepted_skill_targets > 0 {
        println!();
        println!("Syncing skills to agents...");
        match sync_after_review(skills_dir) {
            Ok(report) => {
                println!(
                    "  Synced {} operation(s) across agents ({} skipped).",
                    report.synced, report.skipped,
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

    Ok(())
}

fn sync_after_review(skills_dir: &Path) -> Result<crate::sync::SyncReport> {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    let config = Config::load().unwrap_or_default();
    let agents: Vec<Box<dyn Agent>> = config
        .agents
        .iter()
        .filter(|entry| entry.enabled)
        .filter_map(|entry| match entry.name.as_str() {
            "claude" => Some(from_kind(AgentKind::Claude, home.clone())),
            "codex" => Some(from_kind(AgentKind::Codex, home.clone())),
            _ => None,
        })
        .collect();

    crate::sync::run_sync(skills_dir, &agents)
}

fn proposal_to_spec(proposal: &Proposal) -> ReviewProposalSpec {
    ReviewProposalSpec {
        filename: proposal
            .filename
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string()),
        proposal_type: proposal.frontmatter.proposal_type.clone(),
        confidence: proposal.frontmatter.confidence.clone(),
        target: proposal.frontmatter.resolved_target(),
        target_skill: None,
        created: proposal.frontmatter.created,
        evidence: proposal.frontmatter.evidence.clone(),
        body: proposal.body.clone(),
        decision: None,
    }
}

fn decision_to_review(decision: JsonDecision) -> ReviewDecision {
    match decision {
        JsonDecision::Accept => ReviewDecision::Accept,
        JsonDecision::Reject => ReviewDecision::Reject,
        JsonDecision::Skip => ReviewDecision::Skip,
    }
}

fn validate_spec(spec: &ReviewSpec) -> Result<()> {
    if spec.format_version != review_format_version() {
        bail!(
            "Unsupported review JSON format_version {}. Expected {}.",
            spec.format_version,
            review_format_version()
        );
    }

    let mut seen = HashMap::new();
    for proposal in &spec.proposals {
        if proposal.filename.trim().is_empty() {
            bail!("Proposal entries in review JSON must include `filename`.");
        }
        if seen.insert(proposal.filename.clone(), true).is_some() {
            bail!(
                "Review JSON contains duplicate proposal entry '{}'.",
                proposal.filename
            );
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    if is_stdio(path) {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("Failed to read review JSON from stdin")?;
        return Ok(input);
    }
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn write_text(path: &Path, content: &str) -> Result<()> {
    if is_stdio(path) {
        println!("{content}");
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

fn is_stdio(path: &Path) -> bool {
    path == Path::new(JSON_STDIO_SENTINEL)
}

fn display_path(path: &Path) -> String {
    if is_stdio(path) {
        "stdin".to_string()
    } else {
        path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{ProposalFrontmatter, ProposalTarget, ProposalType};

    #[test]
    fn test_decision_to_review() {
        assert_eq!(
            decision_to_review(JsonDecision::Accept),
            ReviewDecision::Accept
        );
        assert_eq!(
            decision_to_review(JsonDecision::Reject),
            ReviewDecision::Reject
        );
        assert_eq!(decision_to_review(JsonDecision::Skip), ReviewDecision::Skip);
    }

    #[test]
    fn test_validate_spec_rejects_duplicate_filename() {
        let proposal = ReviewProposalSpec {
            filename: "same.md".to_string(),
            proposal_type: ProposalType::New,
            confidence: Confidence::High,
            target: None,
            target_skill: None,
            created: Utc::now(),
            evidence: vec![],
            body: "body".to_string(),
            decision: Some(JsonDecision::Accept),
        };
        let spec = ReviewSpec {
            format_version: 1,
            generated_at: Utc::now(),
            proposals: vec![proposal.clone(), proposal],
        };
        assert!(validate_spec(&spec).is_err());
    }

    #[test]
    fn test_proposal_to_spec_keeps_filename() {
        let proposal = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::Improve,
                confidence: Confidence::Medium,
                target: Some(ProposalTarget::Skill {
                    name: "git-workflow".to_string(),
                }),
                target_skill: None,
                evidence: vec![],
                created: Utc::now(),
            },
            body: "# Skill".to_string(),
            filename: Some("skill.md".to_string()),
        };
        let spec = proposal_to_spec(&proposal);
        assert_eq!(spec.filename, "skill.md");
        assert_eq!(spec.proposal_type, ProposalType::Improve);
        assert_eq!(spec.confidence, Confidence::Medium);
        assert_eq!(
            spec.target,
            Some(ProposalTarget::Skill {
                name: "git-workflow".to_string()
            })
        );
        assert_eq!(spec.target_skill, None);
    }
}
