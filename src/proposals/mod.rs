use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProposalType {
    New,
    Improve,
    Edit,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub session: String,
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProposalTarget {
    Skill { name: String },
    File { path: String },
}

/// YAML frontmatter of a proposal
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposalFrontmatter {
    #[serde(rename = "type")]
    pub proposal_type: ProposalType,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ProposalTarget>,
    // Backward-compatible legacy field. New proposals should populate `target`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_skill: Option<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    pub created: DateTime<Utc>,
}

impl ProposalFrontmatter {
    /// Return a normalized proposal target with backward compatibility for
    /// legacy `target_skill` frontmatter.
    pub fn resolved_target(&self) -> Option<ProposalTarget> {
        self.target.clone().or_else(|| {
            self.target_skill
                .as_ref()
                .map(|name| ProposalTarget::Skill { name: name.clone() })
        })
    }
}

/// A full proposal: frontmatter + markdown body
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub frontmatter: ProposalFrontmatter,
    pub body: String,
    /// Filename (e.g. "new-git-workflow.md"), set when read from disk
    pub filename: Option<String>,
}

impl Proposal {
    /// Serialize a proposal to markdown with YAML frontmatter
    pub fn to_markdown(&self) -> Result<String, serde_yaml::Error> {
        let yaml = serde_yaml::to_string(&self.frontmatter)?;
        Ok(format!("---\n{yaml}---\n\n{}", self.body))
    }

    /// Parse a proposal from markdown with YAML frontmatter
    pub fn from_markdown(input: &str) -> anyhow::Result<Self> {
        let trimmed = input.trim_start();
        if !trimmed.starts_with("---") {
            anyhow::bail!("Proposal must start with YAML frontmatter (---)");
        }

        // Find the closing ---
        let after_first = &trimmed[3..];
        let end = after_first
            .find("\n---")
            .ok_or_else(|| anyhow::anyhow!("No closing --- found for frontmatter"))?;

        let yaml_str = &after_first[..end];
        let body_start = end + 4; // skip \n---
        let body = after_first[body_start..]
            .trim_start_matches('\n')
            .to_string();

        let frontmatter: ProposalFrontmatter = serde_yaml::from_str(yaml_str)?;

        Ok(Self {
            frontmatter,
            body,
            filename: None,
        })
    }
}

fn slugify_skill_name(raw: &str) -> Option<String> {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_was_dash = false;
        } else if !previous_was_dash {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { None } else { Some(slug) }
}

pub fn infer_skill_name_from_body(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let heading = line.trim_start().strip_prefix('#')?;
        let heading = heading.trim_start();
        if heading.is_empty() {
            return None;
        }
        slugify_skill_name(heading)
    })
}

pub fn infer_skill_filename_from_body(body: &str) -> Option<String> {
    infer_skill_name_from_body(body).map(|name| format!("{name}.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proposal() -> Proposal {
        Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::New,
                confidence: Confidence::High,
                target: None,
                target_skill: None,
                evidence: vec![Evidence {
                    session: "~/.claude/sessions/abc123.jsonl".into(),
                    pattern: "User manually ran git rebase workflow 4 times".into(),
                }],
                created: DateTime::parse_from_rfc3339("2026-03-02T10:00:00Z")
                    .unwrap()
                    .to_utc(),
            },
            body: "# Git Rebase Workflow\n\nWhen rebasing, always use interactive rebase.".into(),
            filename: None,
        }
    }

    #[test]
    fn test_proposal_to_markdown() {
        let proposal = sample_proposal();
        let md = proposal.to_markdown().unwrap();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("type: new"));
        assert!(md.contains("confidence: high"));
        assert!(md.contains("# Git Rebase Workflow"));
    }

    #[test]
    fn test_proposal_roundtrip() {
        let proposal = sample_proposal();
        let md = proposal.to_markdown().unwrap();
        let parsed = Proposal::from_markdown(&md).unwrap();
        assert_eq!(proposal.frontmatter, parsed.frontmatter);
        assert_eq!(proposal.body, parsed.body);
    }

    #[test]
    fn test_proposal_from_invalid_markdown() {
        let result = Proposal::from_markdown("no frontmatter here");
        assert!(result.is_err());
    }

    #[test]
    fn test_proposal_type_serde() {
        let yaml = "new";
        let pt: ProposalType = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pt, ProposalType::New);

        let yaml = "improve";
        let pt: ProposalType = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pt, ProposalType::Improve);
    }

    #[test]
    fn test_confidence_serde() {
        let yaml = "high";
        let c: Confidence = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c, Confidence::High);
    }

    #[test]
    fn test_frontmatter_serde_roundtrip() {
        let fm = sample_proposal().frontmatter;
        let yaml = serde_yaml::to_string(&fm).unwrap();
        let parsed: ProposalFrontmatter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(fm, parsed);
    }

    #[test]
    fn test_frontmatter_roundtrip_with_file_target() {
        let proposal = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::Edit,
                confidence: Confidence::Medium,
                target: Some(ProposalTarget::File {
                    path: "/tmp/project/AGENTS.md".to_string(),
                }),
                target_skill: None,
                evidence: vec![],
                created: DateTime::parse_from_rfc3339("2026-03-05T10:00:00Z")
                    .unwrap()
                    .to_utc(),
            },
            body: "# AGENTS\n\nupdated".to_string(),
            filename: None,
        };

        let md = proposal.to_markdown().unwrap();
        let parsed = Proposal::from_markdown(&md).unwrap();
        assert_eq!(parsed.frontmatter.target, proposal.frontmatter.target);
        assert_eq!(parsed.frontmatter.target_skill, None);
    }

    #[test]
    fn test_legacy_target_skill_resolves_to_skill_target() {
        let proposal = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::Improve,
                confidence: Confidence::High,
                target: None,
                target_skill: Some("git-workflow".to_string()),
                evidence: vec![],
                created: DateTime::parse_from_rfc3339("2026-03-05T10:00:00Z")
                    .unwrap()
                    .to_utc(),
            },
            body: "# Git Workflow".to_string(),
            filename: None,
        };
        assert_eq!(
            proposal.frontmatter.resolved_target(),
            Some(ProposalTarget::Skill {
                name: "git-workflow".to_string()
            })
        );
    }

    #[test]
    fn test_infer_skill_name_from_body_slugifies_markdown_title() {
        assert_eq!(
            infer_skill_name_from_body("# Sync AGENTS.md\n\nBody."),
            Some("sync-agents-md".to_string())
        );
    }

    #[test]
    fn test_infer_skill_name_from_body_returns_none_without_heading() {
        assert_eq!(infer_skill_name_from_body("No heading here."), None);
    }
}
