// Onboarding flow — interactive first-run setup.

use anyhow::Result;
use std::io::{self, Write};
use std::path::Path;

use crate::agents::AgentKind;
use crate::config::{AgentEntry, Config, Interval, NotificationPref, ShellType};
use crate::schedule;
use crate::shell::{self, HookStatus};

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

/// Side effects applied after onboarding choices are persisted.
pub struct PostSetupResult {
    /// Shell hook install result (or `None` when user skipped installation).
    pub hook_status: Option<HookStatus>,
    /// Path to the scheduler file created during installation.
    pub scheduler_path: std::path::PathBuf,
}

/// Apply post-onboarding setup:
/// * Optionally install shell hook
/// * Always install scheduler with the chosen interval
pub fn apply_post_onboarding_setup(
    config: &Config,
    home: &Path,
    install_shell_hook: bool,
) -> Result<PostSetupResult> {
    let hook_status = if install_shell_hook {
        Some(shell::install_hook(&config.shell, home)?)
    } else {
        None
    };

    let scheduler = schedule::create_scheduler(home.to_path_buf());
    scheduler.install(&config.scan_interval)?;
    let scheduler_path = scheduler.plist_or_unit_path();

    Ok(PostSetupResult {
        hook_status,
        scheduler_path,
    })
}

// ---------------------------------------------------------------------------
// I/O helpers (used only by run_interactive)
// ---------------------------------------------------------------------------

/// Print `prompt` without a trailing newline, flush stdout, then read one line
/// from stdin.  Returns the trimmed line.
fn prompt(prompt_str: &str) -> Result<String> {
    print!("{}", prompt_str);
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

// ---------------------------------------------------------------------------
// Interactive entry point
// ---------------------------------------------------------------------------

/// Run the full interactive onboarding flow, writing prompts to stdout and
/// reading answers from stdin.  At the end, the resulting config is saved to
/// the default location and a summary is printed.
pub fn run_interactive() -> Result<()> {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    println!();
    println!("Welcome to distill! Let's set things up.");
    println!();

    // ------------------------------------------------------------------
    // Step 1: Detect installed agents
    // ------------------------------------------------------------------

    let detected = detect_agents(&home);

    println!("Detecting installed AI agents...");
    let mut any_found = false;
    for (kind, installed) in &detected {
        let status = if *installed { "found" } else { "not found" };
        println!("  {kind} — {status}");
        if *installed {
            any_found = true;
        }
    }
    if !any_found {
        println!("  (No agents detected. You can still configure distill manually later.)");
    }
    println!();

    // ------------------------------------------------------------------
    // Step 2: Ask which agents to monitor
    // ------------------------------------------------------------------

    let all_agents = AgentKind::all();
    let installed_agents: Vec<AgentKind> = detected
        .iter()
        .filter(|(_, installed)| *installed)
        .map(|(kind, _)| *kind)
        .collect();

    println!("Which agents would you like to monitor?");
    for (i, kind) in all_agents.iter().enumerate() {
        let installed = detected
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, v)| *v)
            .unwrap_or(false);
        let suffix = if installed { "" } else { " (not detected)" };
        println!("  {}. {kind}{suffix}", i + 1);
    }

    // Default: all installed agents, or all known agents if none detected.
    let default_agents: Vec<AgentKind> = if !installed_agents.is_empty() {
        installed_agents.clone()
    } else {
        all_agents.clone()
    };
    let default_label: Vec<String> = default_agents.iter().map(|k| k.to_string()).collect();
    let default_label = default_label.join(", ");

    let agent_input = prompt(&format!(
        "Enter numbers (comma-separated) or 'all' [default: {default_label}]: "
    ))?;

    let enabled_agents: Vec<AgentKind> =
        if agent_input.is_empty() || agent_input.to_lowercase() == "all" {
            default_agents.clone()
        } else {
            let mut chosen = Vec::new();
            for part in agent_input.split(',') {
                let part = part.trim();
                if let Ok(n) = part.parse::<usize>() {
                    if n >= 1 && n <= all_agents.len() {
                        chosen.push(all_agents[n - 1]);
                    }
                }
            }
            if chosen.is_empty() {
                default_agents.clone()
            } else {
                chosen
            }
        };
    println!();

    // ------------------------------------------------------------------
    // Step 3: Ask scan interval
    // ------------------------------------------------------------------

    println!("How often should distill scan for new skills?");
    println!("  1. daily");
    println!("  2. weekly  (recommended)");
    println!("  3. monthly");

    let interval_input = prompt("Enter number [default: 2 (weekly)]: ")?;
    let scan_interval = match interval_input.as_str() {
        "1" => Interval::Daily,
        "3" => Interval::Monthly,
        _ => Interval::Weekly,
    };
    println!();

    // ------------------------------------------------------------------
    // Step 4: Ask which agent to use for proposals
    // ------------------------------------------------------------------

    let proposal_agent = if enabled_agents.len() <= 1 {
        *enabled_agents.first().unwrap_or(&AgentKind::Claude)
    } else {
        println!("Which agent should be used to generate skill proposals?");
        for (i, kind) in enabled_agents.iter().enumerate() {
            println!("  {}. {kind}", i + 1);
        }
        let proposal_input = prompt(&format!(
            "Enter number [default: 1 ({})]: ",
            enabled_agents[0]
        ))?;

        if proposal_input.is_empty() {
            enabled_agents[0]
        } else if let Ok(n) = proposal_input.parse::<usize>() {
            if n >= 1 && n <= enabled_agents.len() {
                enabled_agents[n - 1]
            } else {
                enabled_agents[0]
            }
        } else {
            enabled_agents[0]
        }
    };
    println!();

    // ------------------------------------------------------------------
    // Step 5: Detect shell, confirm with user
    // ------------------------------------------------------------------

    let detected_shell = ShellType::detect();
    println!("Detected shell: {detected_shell}");
    println!("  1. zsh");
    println!("  2. bash");
    println!("  3. fish");
    println!("  4. other");

    let default_shell_num = match detected_shell {
        ShellType::Zsh => "1",
        ShellType::Bash => "2",
        ShellType::Fish => "3",
        ShellType::Other => "4",
    };
    let shell_input = prompt(&format!(
        "Confirm shell [default: {default_shell_num} ({detected_shell})]: "
    ))?;
    let shell = match shell_input.as_str() {
        "1" => ShellType::Zsh,
        "2" => ShellType::Bash,
        "3" => ShellType::Fish,
        "4" => ShellType::Other,
        _ => detected_shell,
    };
    println!();

    let install_shell_hook = if shell == ShellType::Other {
        println!("Automatic shell hook install is not available for 'other'.");
        false
    } else {
        let install_hook_input = prompt("Install terminal notification hook now? [Y/n]: ")?;
        !matches!(
            install_hook_input.trim().to_ascii_lowercase().as_str(),
            "n" | "no"
        )
    };
    println!();

    // ------------------------------------------------------------------
    // Step 6: Ask notification preference
    // ------------------------------------------------------------------

    println!("How would you like to receive notifications?");
    println!("  1. terminal  — print to terminal on next prompt");
    println!("  2. native    — system notification");
    println!("  3. both      (recommended)");
    println!("  4. none");

    let notif_input = prompt("Enter number [default: 3 (both)]: ")?;
    let notifications = match notif_input.as_str() {
        "1" => NotificationPref::Terminal,
        "2" => NotificationPref::Native,
        "4" => NotificationPref::None,
        _ => NotificationPref::Both,
    };
    println!();

    // ------------------------------------------------------------------
    // Step 7: Build and save config
    // ------------------------------------------------------------------

    let answers = OnboardingAnswers {
        detected_agents: detected,
        enabled_agents,
        scan_interval,
        proposal_agent,
        shell,
        notifications,
    };

    let config = build_config(&answers);
    config.save()?;
    let post_setup = apply_post_onboarding_setup(&config, &home, install_shell_hook)?;

    // ------------------------------------------------------------------
    // Step 8: Print summary
    // ------------------------------------------------------------------

    let enabled_names: Vec<&str> = config
        .agents
        .iter()
        .filter(|a| a.enabled)
        .map(|a| a.name.as_str())
        .collect();
    let enabled_display = if enabled_names.is_empty() {
        "(none)".to_string()
    } else {
        enabled_names.join(", ")
    };

    println!("Configuration saved to {}", Config::config_path().display());
    println!();
    println!("Summary:");
    println!("  Agents monitored : {enabled_display}");
    println!("  Scan interval    : {}", config.scan_interval);
    println!("  Proposal agent   : {}", config.proposal_agent);
    println!("  Shell            : {}", config.shell);
    println!("  Notifications    : {}", config.notifications);
    println!();
    println!("Setup:");
    match post_setup.hook_status {
        Some(HookStatus::Installed) => println!("  Shell hook       : installed"),
        Some(HookStatus::AlreadyInstalled) => println!("  Shell hook       : already installed"),
        Some(HookStatus::Unsupported) => {
            println!("  Shell hook       : unsupported shell (manual setup required)")
        }
        Some(HookStatus::Removed) | Some(HookStatus::NotFound) => {
            println!("  Shell hook       : not installed")
        }
        None => println!("  Shell hook       : skipped"),
    }
    println!(
        "  Scheduler        : installed ({})",
        post_setup.scheduler_path.display()
    );
    println!();
    println!("Run 'distill scan --now' to start your first scan.");
    println!("Run 'distill review' to review pending proposals.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Interval, NotificationPref, ShellType};

    // --- detect_agents ---

    #[test]
    fn test_detect_agents_none_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Neither .claude nor .codex exists.
        let detected = detect_agents(&home);
        assert_eq!(
            detected.len(),
            2,
            "should report an entry for every known agent"
        );
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
        let claude = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Claude)
            .unwrap();
        let codex = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Codex)
            .unwrap();
        assert!(claude.1, "Claude should be detected");
        assert!(!codex.1, "Codex should not be detected");
    }

    #[test]
    fn test_detect_agents_codex_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let claude = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Claude)
            .unwrap();
        let codex = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Codex)
            .unwrap();
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

    // --- post-onboarding setup side effects ---

    #[test]
    fn test_apply_post_onboarding_setup_installs_hook_and_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Zsh,
            scan_interval: Interval::Weekly,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, true).unwrap();
        assert_eq!(result.hook_status, Some(HookStatus::Installed));
        assert!(
            home.join(".zshrc").exists(),
            "expected .zshrc to be created"
        );
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }

    #[test]
    fn test_apply_post_onboarding_setup_skips_hook_when_declined() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Bash,
            scan_interval: Interval::Weekly,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, false).unwrap();
        assert_eq!(result.hook_status, None);
        assert!(
            !home.join(".bashrc").exists(),
            "expected .bashrc to remain untouched when hook is skipped"
        );
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }

    #[test]
    fn test_apply_post_onboarding_setup_reports_unsupported_shell() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Other,
            scan_interval: Interval::Daily,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, true).unwrap();
        assert_eq!(result.hook_status, Some(HookStatus::Unsupported));
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }
}
