use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::time::{Duration, Instant};

use crate::commands;
use crate::config::Config;
use crate::notify::notify_scan_complete;
use crate::run_history::{
    self, RunCommandKind, RunMetrics, RunRecord, RunStageRecord, RunTrigger, stage_record,
};

const DEFAULT_SCHEDULED_RUN_MAX_BATCHES: usize = 3;
const DEFAULT_SCHEDULED_RUN_MAX_SECS: u64 = 1800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledCatchUpBudget {
    max_batches: Option<usize>,
    max_duration: Option<Duration>,
}

pub fn run() -> Result<()> {
    let history_dir = Config::history_dir();
    let started_at = Utc::now();
    let mut stage_records: Vec<RunStageRecord> = Vec::new();
    let mut overall_metrics = RunMetrics::default();
    let mut failure_debug_path = None;
    let mut completion_summary = "scheduled-run completed.".to_string();

    let result = (|| -> Result<()> {
        println!("distill scheduled-run: starting scan stage...");
        let budget = scheduled_catch_up_budget()?;
        println!(
            "distill scheduled-run: automatic catch-up budget = {} batch(es), {}.",
            describe_optional_count(budget.max_batches, "unlimited"),
            describe_optional_duration(budget.max_duration, "no time limit")
        );

        let scan_started_at = Instant::now();
        let mut batches_run = 0usize;
        let mut total_scan_proposals = 0usize;
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
                && scan_started_at.elapsed() >= max_duration
            {
                println!(
                    "distill scheduled-run: stopping automatic catch-up after {} elapsed; {final_backlog} pending session(s) remain.",
                    format_duration(scan_started_at.elapsed())
                );
                break;
            }

            let outcome = match commands::scan::run_with_options(commands::scan::ScanRunOptions {
                now: false,
                notify: false,
                invocation: commands::scan::ScanInvocation::ScheduledRun,
                record_history: false,
            }) {
                Ok(outcome) => outcome,
                Err(err) => {
                    failure_debug_path = commands::scan::last_scan_debug_artifact_path();
                    overall_metrics = RunMetrics {
                        proposals_written: Some(total_scan_proposals),
                        backlog_remaining: Some(final_backlog),
                        batches_run: Some(batches_run),
                        ..RunMetrics::default()
                    };
                    stage_records.push(stage_record(
                        "scan",
                        false,
                        Some("scheduled scan stage failed.".to_string()),
                        Some(err.to_string()),
                        failure_debug_path.clone(),
                        overall_metrics.clone(),
                    ));
                    return Err(err);
                }
            };

            batches_run += 1;
            total_scan_proposals += outcome.proposals_written;
            final_backlog = outcome.backlog_remaining;

            if final_backlog == 0 {
                println!(
                    "distill scheduled-run: scan backlog drained after {batches_run} batch(es)."
                );
                break;
            }

            println!(
                "distill scheduled-run: continuing automatic backlog catch-up (remaining: {final_backlog} session(s))."
            );
        }

        let scan_metrics = RunMetrics {
            proposals_written: Some(total_scan_proposals),
            backlog_remaining: Some(final_backlog),
            batches_run: Some(batches_run),
            ..RunMetrics::default()
        };
        overall_metrics = scan_metrics.clone();
        stage_records.push(stage_record(
            "scan",
            true,
            Some(scheduled_scan_stage_summary(
                total_scan_proposals,
                batches_run,
                final_backlog,
            )),
            None,
            None,
            scan_metrics.clone(),
        ));

        let config = Config::load().context(
            "No config found. Run `distill` first to set up, or create ~/.distill/config.yaml manually.",
        )?;
        notify_scan_complete(
            total_scan_proposals,
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
            stage_records.push(stage_record(
                "sync-agents",
                true,
                Some("sync-agents skipped (no configured projects).".to_string()),
                None,
                None,
                RunMetrics::default(),
            ));
            completion_summary = format!(
                "scheduled-run completed with {} scan proposal(s); sync-agents skipped.",
                total_scan_proposals
            );
            return Ok(());
        }

        println!("distill scheduled-run: starting sync-agents stage...");
        let sync_outcome = match commands::sync_agents::run_with_options(
            &[],
            true,
            false,
            false,
            false,
            None,
            commands::sync_agents::SyncAgentsCommandOptions {
                record_history: false,
                trigger: None,
            },
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
                stage_records.push(stage_record(
                    "sync-agents",
                    false,
                    Some("sync-agents stage failed.".to_string()),
                    Some(err.to_string()),
                    None,
                    RunMetrics::default(),
                ));
                return Err(err);
            }
        };

        let sync_metrics = commands::sync_agents::sync_agents_metrics(&sync_outcome.summary);
        stage_records.push(stage_record(
            "sync-agents",
            true,
            Some(commands::sync_agents::sync_agents_summary_text(
                &sync_outcome.summary,
                false,
            )),
            None,
            None,
            sync_metrics.clone(),
        ));

        overall_metrics = RunMetrics {
            proposals_written: Some(total_scan_proposals + sync_outcome.summary.proposals_written),
            proposals_skipped_pending: sync_metrics.proposals_skipped_pending,
            backlog_remaining: Some(final_backlog),
            batches_run: Some(batches_run),
            projects_evaluated: sync_metrics.projects_evaluated,
            projects_updated: sync_metrics.projects_updated,
            projects_unchanged: sync_metrics.projects_unchanged,
            projects_skipped: sync_metrics.projects_skipped,
        };
        completion_summary = format!(
            "scheduled-run completed with {} scan proposal(s) and {} sync proposal(s).",
            total_scan_proposals, sync_outcome.summary.proposals_written
        );
        Ok(())
    })();

    let finished_at = Utc::now();
    let record = match &result {
        Ok(()) => RunRecord::succeeded(
            RunCommandKind::ScheduledRun,
            RunTrigger::Scheduled,
            started_at,
            finished_at,
            Some(completion_summary),
            overall_metrics,
            stage_records,
        ),
        Err(err) => RunRecord::failed(
            RunCommandKind::ScheduledRun,
            RunTrigger::Scheduled,
            started_at,
            finished_at,
            Some("scheduled-run failed.".to_string()),
            err.to_string(),
            failure_debug_path,
            overall_metrics,
            stage_records,
        ),
    };
    run_history::append_run_record_best_effort(&history_dir, &record);

    result
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

fn scheduled_scan_stage_summary(
    total_scan_proposals: usize,
    batches_run: usize,
    final_backlog: usize,
) -> String {
    if final_backlog == 0 {
        format!(
            "scan stage drained backlog after {batches_run} batch(es) with {total_scan_proposals} proposal(s)."
        )
    } else {
        format!(
            "scan stage stopped after {batches_run} batch(es) with {final_backlog} session(s) remaining and {total_scan_proposals} proposal(s)."
        )
    }
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
