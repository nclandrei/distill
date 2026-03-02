use crate::config::Config;
use anyhow::Result;
use std::path::Path;

/// All the data needed to render the status output.
pub struct StatusInfo {
    pub config: Config,
    pub pending_proposals: usize,
    pub accepted_skills: usize,
    /// Human-readable timestamp string from `last-scan.json`, or `None` when
    /// the file does not exist (i.e. the tool has never run a scan).
    pub last_scan: Option<String>,
}

/// Pure formatting function — takes pre-collected data and returns the full
/// status string.  Keeping this separate from I/O makes it trivially testable.
pub fn format_status(info: &StatusInfo) -> String {
    let mut out = String::new();

    out.push_str("=== distill status ===\n");
    out.push('\n');
    out.push_str(&format!("Scan interval:  {}\n", info.config.scan_interval));
    out.push_str(&format!("Proposal agent: {}\n", info.config.proposal_agent));
    out.push_str(&format!("Shell:          {}\n", info.config.shell));
    out.push_str(&format!("Notifications:  {}\n", info.config.notifications));
    out.push('\n');

    out.push_str("Monitored agents:\n");
    for agent in &info.config.agents {
        let status = if agent.enabled { "enabled" } else { "disabled" };
        out.push_str(&format!("  - {} ({})\n", agent.name, status));
    }
    out.push('\n');

    out.push_str(&format!("Pending proposals: {}\n", info.pending_proposals));
    out.push_str(&format!("Accepted skills:   {}\n", info.accepted_skills));

    let last_scan_display = info.last_scan.as_deref().unwrap_or("never");
    out.push_str(&format!("Last scan:         {last_scan_display}\n"));

    out
}

/// Collect runtime status data (proposal count, skill count, last-scan
/// timestamp) from the given `base_dir` rather than the hard-coded default
/// `~/.distill`.  This makes the function fully testable with `tempfile`.
pub fn collect_status_info(config: &Config, base_dir: &Path) -> Result<StatusInfo> {
    let proposals_dir = base_dir.join("proposals");
    let pending_proposals = if proposals_dir.exists() {
        std::fs::read_dir(&proposals_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .count()
    } else {
        0
    };

    let skills_dir = base_dir.join("skills");
    let accepted_skills = if skills_dir.exists() {
        std::fs::read_dir(&skills_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .count()
    } else {
        0
    };

    let last_scan_path = base_dir.join("last-scan.json");
    let last_scan = if last_scan_path.exists() {
        Some(std::fs::read_to_string(&last_scan_path)?.trim().to_string())
    } else {
        None
    };

    Ok(StatusInfo {
        config: config.clone(),
        pending_proposals,
        accepted_skills,
        last_scan,
    })
}

/// Entry point called by `main`.  Delegates to `collect_status_info` and
/// `format_status` so all logic is covered by unit tests.
pub fn run() -> Result<()> {
    if !Config::exists() {
        println!("distill is not configured. Run 'distill' to start onboarding.");
        return Ok(());
    }

    let config = Config::load()?;
    let base_dir = Config::base_dir();
    let info = collect_status_info(&config, &base_dir)?;
    print!("{}", format_status(&info));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentEntry, Config, Interval, NotificationPref, ShellType};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn default_info() -> StatusInfo {
        StatusInfo {
            config: Config::default(),
            pending_proposals: 0,
            accepted_skills: 0,
            last_scan: None,
        }
    }

    // ── format_status tests ───────────────────────────────────────────────────

    #[test]
    fn test_format_status_default_config() {
        let output = format_status(&default_info());

        assert!(output.contains("=== distill status ==="));
        // Display formatting: "weekly" not "Weekly"
        assert!(output.contains("Scan interval:  weekly"));
        assert!(output.contains("Proposal agent: claude"));
        assert!(output.contains("Shell:          zsh"));
        assert!(output.contains("Notifications:  both"));
    }

    #[test]
    fn test_format_status_shows_all_agents() {
        let output = format_status(&default_info());

        // Default config has claude and codex, both enabled.
        assert!(output.contains("  - claude (enabled)"));
        assert!(output.contains("  - codex (enabled)"));
    }

    #[test]
    fn test_format_status_shows_proposal_count() {
        let mut info = default_info();
        info.pending_proposals = 7;

        let output = format_status(&info);

        assert!(output.contains("Pending proposals: 7"));
    }

    #[test]
    fn test_format_status_shows_skill_count() {
        let mut info = default_info();
        info.accepted_skills = 4;

        let output = format_status(&info);

        assert!(output.contains("Accepted skills:   4"));
    }

    #[test]
    fn test_format_status_never_scanned() {
        // last_scan is None → should print "never"
        let output = format_status(&default_info());

        assert!(output.contains("Last scan:         never"));
    }

    #[test]
    fn test_format_status_with_last_scan() {
        let mut info = default_info();
        info.last_scan = Some("2024-11-20T08:15:00Z".to_string());

        let output = format_status(&info);

        assert!(output.contains("Last scan:         2024-11-20T08:15:00Z"));
        // Must not also say "never"
        assert!(!output.contains("never"));
    }

    /// Disabled agents must be shown as "(disabled)" in the output.
    #[test]
    fn test_format_status_disabled_agent() {
        let mut info = default_info();
        info.config.agents = vec![
            AgentEntry { name: "claude".into(), enabled: true },
            AgentEntry { name: "codex".into(), enabled: false },
        ];

        let output = format_status(&info);

        assert!(output.contains("  - claude (enabled)"));
        assert!(output.contains("  - codex (disabled)"));
    }

    /// Verify that all supported interval / shell / notification variants
    /// produce lowercase Display strings (not Debug variants).
    #[test]
    fn test_format_status_display_formatting_not_debug() {
        let mut info = default_info();
        info.config.scan_interval = Interval::Monthly;
        info.config.shell = ShellType::Fish;
        info.config.notifications = NotificationPref::Native;

        let output = format_status(&info);

        // Display strings
        assert!(output.contains("Scan interval:  monthly"));
        assert!(output.contains("Shell:          fish"));
        assert!(output.contains("Notifications:  native"));
        // Debug strings must not appear
        assert!(!output.contains("Monthly"));
        assert!(!output.contains("Fish"));
        assert!(!output.contains("Native"));
    }

    // ── collect_status_info tests ─────────────────────────────────────────────

    #[test]
    fn test_collect_status_info_counts_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // proposals: two .md files, one .txt that must be ignored
        let proposals = base.join("proposals");
        std::fs::create_dir_all(&proposals).unwrap();
        std::fs::write(proposals.join("p1.md"), "proposal 1").unwrap();
        std::fs::write(proposals.join("p2.md"), "proposal 2").unwrap();
        std::fs::write(proposals.join("notes.txt"), "ignored").unwrap();

        // skills: three .md files
        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("s1.md"), "skill 1").unwrap();
        std::fs::write(skills.join("s2.md"), "skill 2").unwrap();
        std::fs::write(skills.join("s3.md"), "skill 3").unwrap();

        // last-scan.json
        std::fs::write(base.join("last-scan.json"), "2024-06-01T12:00:00Z").unwrap();

        let config = Config::default();
        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.pending_proposals, 2);
        assert_eq!(info.accepted_skills, 3);
        assert_eq!(info.last_scan.as_deref(), Some("2024-06-01T12:00:00Z"));
    }

    #[test]
    fn test_collect_status_info_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Directories exist but contain no files
        std::fs::create_dir_all(base.join("proposals")).unwrap();
        std::fs::create_dir_all(base.join("skills")).unwrap();
        // No last-scan.json

        let config = Config::default();
        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.pending_proposals, 0);
        assert_eq!(info.accepted_skills, 0);
        assert_eq!(info.last_scan, None);
    }

    /// When the proposals and skills directories don't exist at all (fresh
    /// install before `ensure_dirs` has run) the counts should be zero.
    #[test]
    fn test_collect_status_info_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Deliberately do NOT create proposals/ or skills/

        let config = Config::default();
        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.pending_proposals, 0);
        assert_eq!(info.accepted_skills, 0);
        assert_eq!(info.last_scan, None);
    }

    /// `collect_status_info` must preserve the config it was given.
    #[test]
    fn test_collect_status_info_preserves_config() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let config = Config {
            scan_interval: Interval::Daily,
            proposal_agent: "codex".into(),
            shell: ShellType::Bash,
            notifications: NotificationPref::Terminal,
            agents: vec![AgentEntry { name: "codex".into(), enabled: true }],
        };

        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.config.scan_interval, Interval::Daily);
        assert_eq!(info.config.proposal_agent, "codex");
        assert_eq!(info.config.shell, ShellType::Bash);
        assert_eq!(info.config.notifications, NotificationPref::Terminal);
    }

    /// Non-.md files in proposals dir must not be counted.
    #[test]
    fn test_collect_status_info_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let proposals = base.join("proposals");
        std::fs::create_dir_all(&proposals).unwrap();
        std::fs::write(proposals.join("real.md"), "actual proposal").unwrap();
        std::fs::write(proposals.join("draft.yaml"), "not a proposal").unwrap();
        std::fs::write(proposals.join("readme.txt"), "ignore me").unwrap();

        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("real.md"), "actual skill").unwrap();
        std::fs::write(skills.join("meta.json"), "not a skill").unwrap();

        let config = Config::default();
        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.pending_proposals, 1);
        assert_eq!(info.accepted_skills, 1);
    }

    /// last-scan.json content is trimmed of surrounding whitespace.
    #[test]
    fn test_collect_status_info_trims_last_scan_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        std::fs::write(base.join("last-scan.json"), "  2025-01-10T09:00:00Z\n").unwrap();

        let config = Config::default();
        let info = collect_status_info(&config, base).unwrap();

        assert_eq!(info.last_scan.as_deref(), Some("2025-01-10T09:00:00Z"));
    }
}
