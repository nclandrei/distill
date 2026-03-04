use anyhow::{Context, Result};

use crate::agents::{Agent, ClaudeAdapter, CodexAdapter};
use crate::config::Config;
use crate::notify::notify_scan_complete;
use crate::scanner::engine::{self, ScanConfig};

pub fn run(now: bool) -> Result<()> {
    let trigger = scan_trigger_label(now);
    println!("distill scan: running {trigger} scan...");

    // Load config
    let config = Config::load().context(
        "No config found. Run `distill` first to set up, or create ~/.distill/config.yaml manually.",
    )?;
    Config::ensure_dirs()?;

    // Build agent list from config
    let agents = build_agents(&config);
    if agents.is_empty() {
        println!("No agents enabled in config. Nothing to scan.");
        return Ok(());
    }

    let enabled_names: Vec<_> = agents.iter().map(|a| a.kind().to_string()).collect();
    println!("Scanning agents: {}", enabled_names.join(", "));

    // Run the scan engine
    let scan_config = ScanConfig::from_config(&config);
    let proposals = engine::run_scan(&agents, &scan_config)?;

    // Report results
    if proposals.is_empty() {
        println!("Scan complete. No new proposals.");
    } else {
        println!(
            "Scan complete. {} proposal(s) written to {}",
            proposals.len(),
            Config::proposals_dir().display()
        );
        println!("Run 'distill review' to review them.");
    }

    // Send notification according to the user's preference.
    // notify_scan_complete is a no-op when proposal_count == 0.
    notify_scan_complete(
        proposals.len(),
        &config.notifications,
        config.notification_icon.as_deref(),
    )?;

    Ok(())
}

fn scan_trigger_label(now: bool) -> &'static str {
    if now { "immediate" } else { "scheduled" }
}

fn build_agents(config: &Config) -> Vec<Box<dyn Agent>> {
    let mut agents: Vec<Box<dyn Agent>> = Vec::new();

    for entry in &config.agents {
        if !entry.enabled {
            continue;
        }
        match entry.name.as_str() {
            "claude" => agents.push(Box::new(ClaudeAdapter::new())),
            "codex" => agents.push(Box::new(CodexAdapter::new())),
            other => {
                eprintln!("Warning: unknown agent '{other}' in config, skipping.");
            }
        }
    }

    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_trigger_label_now_true() {
        assert_eq!(scan_trigger_label(true), "immediate");
    }

    #[test]
    fn test_scan_trigger_label_now_false() {
        assert_eq!(scan_trigger_label(false), "scheduled");
    }

    #[test]
    fn test_build_agents_enables_only_known_enabled_agents() {
        let config = Config {
            agents: vec![
                crate::config::AgentEntry {
                    name: "claude".into(),
                    enabled: true,
                },
                crate::config::AgentEntry {
                    name: "codex".into(),
                    enabled: false,
                },
                crate::config::AgentEntry {
                    name: "unknown-agent".into(),
                    enabled: true,
                },
            ],
            ..Config::default()
        };

        let agents = build_agents(&config);
        let kinds: Vec<_> = agents.iter().map(|a| a.kind()).collect();
        assert_eq!(kinds, vec![crate::agents::AgentKind::Claude]);
    }

    #[test]
    fn test_build_agents_keeps_both_supported_agents_when_enabled() {
        let config = Config {
            agents: vec![
                crate::config::AgentEntry {
                    name: "claude".into(),
                    enabled: true,
                },
                crate::config::AgentEntry {
                    name: "codex".into(),
                    enabled: true,
                },
            ],
            ..Config::default()
        };

        let agents = build_agents(&config);
        let kinds: Vec<_> = agents.iter().map(|a| a.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                crate::agents::AgentKind::Claude,
                crate::agents::AgentKind::Codex
            ]
        );
    }
}
