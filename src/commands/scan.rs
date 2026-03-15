use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::agents::{Agent, from_name};
use crate::config::Config;
use crate::notify::notify_scan_complete;
use crate::run_history::{self, RunCommandKind, RunMetrics, RunRecord, RunTrigger};
use crate::scanner::engine::{self, ScanConfig};

#[derive(Debug, Clone, Copy)]
pub struct ScanRunOptions {
    pub now: bool,
    pub notify: bool,
    pub invocation: ScanInvocation,
    pub record_history: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOutcome {
    pub proposals_written: usize,
    pub backlog_remaining: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanInvocation {
    Manual,
    Scheduled,
    Onboarding,
    ScheduledRun,
}

impl ScanInvocation {
    fn history_trigger(self) -> Option<RunTrigger> {
        match self {
            Self::Manual => Some(RunTrigger::Manual),
            Self::Scheduled => Some(RunTrigger::Scheduled),
            Self::Onboarding => Some(RunTrigger::Onboarding),
            Self::ScheduledRun => None,
        }
    }
}

#[derive(Deserialize)]
struct StoredBacklog {
    #[serde(default)]
    sessions: Vec<serde_json::Value>,
}

pub fn run(now: bool) -> Result<()> {
    let invocation = if now {
        ScanInvocation::Manual
    } else {
        ScanInvocation::Scheduled
    };
    run_with_options(ScanRunOptions {
        now,
        notify: true,
        invocation,
        record_history: true,
    })
    .map(|_| ())
}

pub fn run_initial() -> Result<()> {
    run_with_options(ScanRunOptions {
        now: true,
        notify: true,
        invocation: ScanInvocation::Onboarding,
        record_history: true,
    })
    .map(|_| ())
}

pub fn run_with_options(options: ScanRunOptions) -> Result<ScanOutcome> {
    let started_at = Utc::now();
    let history_dir = Config::history_dir();
    let result = run_scan_once(options);

    if options.record_history
        && let Some(trigger) = options.invocation.history_trigger()
    {
        let finished_at = Utc::now();
        let record = match &result {
            Ok(outcome) => RunRecord::succeeded(
                RunCommandKind::Scan,
                trigger,
                started_at,
                finished_at,
                Some(scan_success_summary(outcome)),
                RunMetrics {
                    proposals_written: Some(outcome.proposals_written),
                    backlog_remaining: Some(outcome.backlog_remaining),
                    ..RunMetrics::default()
                },
                vec![],
            ),
            Err(err) => RunRecord::failed(
                RunCommandKind::Scan,
                trigger,
                started_at,
                finished_at,
                Some("Scan failed.".to_string()),
                err.to_string(),
                last_scan_debug_artifact_path(),
                RunMetrics::default(),
                vec![],
            ),
        };
        run_history::append_run_record_best_effort(&history_dir, &record);
    }

    result
}

fn run_scan_once(options: ScanRunOptions) -> Result<ScanOutcome> {
    let trigger = scan_trigger_label(options.now);
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
        return Ok(ScanOutcome {
            proposals_written: 0,
            backlog_remaining: 0,
        });
    }

    let enabled_names: Vec<_> = agents.iter().map(|a| a.kind().to_string()).collect();
    println!("Scanning agents: {}", enabled_names.join(", "));

    // Run the scan engine
    let scan_config = ScanConfig::from_config(&config);
    let proposals = engine::run_scan(&agents, &scan_config)?;
    let backlog_remaining = load_pending_scan_backlog(&scan_config.backlog_path)?;

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
    if backlog_remaining > 0 {
        println!("Remaining scan backlog after this run: {backlog_remaining} session(s).");
    }

    // Send notification according to the user's preference.
    // notify_scan_complete is a no-op when proposal_count == 0.
    if options.notify {
        notify_scan_complete(
            proposals.len(),
            &config.notifications,
            config.notification_icon.as_deref(),
        )?;
    }

    Ok(ScanOutcome {
        proposals_written: proposals.len(),
        backlog_remaining,
    })
}

fn scan_success_summary(outcome: &ScanOutcome) -> String {
    if outcome.proposals_written == 0 {
        "Scan completed with no new proposals.".to_string()
    } else {
        format!(
            "Scan completed with {} proposal(s) written.",
            outcome.proposals_written
        )
    }
}

fn scan_trigger_label(now: bool) -> &'static str {
    if now { "immediate" } else { "scheduled" }
}

pub(crate) fn last_scan_debug_artifact_path() -> Option<PathBuf> {
    read_pointer_path(
        &Config::base_dir()
            .join("scan-debug")
            .join("last-failed-run.txt"),
    )
}

fn read_pointer_path(path: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
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

fn load_pending_scan_backlog(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }

    let contents = std::fs::read_to_string(path)?;
    let backlog: StoredBacklog = serde_json::from_str(&contents)?;
    Ok(backlog.sessions.len())
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
    fn test_last_scan_debug_artifact_path_reads_pointer_file() {
        let dir = tempfile::tempdir().unwrap();
        let original_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", dir.path()) };
        let scan_debug_dir = Config::base_dir().join("scan-debug");
        std::fs::create_dir_all(&scan_debug_dir).unwrap();
        std::fs::write(
            scan_debug_dir.join("last-failed-run.txt"),
            "/tmp/distill/scan-debug/scan-20260315",
        )
        .unwrap();

        let path = last_scan_debug_artifact_path();

        match original_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert_eq!(
            path,
            Some(PathBuf::from("/tmp/distill/scan-debug/scan-20260315"))
        );
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
                crate::config::AgentEntry {
                    name: "opencode".into(),
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
                crate::agents::AgentKind::Codex,
                crate::agents::AgentKind::OpenCode
            ]
        );
    }
}
