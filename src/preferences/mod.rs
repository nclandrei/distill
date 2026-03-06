use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::proposals::ProposalType;

const MIN_SIGNAL_COUNT: usize = 3;
const MAX_SIGNALS_PER_DIRECTION: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceSignal {
    pub tag: String,
    pub accepted: usize,
    pub rejected: usize,
}

impl PreferenceSignal {
    fn total(&self) -> usize {
        self.accepted + self.rejected
    }

    fn acceptance_rate(&self) -> f32 {
        if self.total() == 0 {
            0.0
        } else {
            self.accepted as f32 / self.total() as f32
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreferenceProfile {
    pub reviewed: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub prefer: Vec<PreferenceSignal>,
    pub avoid: Vec<PreferenceSignal>,
}

impl PreferenceProfile {
    pub fn load(history_dir: &Path) -> Result<Self> {
        let decisions_path = history_dir.join("decisions.jsonl");
        if !decisions_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&decisions_path)
            .with_context(|| format!("Failed to read {}", decisions_path.display()))?;

        let mut accepted = 0usize;
        let mut rejected = 0usize;
        let mut by_tag: BTreeMap<String, (usize, usize)> = BTreeMap::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(entry) = serde_json::from_str::<StoredHistoryEntry>(trimmed) else {
                continue;
            };

            let is_accepted = match entry.decision.as_str() {
                "accepted" => {
                    accepted += 1;
                    true
                }
                "rejected" => {
                    rejected += 1;
                    false
                }
                _ => continue,
            };

            for tag in entry.normalized_tags() {
                let counters = by_tag.entry(tag).or_default();
                if is_accepted {
                    counters.0 += 1;
                } else {
                    counters.1 += 1;
                }
            }
        }

        let mut prefer = Vec::new();
        let mut avoid = Vec::new();
        for (tag, (tag_accepted, tag_rejected)) in by_tag {
            let signal = PreferenceSignal {
                tag,
                accepted: tag_accepted,
                rejected: tag_rejected,
            };
            if signal.total() < MIN_SIGNAL_COUNT {
                continue;
            }

            let rate = signal.acceptance_rate();
            if rate >= 0.70 && signal.accepted >= 2 {
                prefer.push(signal);
            } else if rate <= 0.30 && signal.rejected >= 2 {
                avoid.push(signal);
            }
        }

        prefer.sort_by(|a, b| {
            b.total()
                .cmp(&a.total())
                .then_with(|| b.accepted.cmp(&a.accepted))
                .then_with(|| a.tag.cmp(&b.tag))
        });
        avoid.sort_by(|a, b| {
            b.total()
                .cmp(&a.total())
                .then_with(|| b.rejected.cmp(&a.rejected))
                .then_with(|| a.tag.cmp(&b.tag))
        });
        prefer.truncate(MAX_SIGNALS_PER_DIRECTION);
        avoid.truncate(MAX_SIGNALS_PER_DIRECTION);

        Ok(Self {
            reviewed: accepted + rejected,
            accepted,
            rejected,
            prefer,
            avoid,
        })
    }

    pub fn signal_count(&self) -> usize {
        self.prefer.len() + self.avoid.len()
    }

    pub fn to_prompt_block(&self) -> String {
        let mut section = String::new();
        section.push_str("## Learned Preferences From Past Reviews\n\n");

        if self.reviewed == 0 {
            section.push_str("No historical review decisions yet.\n\n");
            return section;
        }

        let acceptance_pct = ((self.accepted as f32 / self.reviewed as f32) * 100.0).round();
        section.push_str(&format!(
            "- Decisions reviewed: {} (accepted {}, rejected {}, acceptance {}%)\n",
            self.reviewed, self.accepted, self.rejected, acceptance_pct as usize
        ));

        if self.prefer.is_empty() && self.avoid.is_empty() {
            section.push_str("- No stable category preference signal yet.\n\n");
            return section;
        }

        if !self.prefer.is_empty() {
            section.push_str("- Prioritize categories the user usually accepts:\n");
            for signal in &self.prefer {
                section.push_str(&format!(
                    "  - {} (accepted {}, rejected {})\n",
                    display_tag(&signal.tag),
                    signal.accepted,
                    signal.rejected
                ));
            }
        }

        if !self.avoid.is_empty() {
            section.push_str(
                "- Deprioritize categories the user usually rejects unless evidence is very strong:\n",
            );
            for signal in &self.avoid {
                section.push_str(&format!(
                    "  - {} (accepted {}, rejected {})\n",
                    display_tag(&signal.tag),
                    signal.accepted,
                    signal.rejected
                ));
            }
        }

        section.push_str("- Treat these as weighting hints, not hard rules.\n\n");
        section
    }
}

fn display_tag(tag: &str) -> String {
    if let Some(rest) = tag.strip_prefix("type:") {
        format!("type={rest}")
    } else if let Some(rest) = tag.strip_prefix("target:") {
        format!("target={rest}")
    } else {
        tag.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct StoredHistoryEntry {
    decision: String,
    #[serde(default)]
    proposal_filename: String,
    #[serde(default)]
    proposal_type: Option<ProposalType>,
    #[serde(default)]
    target_kind: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

impl StoredHistoryEntry {
    fn normalized_tags(&self) -> BTreeSet<String> {
        let mut tags = BTreeSet::new();
        for raw in &self.tags {
            if let Some(tag) = normalize_tag(raw) {
                tags.insert(tag);
            }
        }

        if let Some(kind) = &self.target_kind {
            let normalized_kind = kind.trim().to_ascii_lowercase();
            if matches!(normalized_kind.as_str(), "skill" | "file") {
                tags.insert(format!("target:{normalized_kind}"));
            }
        }

        if let Some(proposal_type) = &self.proposal_type {
            tags.insert(format!("type:{}", proposal_type_slug(proposal_type)));
        } else if let Some(inferred) = infer_type_from_filename(&self.proposal_filename) {
            tags.insert(format!("type:{inferred}"));
        }

        tags
    }
}

fn normalize_tag(raw: &str) -> Option<String> {
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_'], "-")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == ':')
        .collect::<String>()
        .trim_matches('-')
        .to_string();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn infer_type_from_filename(filename: &str) -> Option<&'static str> {
    let prefix = filename.split('-').next()?;
    match prefix {
        "new" => Some("new"),
        "improve" => Some("improve"),
        "edit" => Some("edit"),
        "remove" => Some("remove"),
        _ => None,
    }
}

fn proposal_type_slug(proposal_type: &ProposalType) -> &'static str {
    match proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_missing_history_file() {
        let dir = tempfile::tempdir().unwrap();
        let profile = PreferenceProfile::load(dir.path()).unwrap();
        assert_eq!(profile, PreferenceProfile::default());
    }

    #[test]
    fn test_load_builds_prefer_and_avoid_signals() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();

        let lines = [
            r#"{"proposal_filename":"p1.md","decision":"accepted","tags":["git"],"proposal_type":"new","target_kind":"skill"}"#,
            r#"{"proposal_filename":"p2.md","decision":"accepted","tags":["git"],"proposal_type":"improve","target_kind":"skill"}"#,
            r#"{"proposal_filename":"p3.md","decision":"accepted","tags":["git"],"proposal_type":"edit","target_kind":"skill"}"#,
            r#"{"proposal_filename":"p4.md","decision":"rejected","tags":["testing"],"proposal_type":"new","target_kind":"skill"}"#,
            r#"{"proposal_filename":"p5.md","decision":"rejected","tags":["testing"],"proposal_type":"improve","target_kind":"skill"}"#,
            r#"{"proposal_filename":"p6.md","decision":"rejected","tags":["testing"],"proposal_type":"edit","target_kind":"skill"}"#,
        ];
        std::fs::write(history_dir.join("decisions.jsonl"), lines.join("\n")).unwrap();

        let profile = PreferenceProfile::load(&history_dir).unwrap();
        assert_eq!(profile.reviewed, 6);
        assert_eq!(profile.accepted, 3);
        assert_eq!(profile.rejected, 3);
        assert!(profile.prefer.iter().any(|s| s.tag == "git"));
        assert!(profile.avoid.iter().any(|s| s.tag == "testing"));
    }

    #[test]
    fn test_load_infers_type_from_legacy_filename_only_history() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        std::fs::create_dir_all(&history_dir).unwrap();
        let lines = [
            r#"{"proposal_filename":"remove-20260306-0.md","decision":"rejected"}"#,
            r#"{"proposal_filename":"remove-20260306-1.md","decision":"rejected"}"#,
            r#"{"proposal_filename":"remove-20260306-2.md","decision":"rejected"}"#,
        ];
        std::fs::write(history_dir.join("decisions.jsonl"), lines.join("\n")).unwrap();

        let profile = PreferenceProfile::load(&history_dir).unwrap();
        assert!(profile.avoid.iter().any(|s| s.tag == "type:remove"));
    }

    #[test]
    fn test_to_prompt_block_without_history() {
        let profile = PreferenceProfile::default();
        let block = profile.to_prompt_block();
        assert!(block.contains("No historical review decisions yet."));
    }
}
