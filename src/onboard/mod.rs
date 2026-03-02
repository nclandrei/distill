// Onboarding flow — interactive first-run setup.

use std::path::Path;

use crate::agents::AgentKind;
use crate::config::{AgentEntry, Config, Interval, NotificationPref, ShellType};

mod tui;
pub use tui::run_interactive;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Holds all choices gathered during the onboarding flow.
/// The struct is deliberately decoupled from I/O so that `build_config` can be
/// exercised in unit tests without any stdin/stdout interaction.
pub struct OnboardingAnswers {
    /// Every known agent paired with whether its config directory was found on disk.
    pub detected_agents: Vec<(AgentKind, bool)>,
    /// Subset of agents the user chose to enable.
    pub enabled_agents: Vec<AgentKind>,
    /// How often to run a scan.
    pub scan_interval: Interval,
    /// Which agent generates skill proposals.
    pub proposal_agent: AgentKind,
    /// The user's shell.
    pub shell: ShellType,
    /// How to deliver notifications.
    pub notifications: NotificationPref,
}

// ---------------------------------------------------------------------------
// Pure logic functions (testable without I/O)
// ---------------------------------------------------------------------------

/// Detect which agents are installed by checking whether their config
/// directories exist under `home`.
///
/// Returns a `Vec<(AgentKind, bool)>` where `bool` is `true` when the
/// agent's config directory is present.
pub fn detect_agents(home: &Path) -> Vec<(AgentKind, bool)> {
    AgentKind::all()
        .into_iter()
        .map(|kind| {
            let installed = match kind {
                AgentKind::Claude => home.join(".claude").exists(),
                AgentKind::Codex => home.join(".codex").exists(),
            };
            (kind, installed)
        })
        .collect()
}

/// Build a `Config` from the user's onboarding answers.
///
/// This is a pure function — no side-effects, fully testable.
pub fn build_config(answers: &OnboardingAnswers) -> Config {
    // One AgentEntry per detected agent; enabled if the user selected it.
    let agents: Vec<AgentEntry> = answers
        .detected_agents
        .iter()
        .map(|(kind, _installed)| AgentEntry {
            name: kind.to_string(),
            enabled: answers.enabled_agents.contains(kind),
        })
        .collect();

    Config {
        agents,
        scan_interval: answers.scan_interval.clone(),
        proposal_agent: answers.proposal_agent.to_string(),
        shell: answers.shell.clone(),
        notifications: answers.notifications.clone(),
    }
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Interval, NotificationPref, ShellType};

    // --- detect_agents ---

    #[test]
    fn test_detect_agents_none_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Neither .claude nor .codex exists.
        let detected = detect_agents(&home);
        assert_eq!(detected.len(), 2, "should report an entry for every known agent");
        for (_, installed) in &detected {
            assert!(!installed, "no agent should be detected");
        }
    }

    #[test]
    fn test_detect_agents_claude_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let detected = detect_agents(&home);
        let claude = detected.iter().find(|(k, _)| *k == AgentKind::Claude).unwrap();
        let codex = detected.iter().find(|(k, _)| *k == AgentKind::Codex).unwrap();
        assert!(claude.1, "Claude should be detected");
        assert!(!codex.1, "Codex should not be detected");
    }

    #[test]
    fn test_detect_agents_codex_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let claude = detected.iter().find(|(k, _)| *k == AgentKind::Claude).unwrap();
        let codex = detected.iter().find(|(k, _)| *k == AgentKind::Codex).unwrap();
        assert!(!claude.1, "Claude should not be detected");
        assert!(codex.1, "Codex should be detected");
    }

    #[test]
    fn test_detect_agents_both_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        assert_eq!(detected.len(), 2);
        for (_, installed) in &detected {
            assert!(installed, "both agents should be detected");
        }
    }

    // --- build_config: basic field mapping ---

    #[test]
    fn test_build_config_default_answers() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert_eq!(config.proposal_agent, "claude");
        assert_eq!(config.shell, ShellType::Zsh);
        assert_eq!(config.notifications, NotificationPref::Both);
    }

    // --- build_config: default interval is weekly ---

    #[test]
    fn test_build_config_default_interval_is_weekly() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::default(), // should be Weekly
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Weekly);
    }

    // --- build_config: only selected agents are enabled ---

    #[test]
    fn test_build_config_enables_only_selected_agents() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude], // only Claude enabled
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Bash,
            notifications: NotificationPref::Terminal,
        };

        let config = build_config(&answers);
        let claude_entry = config.agents.iter().find(|a| a.name == "claude").unwrap();
        let codex_entry = config.agents.iter().find(|a| a.name == "codex").unwrap();
        assert!(claude_entry.enabled, "Claude should be enabled");
        assert!(!codex_entry.enabled, "Codex should be disabled");
    }

    // --- build_config: agent entries match detected agents ---

    #[test]
    fn test_build_config_agent_entries_match_detected() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Only Claude dir exists, but user enables both.
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(
            config.agents.len(),
            2,
            "should produce one entry per detected agent"
        );
        let claude = config.agents.iter().find(|a| a.name == "claude").unwrap();
        let codex = config.agents.iter().find(|a| a.name == "codex").unwrap();
        assert!(claude.enabled);
        assert!(codex.enabled);
    }

    // --- build_config: various interval and notification combinations ---

    #[test]
    fn test_build_config_daily_interval() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::Daily,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Daily);
    }

    #[test]
    fn test_build_config_monthly_interval_and_native_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Codex],
            scan_interval: Interval::Monthly,
            proposal_agent: AgentKind::Codex,
            shell: ShellType::Fish,
            notifications: NotificationPref::Native,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Monthly);
        assert_eq!(config.proposal_agent, "codex");
        assert_eq!(config.shell, ShellType::Fish);
        assert_eq!(config.notifications, NotificationPref::Native);
    }

    // --- build_config: shell detection is used ---

    #[test]
    fn test_build_config_shell_detection_used() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let detected_shell = ShellType::detect();
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: detected_shell.clone(),
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.shell, detected_shell);
    }

    // --- build_config: proposal agent is codex ---

    #[test]
    fn test_build_config_proposal_agent_codex() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Codex,
            shell: ShellType::Bash,
            notifications: NotificationPref::None,
        };

        let config = build_config(&answers);
        assert_eq!(config.proposal_agent, "codex");
        assert_eq!(config.notifications, NotificationPref::None);
    }
}
