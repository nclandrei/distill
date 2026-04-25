use crate::config::{Config, Interval};
use crate::scanner::reader::LastScan;
use crate::schedule::{ScheduleCadence, SchedulerStatus, schedule_cadence};
use anyhow::Result;
use chrono::{DateTime, Datelike, Local, SecondsFormat, TimeZone, Utc, Weekday};
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct StoredBacklog {
    #[serde(default)]
    sessions: Vec<serde_json::Value>,
}

/// All the data needed to render the status output.
pub struct StatusInfo {
    pub config: Config,
    pub pending_proposals: usize,
    pub existing_skills: usize,
    pub pending_scan_backlog: usize,
    /// Human-readable timestamp string from `last-scan.json`, or `None` when
    /// the file does not exist (i.e. the tool has never run a scan).
    pub last_scan: Option<String>,
    /// Human-readable schedule status. This is either the next scheduled slot
    /// or an overdue marker when the last scan missed the latest slot.
    pub next_scheduled_scan: Option<String>,
    /// Whether the platform scheduler (launchd plist / systemd timer) is
    /// installed, plus the path to the file that was checked.
    pub scheduler_installed: bool,
    pub scheduler_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduledScanTiming {
    Next(DateTime<Utc>),
    Overdue(DateTime<Utc>),
}

fn format_utc_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn format_scheduled_scan_timing(timing: ScheduledScanTiming) -> String {
    match timing {
        ScheduledScanTiming::Next(timestamp) => format_utc_timestamp(timestamp),
        ScheduledScanTiming::Overdue(timestamp) => {
            format!("overdue since {}", format_utc_timestamp(timestamp))
        }
    }
}

fn scheduled_scan_timing_in_timezone<Tz>(
    interval: &Interval,
    last_scan: DateTime<Utc>,
    now_utc: DateTime<Utc>,
    timezone: &Tz,
) -> ScheduledScanTiming
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    let now_local = now_utc.with_timezone(timezone);
    let last_scan_local = last_scan.with_timezone(timezone);
    let (latest_slot, next_slot) = surrounding_schedule_slots(interval, now_local, timezone);

    if last_scan_local < latest_slot {
        ScheduledScanTiming::Overdue(latest_slot.with_timezone(&Utc))
    } else {
        ScheduledScanTiming::Next(next_slot.with_timezone(&Utc))
    }
}

fn surrounding_schedule_slots<Tz>(
    interval: &Interval,
    now_local: DateTime<Tz>,
    timezone: &Tz,
) -> (DateTime<Tz>, DateTime<Tz>)
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    let cadence = schedule_cadence(interval);
    let today = now_local.date_naive();

    match (cadence.day_of_month, cadence.weekday) {
        (None, None) => {
            let today_slot = local_slot(timezone, today, cadence);
            if today_slot <= now_local {
                (
                    today_slot,
                    local_slot(
                        timezone,
                        today
                            .succ_opt()
                            .expect("daily schedule should have next day"),
                        cadence,
                    ),
                )
            } else {
                (
                    local_slot(
                        timezone,
                        today
                            .pred_opt()
                            .expect("daily schedule should have previous day"),
                        cadence,
                    ),
                    today_slot,
                )
            }
        }
        (None, Some(weekday_number)) => {
            let target_weekday = launchd_weekday_to_chrono(weekday_number);
            let days_since_target = days_since_weekday(today.weekday(), target_weekday);
            let candidate_date = today - chrono::Duration::days(days_since_target);
            let candidate_slot = local_slot(timezone, candidate_date, cadence);

            if candidate_slot <= now_local {
                (
                    candidate_slot,
                    local_slot(
                        timezone,
                        candidate_date + chrono::Duration::weeks(1),
                        cadence,
                    ),
                )
            } else {
                (
                    local_slot(
                        timezone,
                        candidate_date - chrono::Duration::weeks(1),
                        cadence,
                    ),
                    candidate_slot,
                )
            }
        }
        (Some(day_of_month), None) => {
            let current_month_slot = local_slot(
                timezone,
                month_day(today.year(), today.month(), day_of_month),
                cadence,
            );
            if current_month_slot <= now_local {
                let (next_year, next_month) = next_month(today.year(), today.month());
                (
                    current_month_slot,
                    local_slot(
                        timezone,
                        month_day(next_year, next_month, day_of_month),
                        cadence,
                    ),
                )
            } else {
                let (previous_year, previous_month) = previous_month(today.year(), today.month());
                (
                    local_slot(
                        timezone,
                        month_day(previous_year, previous_month, day_of_month),
                        cadence,
                    ),
                    current_month_slot,
                )
            }
        }
        (Some(_), Some(_)) => unreachable!("distill schedules never mix day-of-month and weekday"),
    }
}

fn local_slot<Tz>(timezone: &Tz, date: chrono::NaiveDate, cadence: ScheduleCadence) -> DateTime<Tz>
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    timezone
        .with_ymd_and_hms(
            date.year(),
            date.month(),
            date.day(),
            cadence.hour,
            cadence.minute,
            0,
        )
        .single()
        .expect("distill schedule should map to one local wall-clock time")
}

fn launchd_weekday_to_chrono(weekday: u32) -> Weekday {
    match weekday {
        0 | 7 => Weekday::Sun,
        1 => Weekday::Mon,
        2 => Weekday::Tue,
        3 => Weekday::Wed,
        4 => Weekday::Thu,
        5 => Weekday::Fri,
        6 => Weekday::Sat,
        _ => panic!("unsupported launchd weekday: {weekday}"),
    }
}

fn days_since_weekday(current: Weekday, target: Weekday) -> i64 {
    let current = current.num_days_from_monday() as i64;
    let target = target.num_days_from_monday() as i64;
    (7 + current - target) % 7
}

fn month_day(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .expect("distill schedule should use valid month/day combinations")
}

fn previous_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// Pure formatting function — takes pre-collected data and returns the full
/// status string.  Keeping this separate from I/O makes it trivially testable.
pub fn format_status(info: &StatusInfo) -> String {
    let mut out = String::new();

    out.push_str("=== distill status ===\n");
    out.push('\n');
    out.push_str(&format!("Scan interval:  {}\n", info.config.scan_interval));
    out.push_str(&format!("Proposal agent: {}\n", info.config.proposal_agent));
    out.push_str(&format!("Shell:          {}\n", info.config.shell));
    out.push_str(&format!("Notifications:  {}\n", info.config.notifications));
    out.push_str(&format!(
        "Notification icon: {}\n",
        info.config.notification_icon.as_deref().unwrap_or("none")
    ));
    out.push('\n');

    out.push_str("Monitored agents:\n");
    for agent in &info.config.agents {
        let status = if agent.enabled { "enabled" } else { "disabled" };
        out.push_str(&format!("  - {} ({})\n", agent.name, status));
    }
    out.push('\n');

    out.push_str(&format!("Pending proposals: {}\n", info.pending_proposals));
    out.push_str(&format!("Existing skills:   {}\n", info.existing_skills));
    out.push_str(&format!(
        "Pending scan backlog: {}\n",
        info.pending_scan_backlog
    ));

    let last_scan_display = info.last_scan.as_deref().unwrap_or("never");
    out.push_str(&format!("Last scan:         {last_scan_display}\n"));
    let next_scan_display = info.next_scheduled_scan.as_deref().unwrap_or("unknown");
    out.push_str(&format!("Next scheduled scan: {next_scan_display}\n"));

    let scheduler_state = if info.scheduler_installed {
        "installed"
    } else {
        "not installed (run `distill watch --install` or re-run `distill`)"
    };
    out.push_str(&format!(
        "Scheduler:         {scheduler_state} ({})\n",
        info.scheduler_path.display()
    ));

    out
}

/// Collect runtime status data (proposal count, skill count, last-scan
/// timestamp) from the given `base_dir` rather than the hard-coded default
/// `~/.distill`.  This makes the function fully testable with `tempfile`.
pub fn collect_status_info(config: &Config, base_dir: &Path) -> Result<StatusInfo> {
    let scheduler = crate::schedule::create_scheduler_default();
    collect_status_info_with_shared_dir_at(
        config,
        base_dir,
        &Config::shared_skills_dir(),
        Utc::now(),
        scheduler.as_ref(),
    )
}

#[cfg(test)]
fn collect_status_info_with_shared_dir(
    config: &Config,
    base_dir: &Path,
    shared_skills_dir: &Path,
) -> Result<StatusInfo> {
    let scheduler = crate::schedule::create_scheduler_for_tests(base_dir.to_path_buf());
    collect_status_info_with_shared_dir_at(
        config,
        base_dir,
        shared_skills_dir,
        Utc::now(),
        scheduler.as_ref(),
    )
}

fn collect_status_info_with_shared_dir_at(
    config: &Config,
    base_dir: &Path,
    shared_skills_dir: &Path,
    now_utc: DateTime<Utc>,
    scheduler: &dyn crate::schedule::Scheduler,
) -> Result<StatusInfo> {
    let proposals_dir = base_dir.join("proposals");
    let pending_proposals = if proposals_dir.exists() {
        std::fs::read_dir(&proposals_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .count()
    } else {
        0
    };

    let existing_skills = crate::sync::load_skills_from_dirs(&[
        base_dir.join("skills"),
        shared_skills_dir.to_path_buf(),
    ])?
    .len();

    let pending_scan_backlog = load_pending_scan_backlog(&base_dir.join("scan-backlog.json"))?;

    let last_scan_path = base_dir.join("last-scan.json");
    let last_scan_data = LastScan::load(&last_scan_path)?;
    let (last_scan, next_scheduled_scan) = if let Some(last_scan_data) = last_scan_data {
        let last_scan = format_utc_timestamp(last_scan_data.timestamp);
        let next_scan = format_scheduled_scan_timing(scheduled_scan_timing_in_timezone(
            &config.scan_interval,
            last_scan_data.timestamp,
            now_utc,
            &Local,
        ));
        (Some(last_scan), Some(next_scan))
    } else {
        (None, None)
    };

    let scheduler_path = scheduler.plist_or_unit_path();
    let scheduler_installed = matches!(scheduler.status()?, SchedulerStatus::Installed);

    Ok(StatusInfo {
        config: config.clone(),
        pending_proposals,
        existing_skills,
        pending_scan_backlog,
        last_scan,
        next_scheduled_scan,
        scheduler_installed,
        scheduler_path,
    })
}

fn load_pending_scan_backlog(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }

    let contents = std::fs::read_to_string(path)?;
    let backlog: StoredBacklog = serde_json::from_str(&contents)?;
    Ok(backlog.sessions.len())
}

/// Entry point called by `main`.  Delegates to `collect_status_info` and
/// `format_status` so all logic is covered by unit tests.
pub fn run() -> Result<()> {
    if !Config::exists() {
        println!("distill is not configured. Run 'distill' to start onboarding.");
        return Ok(());
    }

    let config = Config::load()?;
    let base_dir = Config::base_dir();
    let info = collect_status_info(&config, &base_dir)?;
    print!("{}", format_status(&info));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentEntry, Config, Interval, NotificationPref, ShellType, SyncAgentsConfig,
    };
    use chrono::{FixedOffset, TimeZone};

    // ── helpers ──────────────────────────────────────────────────────────────

    fn default_info() -> StatusInfo {
        StatusInfo {
            config: Config::default(),
            pending_proposals: 0,
            existing_skills: 0,
            pending_scan_backlog: 0,
            last_scan: None,
            next_scheduled_scan: None,
            scheduler_installed: false,
            scheduler_path: PathBuf::from("/tmp/distill-test-scheduler"),
        }
    }

    fn utc_timestamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
    }

    // ── format_status tests ───────────────────────────────────────────────────

    #[test]
    fn test_format_status_default_config() {
        let output = format_status(&default_info());

        assert!(output.contains("=== distill status ==="));
        // Display formatting: "weekly" not "Weekly"
        assert!(output.contains("Scan interval:  weekly"));
        assert!(output.contains("Proposal agent: claude"));
        assert!(output.contains("Shell:          zsh"));
        assert!(output.contains("Notifications:  both"));
    }

    #[test]
    fn test_format_status_shows_all_agents() {
        let output = format_status(&default_info());

        // Default config lists every supported agent.
        assert!(output.contains("  - claude (enabled)"));
        assert!(output.contains("  - codex (enabled)"));
        assert!(output.contains("  - opencode (enabled)"));
    }

    #[test]
    fn test_format_status_shows_proposal_count() {
        let mut info = default_info();
        info.pending_proposals = 7;

        let output = format_status(&info);

        assert!(output.contains("Pending proposals: 7"));
    }

    #[test]
    fn test_format_status_shows_skill_count() {
        let mut info = default_info();
        info.existing_skills = 4;

        let output = format_status(&info);

        assert!(output.contains("Existing skills:   4"));
    }

    #[test]
    fn test_format_status_shows_backlog_count() {
        let mut info = default_info();
        info.pending_scan_backlog = 12;

        let output = format_status(&info);

        assert!(output.contains("Pending scan backlog: 12"));
    }

    #[test]
    fn test_format_status_never_scanned() {
        // last_scan is None → should print "never"
        let output = format_status(&default_info());

        assert!(output.contains("Last scan:         never"));
        assert!(output.contains("Next scheduled scan: unknown"));
    }

    #[test]
    fn test_format_status_with_last_scan() {
        let mut info = default_info();
        info.last_scan = Some("2024-11-20T08:15:00Z".to_string());
        info.next_scheduled_scan = Some("2024-11-27T08:15:00Z".to_string());

        let output = format_status(&info);

        assert!(output.contains("Last scan:         2024-11-20T08:15:00Z"));
        assert!(output.contains("Next scheduled scan: 2024-11-27T08:15:00Z"));
        // Must not also say "never"
        assert!(!output.contains("never"));
    }

    #[test]
    fn test_format_status_reports_scheduler_state_so_users_can_verify_distill_runs() {
        let mut info = default_info();
        info.scheduler_installed = false;
        info.scheduler_path = PathBuf::from("/tmp/distill-test/com.distill.agent.plist");
        let output = format_status(&info);
        assert!(
            output.contains("Scheduler:"),
            "status output must surface scheduler state so users can confirm distill is wired up"
        );
        assert!(
            output.contains("not installed"),
            "missing scheduler must read as not installed instead of being silently absent"
        );
        assert!(
            output.contains("/tmp/distill-test/com.distill.agent.plist"),
            "status output must point at the scheduler artifact path for debugging"
        );

        info.scheduler_installed = true;
        let output = format_status(&info);
        assert!(
            output.contains("installed"),
            "installed scheduler must be reported as installed"
        );
    }

    /// Disabled agents must be shown as "(disabled)" in the output.
    #[test]
    fn test_format_status_disabled_agent() {
        let mut info = default_info();
        info.config.agents = vec![
            AgentEntry {
                name: "claude".into(),
                enabled: true,
            },
            AgentEntry {
                name: "codex".into(),
                enabled: false,
            },
            AgentEntry {
                name: "opencode".into(),
                enabled: true,
            },
        ];

        let output = format_status(&info);

        assert!(output.contains("  - claude (enabled)"));
        assert!(output.contains("  - codex (disabled)"));
        assert!(output.contains("  - opencode (enabled)"));
    }

    /// Verify that all supported interval / shell / notification variants
    /// produce lowercase Display strings (not Debug variants).
    #[test]
    fn test_format_status_display_formatting_not_debug() {
        let mut info = default_info();
        info.config.scan_interval = Interval::Monthly;
        info.config.shell = ShellType::Fish;
        info.config.notifications = NotificationPref::Native;

        let output = format_status(&info);

        // Display strings
        assert!(output.contains("Scan interval:  monthly"));
        assert!(output.contains("Shell:          fish"));
        assert!(output.contains("Notifications:  native"));
        // Debug strings must not appear
        assert!(!output.contains("Monthly"));
        assert!(!output.contains("Fish"));
        assert!(!output.contains("Native"));
    }

    // ── collect_status_info tests ─────────────────────────────────────────────

    #[test]
    fn test_collect_status_info_counts_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // proposals: two .md files, one .txt that must be ignored
        let proposals = base.join("proposals");
        std::fs::create_dir_all(&proposals).unwrap();
        std::fs::write(proposals.join("p1.md"), "proposal 1").unwrap();
        std::fs::write(proposals.join("p2.md"), "proposal 2").unwrap();
        std::fs::write(proposals.join("notes.txt"), "ignored").unwrap();

        // skills: three .md files
        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("s1.md"), "skill 1").unwrap();
        std::fs::write(skills.join("s2.md"), "skill 2").unwrap();
        std::fs::write(skills.join("s3.md"), "skill 3").unwrap();

        // last-scan.json
        std::fs::write(
            base.join("last-scan.json"),
            r#"{"timestamp":"2024-06-01T12:00:00Z","session_ids":[]}"#,
        )
        .unwrap();

        let config = Config::default();
        let scheduler = crate::schedule::create_scheduler_for_tests(base.to_path_buf());
        let info = collect_status_info_with_shared_dir_at(
            &config,
            base,
            &dir.path().join("shared"),
            utc_timestamp(2024, 6, 3, 8, 0),
            scheduler.as_ref(),
        )
        .unwrap();

        assert_eq!(info.pending_proposals, 2);
        assert_eq!(info.existing_skills, 3);
        assert_eq!(info.last_scan.as_deref(), Some("2024-06-01T12:00:00Z"));
        assert!(info.next_scheduled_scan.is_some());
    }

    #[test]
    fn test_collect_status_info_counts_shared_skill_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let shared = dir.path().join("shared");

        std::fs::create_dir_all(shared.join("review")).unwrap();
        std::fs::write(
            shared.join("review").join("SKILL.md"),
            "# Review\nShared skill",
        )
        .unwrap();

        let config = Config::default();
        let info = collect_status_info_with_shared_dir(&config, base, &shared).unwrap();

        assert_eq!(info.existing_skills, 1);
    }

    #[test]
    fn test_collect_status_info_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Directories exist but contain no files
        std::fs::create_dir_all(base.join("proposals")).unwrap();
        std::fs::create_dir_all(base.join("skills")).unwrap();
        // No last-scan.json

        let config = Config::default();
        let info =
            collect_status_info_with_shared_dir(&config, base, &dir.path().join("shared")).unwrap();

        assert_eq!(info.pending_proposals, 0);
        assert_eq!(info.existing_skills, 0);
        assert_eq!(info.last_scan, None);
        assert_eq!(info.next_scheduled_scan, None);
    }

    /// When the proposals and skills directories don't exist at all (fresh
    /// install before `ensure_dirs` has run) the counts should be zero.
    #[test]
    fn test_collect_status_info_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Deliberately do NOT create proposals/ or skills/

        let config = Config::default();
        let info =
            collect_status_info_with_shared_dir(&config, base, &dir.path().join("shared")).unwrap();

        assert_eq!(info.pending_proposals, 0);
        assert_eq!(info.existing_skills, 0);
        assert_eq!(info.last_scan, None);
        assert_eq!(info.next_scheduled_scan, None);
    }

    /// `collect_status_info` must preserve the config it was given.
    #[test]
    fn test_collect_status_info_preserves_config() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let config = Config {
            scan_interval: Interval::Daily,
            proposal_agent: "codex".into(),
            shell: ShellType::Bash,
            notifications: NotificationPref::Terminal,
            notification_icon: Some("/tmp/distill-icon.png".into()),
            agents: vec![AgentEntry {
                name: "codex".into(),
                enabled: true,
            }],
            sync_agents: SyncAgentsConfig::default(),
        };

        let info =
            collect_status_info_with_shared_dir(&config, base, &dir.path().join("shared")).unwrap();

        assert_eq!(info.config.scan_interval, Interval::Daily);
        assert_eq!(info.config.proposal_agent, "codex");
        assert_eq!(info.config.shell, ShellType::Bash);
        assert_eq!(info.config.notifications, NotificationPref::Terminal);
        assert_eq!(
            info.config.notification_icon.as_deref(),
            Some("/tmp/distill-icon.png")
        );
    }

    /// Non-.md files in proposals dir must not be counted.
    #[test]
    fn test_collect_status_info_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let proposals = base.join("proposals");
        std::fs::create_dir_all(&proposals).unwrap();
        std::fs::write(proposals.join("real.md"), "actual proposal").unwrap();
        std::fs::write(proposals.join("draft.yaml"), "not a proposal").unwrap();
        std::fs::write(proposals.join("readme.txt"), "ignore me").unwrap();

        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("real.md"), "actual skill").unwrap();
        std::fs::write(skills.join("meta.json"), "not a skill").unwrap();

        let config = Config::default();
        let info =
            collect_status_info_with_shared_dir(&config, base, &dir.path().join("shared")).unwrap();

        assert_eq!(info.pending_proposals, 1);
        assert_eq!(info.existing_skills, 1);
    }

    /// Parse the structured last-scan payload and expose only the timestamp.
    #[test]
    fn test_collect_status_info_parses_last_scan_timestamp_field() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        std::fs::write(
            base.join("last-scan.json"),
            r#"
{
  "timestamp": "2025-01-10T09:00:00Z",
  "session_ids": ["s-1", "s-2"]
}
"#,
        )
        .unwrap();

        let config = Config::default();
        let scheduler = crate::schedule::create_scheduler_for_tests(base.to_path_buf());
        let info = collect_status_info_with_shared_dir_at(
            &config,
            base,
            &dir.path().join("shared"),
            utc_timestamp(2025, 1, 11, 12, 0),
            scheduler.as_ref(),
        )
        .unwrap();

        assert_eq!(info.last_scan.as_deref(), Some("2025-01-10T09:00:00Z"));
        assert!(info.next_scheduled_scan.is_some());
    }

    #[test]
    fn test_scheduled_scan_timing_returns_next_weekly_slot_after_recent_scan() {
        let timezone = FixedOffset::east_opt(0).unwrap();
        let timing = scheduled_scan_timing_in_timezone(
            &Interval::Weekly,
            utc_timestamp(2025, 1, 6, 10, 0),
            utc_timestamp(2025, 1, 8, 12, 0),
            &timezone,
        );

        assert_eq!(
            timing,
            ScheduledScanTiming::Next(utc_timestamp(2025, 1, 13, 9, 0))
        );
    }

    #[test]
    fn test_scheduled_scan_timing_marks_missed_daily_slot_overdue() {
        let timezone = FixedOffset::east_opt(2 * 3600).unwrap();
        let timing = scheduled_scan_timing_in_timezone(
            &Interval::Daily,
            utc_timestamp(2026, 3, 13, 8, 0),
            utc_timestamp(2026, 3, 14, 12, 0),
            &timezone,
        );

        assert_eq!(
            timing,
            ScheduledScanTiming::Overdue(utc_timestamp(2026, 3, 14, 7, 0))
        );
    }

    #[test]
    fn test_scheduled_scan_timing_returns_next_monthly_slot_after_catch_up() {
        let timezone = FixedOffset::east_opt(0).unwrap();
        let timing = scheduled_scan_timing_in_timezone(
            &Interval::Monthly,
            utc_timestamp(2025, 1, 1, 10, 0),
            utc_timestamp(2025, 1, 10, 12, 0),
            &timezone,
        );

        assert_eq!(
            timing,
            ScheduledScanTiming::Next(utc_timestamp(2025, 2, 1, 9, 0))
        );
    }

    #[test]
    fn test_collect_status_info_reports_overdue_schedule_state_for_missed_run() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        std::fs::write(
            base.join("last-scan.json"),
            r#"{"timestamp":"2020-01-10T09:00:00Z","session_ids":[]}"#,
        )
        .unwrap();

        let config = Config::default();
        let scheduler = crate::schedule::create_scheduler_for_tests(base.to_path_buf());
        let info = collect_status_info_with_shared_dir_at(
            &config,
            base,
            &dir.path().join("shared"),
            utc_timestamp(2026, 3, 15, 12, 31),
            scheduler.as_ref(),
        )
        .unwrap();

        assert_eq!(info.last_scan.as_deref(), Some("2020-01-10T09:00:00Z"));
        assert!(
            info.next_scheduled_scan
                .as_deref()
                .is_some_and(|value| value.starts_with("overdue since ")),
            "missed scheduled runs should be reported as overdue instead of only showing last_scan + interval"
        );
    }
}
