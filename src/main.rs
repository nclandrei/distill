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

#[derive(Parser)]
#[command(name = "distill", version, about = "Monitor AI agent sessions and distill them into reusable skills")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a scan for new skill proposals
    Scan {
        /// Run immediately, bypassing schedule
        #[arg(long)]
        now: bool,
    },
    /// Interactively review pending proposals
    Review,
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
        Some(Commands::Review) => {
            commands::review::run()?;
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
