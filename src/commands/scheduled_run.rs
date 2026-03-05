use anyhow::{Context, Result};

use crate::commands;
use crate::config::Config;

pub fn run() -> Result<()> {
    println!("distill scheduled-run: starting scan stage...");
    commands::scan::run(false)?;

    let config = Config::load().context(
        "No config found. Run `distill` first to set up, or create ~/.distill/config.yaml manually.",
    )?;

    if config.sync_agents.projects.is_empty() {
        println!("distill scheduled-run: sync-agents skipped (no configured projects).");
        return Ok(());
    }

    println!("distill scheduled-run: starting sync-agents stage...");
    commands::sync_agents::run(&[], true, false, false, false, None)?;
    Ok(())
}
