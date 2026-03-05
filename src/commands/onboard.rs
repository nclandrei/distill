use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::agents::AgentKind;
use crate::config::{AgentEntry, Config, Interval, NotificationPref, ShellType};
use crate::onboard;
use crate::shell::{self, HookStatus};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DetectedAgent {
    name: String,
    installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OnboardingSpec {
    #[serde(default = "onboarding_format_version")]
    format_version: u32,
    #[serde(default)]
    detected_agents: Vec<DetectedAgent>,
    agents: Vec<AgentEntry>,
    #[serde(default)]
    scan_interval: Interval,
    proposal_agent: String,
    shell: ShellType,
    #[serde(default)]
    notifications: NotificationPref,
    #[serde(default)]
    notification_icon: Option<String>,
    #[serde(default = "default_install_shell_hook")]
    install_shell_hook: bool,
}

const JSON_STDIO_SENTINEL: &str = "-";

fn onboarding_format_version() -> u32 {
    1
}

fn default_install_shell_hook() -> bool {
    true
}

pub fn run(write_json: Option<&Path>, apply_json: Option<&Path>) -> Result<()> {
    match (write_json, apply_json) {
        (Some(path), None) => write_spec_json(path),
        (None, Some(path)) => apply_spec_json(path),
        (None, None) => onboard::run_interactive(),
        (Some(_), Some(_)) => unreachable!("clap enforces flag conflicts"),
    }
}

fn write_spec_json(path: &Path) -> Result<()> {
    let home = home_dir();
    let spec = export_spec(&home)?;
    let json =
        serde_json::to_string_pretty(&spec).context("Failed to serialize onboarding JSON")?;
    write_text(path, &json)?;

    if !is_stdio(path) {
        println!("Wrote onboarding JSON template to {}", path.display());
    }
    Ok(())
}

fn apply_spec_json(path: &Path) -> Result<()> {
    let raw = read_text(path)?;
    let spec: OnboardingSpec = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse onboarding JSON from {}",
            display_path(path)
        )
    })?;
    validate_spec(&spec)?;

    let config = config_from_spec(&spec);
    let home = home_dir();
    let post_setup = onboard::save_config_then_setup(&config, &home, spec.install_shell_hook)?;

    println!("Onboarding applied from JSON.");
    println!("  Config path      : {}", Config::config_path().display());
    println!(
        "  Agents monitored : {}",
        enabled_agents_label(&config.agents)
    );
    println!("  Scan interval    : {}", config.scan_interval);
    println!("  Proposal agent   : {}", config.proposal_agent);
    println!("  Shell            : {}", config.shell);
    println!("  Notifications    : {}", config.notifications);
    match post_setup.hook_status {
        Some(HookStatus::Installed) => println!("  Shell hook       : installed"),
        Some(HookStatus::AlreadyInstalled) => println!("  Shell hook       : already installed"),
        Some(HookStatus::Unsupported) => println!("  Shell hook       : unsupported shell"),
        Some(HookStatus::Removed) | Some(HookStatus::NotFound) => {
            println!("  Shell hook       : not installed")
        }
        None => println!("  Shell hook       : skipped"),
    }
    println!(
        "  Scheduler        : installed ({})",
        post_setup.scheduler_path.display()
    );

    Ok(())
}

fn export_spec(home: &Path) -> Result<OnboardingSpec> {
    let detected = onboard::detect_agents(home);
    let config_path = home.join(".distill").join("config.yaml");
    let has_existing_config = config_path.exists();
    let config = if has_existing_config {
        Config::load_from(&config_path)
            .with_context(|| format!("Failed to load existing distill config at {}", config_path.display()))?
    } else {
        default_config_from_detected(&detected)
    };
    let install_shell_hook = if has_existing_config {
        shell_hook_installed(&config.shell, home)
    } else {
        config.shell != ShellType::Other
    };

    Ok(OnboardingSpec {
        format_version: onboarding_format_version(),
        detected_agents: detected
            .into_iter()
            .map(|(kind, installed)| DetectedAgent {
                name: kind.to_string(),
                installed,
            })
            .collect(),
        agents: config.agents,
        scan_interval: config.scan_interval,
        proposal_agent: config.proposal_agent,
        shell: config.shell,
        notifications: config.notifications,
        notification_icon: config.notification_icon,
        install_shell_hook,
    })
}

fn shell_hook_installed(shell_type: &ShellType, home: &Path) -> bool {
    let Some(path) = shell::shell_config_path(shell_type, home) else {
        return false;
    };
    fs::read_to_string(path)
        .map(|content| content.contains("# distill hook"))
        .unwrap_or(false)
}

fn default_config_from_detected(detected: &[(AgentKind, bool)]) -> Config {
    let installed: HashSet<AgentKind> = detected
        .iter()
        .filter_map(|(kind, is_installed)| is_installed.then_some(*kind))
        .collect();
    let known = AgentKind::all();

    let enabled: HashSet<AgentKind> = if installed.is_empty() {
        known.iter().copied().collect()
    } else {
        installed
    };
    let agents = known
        .iter()
        .map(|kind| AgentEntry {
            name: kind.to_string(),
            enabled: enabled.contains(kind),
        })
        .collect::<Vec<_>>();
    let proposal_agent = known
        .iter()
        .find(|kind| enabled.contains(kind))
        .copied()
        .unwrap_or(AgentKind::Claude)
        .to_string();

    Config {
        agents,
        scan_interval: Interval::Weekly,
        proposal_agent,
        shell: ShellType::detect(),
        notifications: NotificationPref::Both,
        notification_icon: None,
    }
}

fn config_from_spec(spec: &OnboardingSpec) -> Config {
    Config {
        agents: spec.agents.clone(),
        scan_interval: spec.scan_interval.clone(),
        proposal_agent: spec.proposal_agent.clone(),
        shell: spec.shell.clone(),
        notifications: spec.notifications.clone(),
        notification_icon: spec.notification_icon.clone(),
    }
}

fn validate_spec(spec: &OnboardingSpec) -> Result<()> {
    if spec.format_version != onboarding_format_version() {
        bail!(
            "Unsupported onboarding JSON format_version {}. Expected {}.",
            spec.format_version,
            onboarding_format_version()
        );
    }
    if spec.agents.is_empty() {
        bail!("`agents` must include at least one entry.");
    }

    let supported: HashSet<String> = AgentKind::all()
        .into_iter()
        .map(|kind| kind.to_string())
        .collect();
    let mut seen = HashSet::new();
    for entry in &spec.agents {
        if entry.name.trim().is_empty() {
            bail!("Agent names must be non-empty strings.");
        }
        if !seen.insert(entry.name.clone()) {
            bail!("Duplicate agent '{}' in onboarding JSON.", entry.name);
        }
        if !supported.contains(&entry.name) {
            let mut known = supported.iter().cloned().collect::<Vec<_>>();
            known.sort();
            bail!(
                "Unknown agent '{}'. Supported values: {}.",
                entry.name,
                known.join(", ")
            );
        }
    }

    if spec.proposal_agent.trim().is_empty() {
        bail!("`proposal_agent` must be non-empty.");
    }
    if !supported.contains(&spec.proposal_agent) {
        let mut known = supported.iter().cloned().collect::<Vec<_>>();
        known.sort();
        bail!(
            "Unknown proposal_agent '{}'. Supported values: {}.",
            spec.proposal_agent,
            known.join(", ")
        );
    }
    if !seen.contains(&spec.proposal_agent) {
        bail!(
            "`proposal_agent` must also appear in `agents` (got '{}').",
            spec.proposal_agent
        );
    }

    Ok(())
}

fn enabled_agents_label(agents: &[AgentEntry]) -> String {
    let enabled = agents
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        "(none)".to_string()
    } else {
        enabled.join(", ")
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn read_text(path: &Path) -> Result<String> {
    if is_stdio(path) {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("Failed to read onboarding JSON from stdin")?;
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
    use std::path::PathBuf;

    #[test]
    fn test_default_config_from_detected_prefers_installed_agents() {
        let detected = vec![(AgentKind::Claude, true), (AgentKind::Codex, false)];
        let config = default_config_from_detected(&detected);

        assert_eq!(config.proposal_agent, "claude");
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert!(
            config
                .agents
                .iter()
                .find(|agent| agent.name == "claude")
                .expect("claude entry")
                .enabled
        );
        assert!(
            !config
                .agents
                .iter()
                .find(|agent| agent.name == "codex")
                .expect("codex entry")
                .enabled
        );
    }

    #[test]
    fn test_validate_spec_rejects_unknown_agent() {
        let spec = OnboardingSpec {
            format_version: 1,
            detected_agents: vec![],
            agents: vec![AgentEntry {
                name: "unknown".to_string(),
                enabled: true,
            }],
            scan_interval: Interval::Weekly,
            proposal_agent: "unknown".to_string(),
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
            notification_icon: None,
            install_shell_hook: true,
        };
        assert!(validate_spec(&spec).is_err());
    }

    #[test]
    fn test_config_from_spec_preserves_values() {
        let spec = OnboardingSpec {
            format_version: 1,
            detected_agents: vec![],
            agents: vec![
                AgentEntry {
                    name: "claude".to_string(),
                    enabled: true,
                },
                AgentEntry {
                    name: "codex".to_string(),
                    enabled: false,
                },
            ],
            scan_interval: Interval::Daily,
            proposal_agent: "claude".to_string(),
            shell: ShellType::Bash,
            notifications: NotificationPref::Native,
            notification_icon: Some("/tmp/icon.png".to_string()),
            install_shell_hook: false,
        };
        let config = config_from_spec(&spec);
        assert_eq!(config.agents, spec.agents);
        assert_eq!(config.scan_interval, Interval::Daily);
        assert_eq!(config.shell, ShellType::Bash);
        assert_eq!(config.notifications, NotificationPref::Native);
        assert_eq!(config.notification_icon.as_deref(), Some("/tmp/icon.png"));
    }

    #[test]
    fn test_export_spec_existing_config_without_hook_sets_install_shell_hook_false() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config_path = home.join(".distill").join("config.yaml");
        let config = Config {
            agents: vec![AgentEntry {
                name: "claude".to_string(),
                enabled: true,
            }],
            scan_interval: Interval::Weekly,
            proposal_agent: "claude".to_string(),
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
            notification_icon: None,
        };
        config.save_to(&config_path).unwrap();

        let spec = export_spec(home).unwrap();
        assert!(
            !spec.install_shell_hook,
            "should reflect current shell hook state when config already exists"
        );
    }

    #[test]
    fn test_export_spec_existing_config_with_hook_sets_install_shell_hook_true() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config_path = home.join(".distill").join("config.yaml");
        let config = Config {
            agents: vec![AgentEntry {
                name: "claude".to_string(),
                enabled: true,
            }],
            scan_interval: Interval::Weekly,
            proposal_agent: "claude".to_string(),
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
            notification_icon: None,
        };
        config.save_to(&config_path).unwrap();

        let zshrc = PathBuf::from(home).join(".zshrc");
        fs::write(
            &zshrc,
            "# distill hook\ncommand -v distill &>/dev/null && distill notify --check\n",
        )
        .unwrap();

        let spec = export_spec(home).unwrap();
        assert!(spec.install_shell_hook);
    }
}
