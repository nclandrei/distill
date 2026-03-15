use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::agents::{Agent, from_name};
use crate::config::Config;
use crate::run_history::{self, RunCommandKind, RunMetrics, RunRecord, RunTrigger};
use crate::sync_agents::{
    ProjectStatus, SyncAgentsRunConfig, SyncAgentsSummary, parse_since, resolve_projects,
    run_sync_agents,
};

#[derive(Debug, Clone)]
pub struct SyncAgentsCommandOutcome {
    pub summary: SyncAgentsSummary,
}

#[derive(Debug, Clone, Copy)]
pub struct SyncAgentsCommandOptions {
    pub record_history: bool,
    pub trigger: Option<RunTrigger>,
}

pub fn run(
    projects: &[String],
    all_configured: bool,
    save_projects: bool,
    list_configured: bool,
    dry_run: bool,
    since: Option<&str>,
) -> Result<()> {
    run_with_options(
        projects,
        all_configured,
        save_projects,
        list_configured,
        dry_run,
        since,
        SyncAgentsCommandOptions {
            record_history: true,
            trigger: Some(RunTrigger::Manual),
        },
    )
    .map(|_| ())
}

pub fn run_with_options(
    projects: &[String],
    all_configured: bool,
    save_projects: bool,
    list_configured: bool,
    dry_run: bool,
    since: Option<&str>,
    options: SyncAgentsCommandOptions,
) -> Result<SyncAgentsCommandOutcome> {
    if list_configured {
        return run_impl(
            projects,
            all_configured,
            save_projects,
            list_configured,
            dry_run,
            since,
        );
    }

    let started_at = Utc::now();
    let history_dir = Config::history_dir();
    let result = run_impl(
        projects,
        all_configured,
        save_projects,
        list_configured,
        dry_run,
        since,
    );

    if options.record_history
        && let Some(trigger) = options.trigger
    {
        let finished_at = Utc::now();
        let record = match &result {
            Ok(outcome) => RunRecord::succeeded(
                RunCommandKind::SyncAgents,
                trigger,
                started_at,
                finished_at,
                Some(sync_agents_summary_text(&outcome.summary, dry_run)),
                sync_agents_metrics(&outcome.summary),
                vec![],
            ),
            Err(err) => RunRecord::failed(
                RunCommandKind::SyncAgents,
                trigger,
                started_at,
                finished_at,
                Some("sync-agents failed.".to_string()),
                err.to_string(),
                None,
                RunMetrics::default(),
                vec![],
            ),
        };
        run_history::append_run_record_best_effort(&history_dir, &record);
    }

    result
}

fn run_impl(
    projects: &[String],
    all_configured: bool,
    save_projects: bool,
    list_configured: bool,
    dry_run: bool,
    since: Option<&str>,
) -> Result<SyncAgentsCommandOutcome> {
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
        return Ok(SyncAgentsCommandOutcome {
            summary: SyncAgentsSummary {
                since: Utc::now(),
                proposals_written: 0,
                proposals_skipped_pending: 0,
                results: vec![],
            },
        });
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

    Ok(SyncAgentsCommandOutcome { summary })
}

pub fn sync_agents_metrics(summary: &SyncAgentsSummary) -> RunMetrics {
    let mut projects_updated = 0usize;
    let mut projects_unchanged = 0usize;
    let mut projects_skipped = 0usize;

    for result in &summary.results {
        match &result.status {
            ProjectStatus::Updated => projects_updated += 1,
            ProjectStatus::NoChanges => projects_unchanged += 1,
            ProjectStatus::Skipped(_) => projects_skipped += 1,
        }
    }

    RunMetrics {
        proposals_written: Some(summary.proposals_written),
        proposals_skipped_pending: Some(summary.proposals_skipped_pending),
        projects_evaluated: Some(summary.results.len()),
        projects_updated: Some(projects_updated),
        projects_unchanged: Some(projects_unchanged),
        projects_skipped: Some(projects_skipped),
        ..RunMetrics::default()
    }
}

pub fn sync_agents_summary_text(summary: &SyncAgentsSummary, dry_run: bool) -> String {
    if dry_run {
        format!(
            "sync-agents dry run evaluated {} project(s).",
            summary.results.len()
        )
    } else {
        format!(
            "sync-agents evaluated {} project(s) and wrote {} proposal(s).",
            summary.results.len(),
            summary.proposals_written
        )
    }
}

fn build_agents(config: &Config) -> Vec<Box<dyn Agent>> {
    let mut agents: Vec<Box<dyn Agent>> = Vec::new();

    for entry in &config.agents {
        if !entry.enabled {
            continue;
        }

        if let Some(agent) = from_name(
            entry.name.as_str(),
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ) {
            agents.push(agent);
        } else {
            eprintln!(
                "Warning: unknown agent '{}' in config, skipping.",
                entry.name
            );
        }
    }

    agents
}
