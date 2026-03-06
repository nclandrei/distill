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

const CONVERT_LONG_ABOUT: &str = "\
Convert MCP servers into orchestrator + tool skills with an atomic per-server runtime gate.

Default one-shot flow:
  distill convert <server>

V4 pipeline:
  discover -> build -> contract-test -> apply

Backend selection is backend-agnostic (Codex or Claude):
  1) explicit --backend if provided
  2) config convert.backend_preference when available
  3) auto-detect installed backend (codex, then claude)

Use --backend-health to print backend diagnostics.
Use --allow-side-effects, --probe-timeout-seconds, and --probe-retries to control runtime probes.
Use --config <path> to include extra MCP config files.
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
    /// Inspect MCP servers and convert to skills
    #[command(long_about = CONVERT_LONG_ABOUT)]
    Convert {
        /// Server id (source:name) or unique server name
        server: Option<String>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Override output directory for generated skills
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Force backend selection to codex or claude
        #[arg(long, value_parser = ["codex", "claude"])]
        backend: Option<String>,
        /// Enable backend auto-detect/fallback mode
        #[arg(long)]
        backend_auto: bool,
        /// Print backend availability diagnostics
        #[arg(long)]
        backend_health: bool,
        /// Allow executing explicit side-effectful probes during contract testing
        #[arg(long, global = true)]
        allow_side_effects: bool,
        /// Runtime probe timeout in seconds
        #[arg(long = "probe-timeout-seconds", value_name = "N", global = true)]
        probe_timeout_seconds: Option<u64>,
        /// Number of retries for failed runtime probes
        #[arg(long = "probe-retries", value_name = "N", global = true)]
        probe_retries: Option<u32>,
        #[command(subcommand)]
        command: Option<ConvertCommands>,
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

#[derive(Subcommand)]
enum ConvertCommands {
    /// Discover runtime tools and generate backend-neutral dossiers
    Discover {
        /// Server id (source:name) or unique server name
        server: Option<String>,
        /// Discover all servers from config sources
        #[arg(long, conflicts_with = "server")]
        all: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Optional path to write dossier JSON
        #[arg(long = "out", value_name = "PATH")]
        out: Option<PathBuf>,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Build skill files from an existing dossier JSON
    Build {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Override output directory for generated skills
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Run contract tests from an existing dossier JSON
    ContractTest {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Optional path to write contract-test report JSON
        #[arg(long = "report", value_name = "PATH")]
        report: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Apply a fully passing dossier: write skills and remove MCP config entry
    Apply {
        /// Input dossier JSON path
        #[arg(long = "from-dossier", value_name = "PATH")]
        from_dossier: PathBuf,
        /// Required confirmation because this mutates MCP config
        #[arg(long)]
        yes: bool,
        /// Override output directory for generated skills
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// List discovered MCP servers
    List {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Inspect one MCP server by id or by unique name
    Inspect {
        /// Server id (source:name) or unique server name
        server: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Generate a conversion plan for one MCP server
    Plan {
        /// Server id (source:name) or unique server name
        server: String,
        /// Planning mode (auto resolves from recommendation)
        #[arg(long, default_value = "auto", value_parser = ["auto", "hybrid", "replace"])]
        mode: String,
        /// Explicit no-op apply guard (planning only)
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
    },
    /// Verify parity coverage between generated skill and live MCP tool list
    Verify {
        /// Server id (source:name) or unique server name
        server: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Additional MCP config file paths to inspect
        #[arg(long = "config", value_name = "PATH")]
        config: Vec<PathBuf>,
        /// Override skills directory for generated files
        #[arg(long = "skills-dir", value_name = "PATH")]
        skills_dir: Option<PathBuf>,
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
        Some(Commands::Convert {
            server,
            json,
            config,
            skills_dir,
            backend,
            backend_auto,
            backend_health,
            allow_side_effects,
            probe_timeout_seconds,
            probe_retries,
            command,
        }) => {
            let app_config = config::Config::load().unwrap_or_default();
            match command {
                Some(ConvertCommands::Discover {
                    server,
                    all,
                    json,
                    out,
                    config,
                }) => {
                    commands::convert::run_discover_v3(
                        server.as_deref(),
                        all,
                        json,
                        out,
                        &config,
                        backend.as_deref(),
                        backend_auto,
                        backend_health,
                        &app_config,
                    )?;
                }
                Some(ConvertCommands::Build {
                    from_dossier,
                    skills_dir,
                    json,
                }) => {
                    commands::convert::run_build_v3(
                        &from_dossier,
                        skills_dir,
                        json,
                        backend_health,
                        &app_config,
                    )?;
                }
                Some(ConvertCommands::ContractTest {
                    from_dossier,
                    report,
                    json,
                }) => {
                    commands::convert::run_contract_test_v3(
                        &from_dossier,
                        report.as_deref(),
                        json,
                        backend_health,
                        allow_side_effects,
                        probe_timeout_seconds,
                        probe_retries,
                        &app_config,
                    )?;
                }
                Some(ConvertCommands::Apply {
                    from_dossier,
                    yes,
                    skills_dir,
                    json,
                }) => {
                    commands::convert::run_apply_v3(
                        &from_dossier,
                        yes,
                        skills_dir,
                        json,
                        backend_health,
                        allow_side_effects,
                        probe_timeout_seconds,
                        probe_retries,
                        &app_config,
                    )?;
                }
                Some(ConvertCommands::List { json, config }) => {
                    commands::convert::run_list(json, &config)?;
                }
                Some(ConvertCommands::Inspect {
                    server,
                    json,
                    config,
                }) => {
                    commands::convert::run_inspect(&server, json, &config)?;
                }
                Some(ConvertCommands::Plan {
                    server,
                    mode,
                    dry_run,
                    json,
                    config,
                }) => {
                    commands::convert::run_plan(&server, &mode, dry_run, json, &config)?;
                }
                Some(ConvertCommands::Verify {
                    server,
                    json,
                    config,
                    skills_dir,
                }) => {
                    commands::convert::run_verify(&server, json, &config, skills_dir)?;
                }
                None => {
                    if let Some(server) = server {
                        commands::convert::run_one_shot_v3(
                            &server,
                            json,
                            &config,
                            skills_dir,
                            backend.as_deref(),
                            backend_auto,
                            backend_health,
                            allow_side_effects,
                            probe_timeout_seconds,
                            probe_retries,
                            &app_config,
                        )?;
                    } else {
                        commands::convert::run_overview_v3(&config)?;
                    }
                }
            }
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
