use anyhow::{Context, Result};

use crate::agents::{Agent, ClaudeAdapter, CodexAdapter};
use crate::config::Config;
use crate::notify::notify_scan_complete;
use crate::scanner::engine::{self, ScanConfig};

pub fn run(now: bool) -> Result<()> {
    if !now {
        println!("distill scan: scheduled scan not yet implemented. Use --now for immediate scan.");
        return Ok(());
    }

    println!("distill scan: running immediate scan...");

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
    notify_scan_complete(proposals.len(), &config.notifications)?;

    Ok(())
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
