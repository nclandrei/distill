use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::proposals::{Confidence, Evidence, Proposal, ProposalFrontmatter, ProposalType};

#[derive(Debug, Clone)]
struct SkillFile {
    filename: String,
    stem: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct DuplicateGroup {
    canonical: SkillFile,
    duplicates: Vec<SkillFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DedupeSummary {
    groups: usize,
    duplicates: usize,
    proposals_written: usize,
    proposals_skipped_existing: usize,
}

#[derive(Debug, Clone)]
struct DedupePaths {
    skills_dir: PathBuf,
    proposals_dir: PathBuf,
}

impl DedupePaths {
    fn from_config() -> Self {
        Self {
            skills_dir: Config::skills_dir(),
            proposals_dir: Config::proposals_dir(),
        }
    }
}

pub fn run(dry_run: bool) -> Result<()> {
    Config::ensure_dirs()?;
    let paths = DedupePaths::from_config();
    let summary = run_with_paths(&paths, dry_run)?;

    if summary.duplicates == 0 {
        println!("No duplicate global skills found.");
        return Ok(());
    }

    println!(
        "Found {} duplicate skill file(s) in {} duplicate group(s).",
        summary.duplicates, summary.groups
    );
    if dry_run {
        println!("Dry run: no proposals were written.");
    } else {
        println!(
            "Wrote {} remove proposal(s), skipped {} existing pending proposal(s).",
            summary.proposals_written, summary.proposals_skipped_existing
        );
        println!("Run 'distill review' to process deduplication proposals.");
    }

    Ok(())
}

fn run_with_paths(paths: &DedupePaths, dry_run: bool) -> Result<DedupeSummary> {
    let groups = find_duplicate_groups(&paths.skills_dir)?;
    if groups.is_empty() {
        return Ok(DedupeSummary::default());
    }

    let duplicate_count = groups.iter().map(|g| g.duplicates.len()).sum::<usize>();
    let mut summary = DedupeSummary {
        groups: groups.len(),
        duplicates: duplicate_count,
        proposals_written: 0,
        proposals_skipped_existing: 0,
    };

    if dry_run {
        return Ok(summary);
    }

    std::fs::create_dir_all(&paths.proposals_dir).with_context(|| {
        format!(
            "Failed to create proposals directory: {}",
            paths.proposals_dir.display()
        )
    })?;

    let mut existing_targets = pending_remove_targets(&paths.proposals_dir)?;

    for (group_idx, group) in groups.into_iter().enumerate() {
        for (dup_idx, duplicate) in group.duplicates.into_iter().enumerate() {
            let target_key = normalize_target_key(&duplicate.filename);
            if existing_targets.contains(&target_key) {
                summary.proposals_skipped_existing += 1;
                continue;
            }

            let proposal = build_remove_proposal(&duplicate, &group.canonical);
            let proposal_filename = format!(
                "remove-dedupe-{}-{}-{}-{}.md",
                sanitize_slug(&duplicate.stem),
                Utc::now().format("%Y%m%d-%H%M%S"),
                group_idx,
                dup_idx
            );
            let proposal_path = paths.proposals_dir.join(&proposal_filename);

            let markdown = proposal
                .to_markdown()
                .context("Failed to serialize dedupe proposal")?;
            std::fs::write(&proposal_path, markdown)
                .with_context(|| format!("Failed to write {}", proposal_path.display()))?;

            existing_targets.insert(target_key);
            summary.proposals_written += 1;
        }
    }

    Ok(summary)
}

fn find_duplicate_groups(skills_dir: &Path) -> Result<Vec<DuplicateGroup>> {
    if !skills_dir.exists() {
        return Ok(vec![]);
    }

    let mut by_content: BTreeMap<String, Vec<SkillFile>> = BTreeMap::new();

    for entry in std::fs::read_dir(skills_dir)
        .with_context(|| format!("Failed to read {}", skills_dir.display()))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown.md")
            .to_string();
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let normalized_content = normalize_content(&content);

        by_content
            .entry(normalized_content.clone())
            .or_default()
            .push(SkillFile {
                filename,
                stem,
                path,
            });
    }

    let mut groups = vec![];
    for mut files in by_content.into_values() {
        if files.len() < 2 {
            continue;
        }
        files.sort_by(canonical_skill_order);

        let canonical = files.remove(0);
        groups.push(DuplicateGroup {
            canonical,
            duplicates: files,
        });
    }

    groups.sort_by(|a, b| a.canonical.filename.cmp(&b.canonical.filename));
    Ok(groups)
}

fn canonical_skill_order(a: &SkillFile, b: &SkillFile) -> std::cmp::Ordering {
    a.filename
        .cmp(&b.filename)
        .then_with(|| a.path.cmp(&b.path))
}

fn normalize_content(content: &str) -> String {
    let unix = content.replace("\r\n", "\n");
    let mut lines = unix
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

fn normalize_target_key(target: &str) -> String {
    let trimmed = target.trim().to_lowercase();
    if trimmed.ends_with(".md") {
        trimmed
    } else {
        format!("{trimmed}.md")
    }
}

fn pending_remove_targets(proposals_dir: &Path) -> Result<BTreeSet<String>> {
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
        if proposal.frontmatter.proposal_type != ProposalType::Remove {
            continue;
        }
        if let Some(target) = proposal.frontmatter.target_skill.as_deref() {
            targets.insert(normalize_target_key(target));
        }
    }
    Ok(targets)
}

fn sanitize_slug(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed
    }
}

fn build_remove_proposal(duplicate: &SkillFile, canonical: &SkillFile) -> Proposal {
    let body = format!(
        "# Remove duplicate skill `{}`\n\n\
         This skill is a duplicate of `{}` and can be removed to reduce noise.\n\n\
         ## Deduplication rationale\n\n\
         - Duplicate target: `{}`\n\
         - Canonical skill kept: `{}`\n\
         - Match strategy: normalized markdown content\n",
        duplicate.filename, canonical.filename, duplicate.filename, canonical.filename
    );

    Proposal {
        frontmatter: ProposalFrontmatter {
            proposal_type: ProposalType::Remove,
            confidence: Confidence::High,
            target_skill: Some(duplicate.filename.clone()),
            evidence: vec![Evidence {
                session: "internal://distill-dedupe".to_string(),
                pattern: format!(
                    "Skill content matches canonical file {}",
                    canonical.filename
                ),
            }],
            created: Utc::now(),
        },
        body,
        filename: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_duplicate_groups_identifies_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.md"), "# Test\nvalue\n").unwrap();
        std::fs::write(dir.path().join("beta.md"), "# Test\nvalue\n").unwrap();
        std::fs::write(dir.path().join("unique.md"), "# Other\nunique\n").unwrap();

        let groups = find_duplicate_groups(dir.path()).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].duplicates.len(), 1);
        assert_eq!(groups[0].canonical.filename, "alpha.md");
        assert_eq!(groups[0].duplicates[0].filename, "beta.md");
    }

    #[test]
    fn test_find_duplicate_groups_normalizes_line_endings_and_trailing_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# Test\r\nvalue   \r\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "# Test\nvalue\n\n").unwrap();

        let groups = find_duplicate_groups(dir.path()).unwrap();
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn test_run_with_paths_dry_run_does_not_write_proposals() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("a.md"), "same").unwrap();
        std::fs::write(skills_dir.join("b.md"), "same").unwrap();

        let summary = run_with_paths(
            &DedupePaths {
                skills_dir,
                proposals_dir: proposals_dir.clone(),
            },
            true,
        )
        .unwrap();

        assert_eq!(summary.duplicates, 1);
        assert_eq!(summary.proposals_written, 0);
        assert!(
            !proposals_dir.exists() || std::fs::read_dir(proposals_dir).unwrap().next().is_none()
        );
    }

    #[test]
    fn test_run_with_paths_writes_remove_proposals() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("canonical.md"), "same").unwrap();
        std::fs::write(skills_dir.join("dup.md"), "same").unwrap();

        let summary = run_with_paths(
            &DedupePaths {
                skills_dir,
                proposals_dir: proposals_dir.clone(),
            },
            false,
        )
        .unwrap();

        assert_eq!(summary.proposals_written, 1);
        let files = std::fs::read_dir(&proposals_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        let proposal_content = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(proposal_content.contains("type: remove"));
        assert!(proposal_content.contains("target_skill: dup.md"));
    }

    #[test]
    fn test_run_with_paths_skips_existing_pending_remove() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let proposals_dir = dir.path().join("proposals");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::create_dir_all(&proposals_dir).unwrap();
        std::fs::write(skills_dir.join("canonical.md"), "same").unwrap();
        std::fs::write(skills_dir.join("dup.md"), "same").unwrap();

        let pending = Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::Remove,
                confidence: Confidence::High,
                target_skill: Some("dup.md".to_string()),
                evidence: vec![],
                created: Utc::now(),
            },
            body: "pending".to_string(),
            filename: None,
        };
        std::fs::write(
            proposals_dir.join("already-pending.md"),
            pending.to_markdown().unwrap(),
        )
        .unwrap();

        let summary = run_with_paths(
            &DedupePaths {
                skills_dir,
                proposals_dir,
            },
            false,
        )
        .unwrap();

        assert_eq!(summary.proposals_written, 0);
        assert_eq!(summary.proposals_skipped_existing, 1);
    }

    #[test]
    fn test_build_remove_proposal_targets_duplicate() {
        let duplicate = SkillFile {
            filename: "dup.md".to_string(),
            stem: "dup".to_string(),
            path: PathBuf::from("/tmp/dup.md"),
        };
        let canonical = SkillFile {
            filename: "canonical.md".to_string(),
            stem: "canonical".to_string(),
            path: PathBuf::from("/tmp/canonical.md"),
        };

        let proposal = build_remove_proposal(&duplicate, &canonical);
        assert_eq!(proposal.frontmatter.proposal_type, ProposalType::Remove);
        assert_eq!(proposal.frontmatter.target_skill.as_deref(), Some("dup.md"));
        assert!(proposal.body.contains("canonical.md"));
    }

    #[test]
    fn test_normalize_target_key_adds_md_suffix() {
        assert_eq!(normalize_target_key("foo"), "foo.md");
        assert_eq!(normalize_target_key("bar.md"), "bar.md");
    }

    #[test]
    fn test_sanitize_slug() {
        assert_eq!(sanitize_slug("Git Workflow"), "git-workflow");
        assert_eq!(sanitize_slug("!!!"), "skill");
    }
}
