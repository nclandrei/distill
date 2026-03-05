use anyhow::{Context, Result, bail};

use crate::agents::{Agent, ClaudeAdapter, CodexAdapter};
use crate::config::Config;
use crate::sync_agents::{
    ProjectStatus, SyncAgentsRunConfig, parse_since, resolve_projects, run_sync_agents,
};

pub fn run(
    projects: &[String],
    all_configured: bool,
    save_projects: bool,
    list_configured: bool,
    dry_run: bool,
    since: Option<&str>,
) -> Result<()> {
    let mut config = Config::load().context(
        "No config found. Run `distill` first to set up, or create ~/.distill/config.yaml manually.",
    )?;

    if list_configured {
        if config.sync_agents.projects.is_empty() {
            println!("No configured sync-agents projects.");
        } else {
            println!("Configured sync-agents projects:");
            for project in &config.sync_agents.projects {
                println!("- {project}");
            }
        }
        return Ok(());
    }

    let selected_raw = if all_configured {
        if config.sync_agents.projects.is_empty() {
            bail!(
                "No configured sync-agents projects. Add them with: distill sync-agents --projects /abs/repo --save-projects"
            );
        }
        config.sync_agents.projects.clone()
    } else if !projects.is_empty() {
        projects.to_vec()
    } else {
        bail!("No projects selected. Use --projects /abs/repo[,/abs/repo2] or --all-configured.");
    };

    let resolved_projects = resolve_projects(&selected_raw)?;

    if save_projects {
        config.sync_agents.projects = resolved_projects
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        config.save()?;
        println!(
            "Saved {} project(s) to sync-agents allowlist.",
            config.sync_agents.projects.len()
        );
        println!("Scheduled runs via 'distill watch --install' will use this saved allowlist.");
    }

    let since_override = since.map(parse_since).transpose()?;

    Config::ensure_dirs()?;
    let agents = build_agents(&config);

    let run_config = SyncAgentsRunConfig {
        proposal_agent: config.proposal_agent.clone(),
        proposals_dir: Config::proposals_dir(),
        last_sync_path: Config::last_sync_agents_path(),
        dry_run,
        since_override,
    };

    let summary = run_sync_agents(&resolved_projects, &agents, &run_config)?;

    println!(
        "sync-agents: evaluated {} project(s) since {}",
        summary.results.len(),
        summary.since.to_rfc3339()
    );

    for result in &summary.results {
        let status = match &result.status {
            ProjectStatus::Updated => "Updated".to_string(),
            ProjectStatus::NoChanges => "No changes".to_string(),
            ProjectStatus::Skipped(reason) => format!("Skipped ({reason})"),
        };
        println!(
            "- {}: {} [commits={}, files={}, sessions={}, written={}, skipped-pending={}]",
            result.project.display(),
            status,
            result.commit_count,
            result.file_count,
            result.session_count,
            result.proposals_written,
            result.proposals_skipped_pending
        );
    }

    if dry_run {
        println!("Dry run: no proposals were written and watermark was not updated.");
    } else {
        println!(
            "Wrote {} proposal(s), skipped {} due to existing pending AGENTS.md targets.",
            summary.proposals_written, summary.proposals_skipped_pending
        );
        if summary.proposals_written > 0 {
            println!("Run 'distill review' to accept/reject AGENTS.md proposals.");
        }
    }

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
            other => eprintln!("Warning: unknown agent '{other}' in config, skipping."),
        }
    }

    agents
}
