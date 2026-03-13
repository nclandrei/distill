use anyhow::{Context, Result, bail};
use std::time::{Duration, Instant};

use crate::commands;
use crate::config::Config;
use crate::notify::notify_scan_complete;

const DEFAULT_SCHEDULED_RUN_MAX_BATCHES: usize = 3;
const DEFAULT_SCHEDULED_RUN_MAX_SECS: u64 = 1800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledCatchUpBudget {
    max_batches: Option<usize>,
    max_duration: Option<Duration>,
}

pub fn run() -> Result<()> {
    println!("distill scheduled-run: starting scan stage...");
    let budget = scheduled_catch_up_budget()?;
    println!(
        "distill scheduled-run: automatic catch-up budget = {} batch(es), {}.",
        describe_optional_count(budget.max_batches, "unlimited"),
        describe_optional_duration(budget.max_duration, "no time limit")
    );

    let started_at = Instant::now();
    let mut batches_run = 0usize;
    let mut total_proposals = 0usize;
    let mut final_backlog = 0usize;

    loop {
        if let Some(max_batches) = budget.max_batches
            && batches_run >= max_batches
        {
            println!(
                "distill scheduled-run: stopping automatic catch-up after {batches_run} batch(es); {final_backlog} pending session(s) remain."
            );
            break;
        }
        if let Some(max_duration) = budget.max_duration
            && batches_run > 0
            && started_at.elapsed() >= max_duration
        {
            println!(
                "distill scheduled-run: stopping automatic catch-up after {} elapsed; {final_backlog} pending session(s) remain.",
                format_duration(started_at.elapsed())
            );
            break;
        }

        let outcome = commands::scan::run_with_options(commands::scan::ScanRunOptions {
            now: false,
            notify: false,
        })?;
        batches_run += 1;
        total_proposals += outcome.proposals_written;
        final_backlog = outcome.backlog_remaining;

        if final_backlog == 0 {
            println!("distill scheduled-run: scan backlog drained after {batches_run} batch(es).");
            break;
        }

        println!(
            "distill scheduled-run: continuing automatic backlog catch-up (remaining: {final_backlog} session(s))."
        );
    }

    let config = Config::load().context(
        "No config found. Run `distill` first to set up, or create ~/.distill/config.yaml manually.",
    )?;
    notify_scan_complete(
        total_proposals,
        &config.notifications,
        config.notification_icon.as_deref(),
    )?;

    if final_backlog > 0 {
        println!(
            "distill scheduled-run: future scheduled runs will continue automatically. If new sessions arrive faster than scans process them, the backlog may still grow."
        );
        println!(
            "distill scheduled-run: run `distill scan --now` any time to accelerate catch-up."
        );
    }

    if config.sync_agents.projects.is_empty() {
        println!("distill scheduled-run: sync-agents skipped (no configured projects).");
        return Ok(());
    }

    println!("distill scheduled-run: starting sync-agents stage...");
    commands::sync_agents::run(&[], true, false, false, false, None)?;
    Ok(())
}

fn scheduled_catch_up_budget() -> Result<ScheduledCatchUpBudget> {
    Ok(ScheduledCatchUpBudget {
        max_batches: scheduled_run_max_batches()?,
        max_duration: scheduled_run_max_duration()?,
    })
}

fn scheduled_run_max_batches() -> Result<Option<usize>> {
    parse_optional_positive_usize_env(
        "DISTILL_SCHEDULED_RUN_MAX_BATCHES",
        Some(DEFAULT_SCHEDULED_RUN_MAX_BATCHES),
    )
}

fn scheduled_run_max_duration() -> Result<Option<Duration>> {
    Ok(parse_optional_positive_u64_env(
        "DISTILL_SCHEDULED_RUN_MAX_SECS",
        Some(DEFAULT_SCHEDULED_RUN_MAX_SECS),
    )?
    .map(Duration::from_secs))
}

fn parse_optional_positive_usize_env(name: &str, default: Option<usize>) -> Result<Option<usize>> {
    match std::env::var(name) {
        Ok(raw) => {
            let value: usize = raw.parse().with_context(|| {
                format!("Failed to parse {name}={raw:?} as a non-negative integer")
            })?;
            if value == 0 {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} must be valid Unicode."),
    }
}

fn parse_optional_positive_u64_env(name: &str, default: Option<u64>) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(raw) => {
            let value: u64 = raw.parse().with_context(|| {
                format!("Failed to parse {name}={raw:?} as a non-negative integer")
            })?;
            if value == 0 {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} must be valid Unicode."),
    }
}

fn describe_optional_count(value: Option<usize>, unlimited_label: &str) -> String {
    value
        .map(|count| count.to_string())
        .unwrap_or_else(|| unlimited_label.to_string())
}

fn describe_optional_duration(value: Option<Duration>, unlimited_label: &str) -> String {
    value
        .map(format_duration)
        .unwrap_or_else(|| unlimited_label.to_string())
}

fn format_duration(duration: Duration) -> String {
    format!("{}s", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_optional_positive_usize_env_zero_disables_limit() {
        unsafe { std::env::set_var("DISTILL_SCHEDULED_RUN_MAX_BATCHES", "0") };
        let result = scheduled_run_max_batches().unwrap();
        unsafe { std::env::remove_var("DISTILL_SCHEDULED_RUN_MAX_BATCHES") };
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_optional_positive_u64_env_uses_default_when_missing() {
        unsafe { std::env::remove_var("DISTILL_SCHEDULED_RUN_MAX_SECS") };
        let result = scheduled_run_max_duration().unwrap();
        assert_eq!(
            result,
            Some(Duration::from_secs(DEFAULT_SCHEDULED_RUN_MAX_SECS))
        );
    }
}
