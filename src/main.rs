mod agents;
mod commands;
mod config;
mod notify;
mod onboard;
mod preferences;
mod proposals;
mod review;
mod scanner;
mod schedule;
mod shell;
mod sync;
mod sync_agents;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

const CLI_AFTER_HELP: &str = "\
AI-friendly one-shot flow:
  1) distill onboard --write-json onboarding.json
  2) Edit onboarding.json
  3) distill onboard --apply-json onboarding.json
  4) distill review --write-json review.json
  5) Fill review.json decisions (accept/reject/skip)
  6) distill review --apply-json review.json
";

const ONBOARD_LONG_ABOUT: &str = "\
Configure distill onboarding.

Default behavior is the interactive TUI. For agent automation, use JSON mode:
  distill onboard --write-json onboarding.json
  distill onboard --apply-json onboarding.json

The JSON includes all configuration values and install_shell_hook.
Use '-' as the path to read from stdin or write to stdout.
";

const ONBOARD_AFTER_HELP: &str = "\
Example onboarding JSON:
{
  \"format_version\": 1,
  \"agents\": [{\"name\": \"claude\", \"enabled\": true}, {\"name\": \"codex\", \"enabled\": false}],
  \"scan_interval\": \"weekly\",
  \"proposal_agent\": \"claude\",
  \"shell\": \"zsh\",
  \"notifications\": \"both\",
  \"notification_icon\": null,
  \"install_shell_hook\": true
}
";

const REVIEW_LONG_ABOUT: &str = "\
Review pending proposals.

Default behavior is the interactive TUI. For agent automation, use JSON mode:
  distill review --write-json review.json
  distill review --apply-json review.json

Each proposal entry may include a 'decision' field:
  accept | reject | skip
Missing decisions default to skip.
Use '-' as the path to read from stdin or write to stdout.
";

const REVIEW_AFTER_HELP: &str = "\
One-shot AI review workflow:
  1) distill review --write-json review.json
  2) Set each proposal decision: accept | reject | skip
  3) distill review --apply-json review.json
";

const SYNC_AGENTS_LONG_ABOUT: &str = "\
Propose AGENTS.md drift updates from git + session evidence.

Examples:
  distill sync-agents --projects /abs/repo --dry-run
  distill sync-agents --projects /abs/repo1,/abs/repo2 --save-projects
  distill sync-agents --all-configured
  distill sync-agents --list-configured

`--since` accepts YYYY-MM-DD or RFC3339.
";

#[derive(Parser)]
#[command(
    name = "distill",
    version,
    about = "Monitor AI agent sessions and distill them into reusable skills",
    after_long_help = CLI_AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run onboarding (interactive by default, JSON-driven when flags are used)
    #[command(long_about = ONBOARD_LONG_ABOUT, after_long_help = ONBOARD_AFTER_HELP)]
    Onboard {
        /// Write onboarding JSON template to PATH (`-` for stdout)
        #[arg(long, value_name = "PATH", conflicts_with = "apply_json")]
        write_json: Option<PathBuf>,
        /// Apply onboarding JSON from PATH (`-` for stdin)
        #[arg(long, value_name = "PATH", conflicts_with = "write_json")]
        apply_json: Option<PathBuf>,
    },
    /// Run a scan for new skill proposals
    Scan {
        /// Run immediately, bypassing schedule
        #[arg(long)]
        now: bool,
    },
    /// Review proposals (interactive by default, JSON-driven when flags are used)
    #[command(long_about = REVIEW_LONG_ABOUT, after_long_help = REVIEW_AFTER_HELP)]
    Review {
        /// Write pending proposals as JSON to PATH (`-` for stdout)
        #[arg(long, value_name = "PATH", conflicts_with = "apply_json")]
        write_json: Option<PathBuf>,
        /// Apply review decisions from JSON at PATH (`-` for stdin)
        #[arg(long, value_name = "PATH", conflicts_with = "write_json")]
        apply_json: Option<PathBuf>,
    },
    /// Detect duplicate global skills and create remove proposals
    Dedupe {
        /// Preview duplicates without writing proposals
        #[arg(long)]
        dry_run: bool,
    },
    /// Show current config, last scan, and pending proposals
    Status,
    /// Manage the scheduled scan watcher
    Watch {
        /// Install the scheduled watcher
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        /// Remove the scheduled watcher
        #[arg(long, conflicts_with = "install")]
        uninstall: bool,
    },
    /// Check for and display pending proposal notifications
    Notify {
        /// Check for pending proposals (used by shell hook)
        #[arg(long)]
        check: bool,
    },
    /// Propose AGENTS.md updates for selected projects
    #[command(long_about = SYNC_AGENTS_LONG_ABOUT)]
    SyncAgents {
        /// Comma-separated absolute project paths
        #[arg(
            long,
            value_name = "PATHS",
            value_delimiter = ',',
            num_args = 1..,
            conflicts_with = "all_configured"
        )]
        projects: Vec<String>,
        /// Use projects saved in config sync_agents.projects
        #[arg(long, conflicts_with = "projects")]
        all_configured: bool,
        /// Persist --projects into config sync_agents.projects
        #[arg(long, requires = "projects")]
        save_projects: bool,
        /// Print configured sync-agents project allowlist and exit
        #[arg(
            long,
            conflicts_with_all = ["projects", "all_configured", "save_projects", "dry_run", "since"]
        )]
        list_configured: bool,
        /// Preview proposals without writing files or watermark
        #[arg(long)]
        dry_run: bool,
        /// Evidence window start timestamp (YYYY-MM-DD or RFC3339)
        #[arg(long, value_name = "DATE_OR_RFC3339")]
        since: Option<String>,
    },
    /// Internal: run scheduled scan then sync-agents for configured projects
    #[command(hide = true)]
    ScheduledRun,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            if config::Config::exists() {
                println!("distill is configured. Run 'distill status' to see current state.");
                println!("Run 'distill scan --now' to scan for new proposals.");
                println!("Run 'distill review' to review pending proposals.");
            } else {
                onboard::run_interactive()?;
            }
        }
        Some(Commands::Scan { now }) => {
            commands::scan::run(now)?;
        }
        Some(Commands::Onboard {
            write_json,
            apply_json,
        }) => {
            commands::onboard::run(write_json.as_deref(), apply_json.as_deref())?;
        }
        Some(Commands::Review {
            write_json,
            apply_json,
        }) => {
            commands::review::run(write_json.as_deref(), apply_json.as_deref())?;
        }
        Some(Commands::Dedupe { dry_run }) => {
            commands::dedupe::run(dry_run)?;
        }
        Some(Commands::Status) => {
            commands::status::run()?;
        }
        Some(Commands::Watch { install, uninstall }) => {
            commands::watch::run(install, uninstall)?;
        }
        Some(Commands::Notify { check }) => {
            commands::notify::run(check)?;
        }
        Some(Commands::SyncAgents {
            projects,
            all_configured,
            save_projects,
            list_configured,
            dry_run,
            since,
        }) => {
            commands::sync_agents::run(
                &projects,
                all_configured,
                save_projects,
                list_configured,
                dry_run,
                since.as_deref(),
            )?;
        }
        Some(Commands::ScheduledRun) => {
            commands::scheduled_run::run()?;
        }
    }

    Ok(())
}
