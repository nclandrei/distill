mod agents;
mod commands;
mod config;
mod notify;
mod onboard;
mod proposals;
mod review;
mod scanner;
mod schedule;
mod shell;
mod sync;

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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // No subcommand: run onboarding if first run, otherwise show help hint
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
    }

    Ok(())
}
