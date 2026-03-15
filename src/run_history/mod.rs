use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_ERROR_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunCommandKind {
    Scan,
    ScheduledRun,
    SyncAgents,
}

impl RunCommandKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::ScheduledRun => "scheduled-run",
            Self::SyncAgents => "sync-agents",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunTrigger {
    Manual,
    Scheduled,
    Onboarding,
}

impl RunTrigger {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Onboarding => "onboarding",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposals_written: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposals_skipped_pending: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog_remaining: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batches_run: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_evaluated: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_updated: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_unchanged: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_skipped: Option<usize>,
}

impl RunMetrics {
    pub fn is_empty(&self) -> bool {
        self.proposals_written.is_none()
            && self.proposals_skipped_pending.is_none()
            && self.backlog_remaining.is_none()
            && self.batches_run.is_none()
            && self.projects_evaluated.is_none()
            && self.projects_updated.is_none()
            && self.projects_unchanged.is_none()
            && self.projects_skipped.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunStageRecord {
    pub name: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_artifact_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "RunMetrics::is_empty")]
    pub metrics: RunMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub command: RunCommandKind,
    pub trigger: RunTrigger,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_artifact_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "RunMetrics::is_empty")]
    pub metrics: RunMetrics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<RunStageRecord>,
}

impl RunRecord {
    pub fn succeeded(
        command: RunCommandKind,
        trigger: RunTrigger,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        summary: Option<String>,
        metrics: RunMetrics,
        stages: Vec<RunStageRecord>,
    ) -> Self {
        Self::completed(
            command,
            trigger,
            started_at,
            finished_at,
            true,
            summary,
            None,
            None,
            metrics,
            stages,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn failed(
        command: RunCommandKind,
        trigger: RunTrigger,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        summary: Option<String>,
        error: impl Into<String>,
        debug_artifact_path: Option<PathBuf>,
        metrics: RunMetrics,
        stages: Vec<RunStageRecord>,
    ) -> Self {
        Self::completed(
            command,
            trigger,
            started_at,
            finished_at,
            false,
            summary,
            Some(truncate_text(error.into(), MAX_ERROR_CHARS)),
            debug_artifact_path,
            metrics,
            stages,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn completed(
        command: RunCommandKind,
        trigger: RunTrigger,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        success: bool,
        summary: Option<String>,
        error: Option<String>,
        debug_artifact_path: Option<PathBuf>,
        metrics: RunMetrics,
        stages: Vec<RunStageRecord>,
    ) -> Self {
        let duration_ms = finished_at
            .signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64;
        Self {
            command,
            trigger,
            started_at,
            finished_at,
            duration_ms,
            success,
            summary,
            error,
            debug_artifact_path,
            metrics,
            stages,
        }
    }

    pub fn status_label(&self) -> &'static str {
        if self.success { "success" } else { "failure" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunHistorySummary {
    pub total_runs: usize,
    pub successful_runs: usize,
    pub failed_runs: usize,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
}

pub fn append_run_record(history_dir: &Path, record: &RunRecord) -> Result<()> {
    fs::create_dir_all(history_dir).with_context(|| {
        format!(
            "Failed to create history directory: {}",
            history_dir.display()
        )
    })?;

    let path = history_dir.join("runs.jsonl");
    let line = serde_json::to_string(record).context("Failed to serialize run history entry")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn append_run_record_best_effort(history_dir: &Path, record: &RunRecord) {
    if let Err(err) = append_run_record(history_dir, record) {
        eprintln!("Warning: failed to write run history: {err}");
    }
}

pub fn load_run_records(history_dir: &Path) -> Result<Vec<RunRecord>> {
    let path = history_dir.join("runs.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut records = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: RunRecord = serde_json::from_str(trimmed).with_context(|| {
            format!(
                "Failed to parse {} line {} as a run history entry",
                path.display(),
                index + 1
            )
        })?;
        records.push(record);
    }

    records.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.finished_at.cmp(&left.finished_at))
    });
    Ok(records)
}

pub fn summarize_run_records(records: &[RunRecord]) -> RunHistorySummary {
    let mut summary = RunHistorySummary {
        total_runs: records.len(),
        ..RunHistorySummary::default()
    };

    for record in records {
        if record.success {
            summary.successful_runs += 1;
            if summary.last_success_at.is_none() {
                summary.last_success_at = Some(record.finished_at);
            }
        } else {
            summary.failed_runs += 1;
            if summary.last_failure_at.is_none() {
                summary.last_failure_at = Some(record.finished_at);
            }
        }
    }

    summary
}

pub fn stage_record(
    name: impl Into<String>,
    success: bool,
    summary: Option<String>,
    error: Option<String>,
    debug_artifact_path: Option<PathBuf>,
    metrics: RunMetrics,
) -> RunStageRecord {
    RunStageRecord {
        name: name.into(),
        success,
        summary,
        error: error.map(|value| truncate_text(value, MAX_ERROR_CHARS)),
        debug_artifact_path,
        metrics,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunsFilter {
    All,
    Success,
    Failure,
}

impl RunsFilter {
    fn label(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }

    fn matches(&self, record: &RunRecord) -> bool {
        match self {
            Self::All => true,
            Self::Success => record.success,
            Self::Failure => !record.success,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Success,
            Self::Success => Self::Failure,
            Self::Failure => Self::All,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::All => Self::Failure,
            Self::Success => Self::All,
            Self::Failure => Self::Success,
        }
    }
}

struct RunHistoryUiState {
    records: Vec<RunRecord>,
    filtered_indices: Vec<usize>,
    selected: usize,
    filter: RunsFilter,
    detail_scroll: u16,
}

impl RunHistoryUiState {
    fn new(records: Vec<RunRecord>) -> Self {
        let mut state = Self {
            records,
            filtered_indices: vec![],
            selected: 0,
            filter: RunsFilter::All,
            detail_scroll: 0,
        };
        state.refresh_filtered_indices();
        state
    }

    fn refresh_filtered_indices(&mut self) {
        self.filtered_indices = self
            .records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| self.filter.matches(record).then_some(index))
            .collect();

        if self.filtered_indices.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.filtered_indices.len() - 1);
        }
        self.detail_scroll = 0;
    }

    fn set_filter(&mut self, filter: RunsFilter) {
        if self.filter != filter {
            self.filter = filter;
            self.refresh_filtered_indices();
        }
    }

    fn next_filter(&mut self) {
        self.set_filter(self.filter.next());
    }

    fn prev_filter(&mut self) {
        self.set_filter(self.filter.prev());
    }

    fn filtered_len(&self) -> usize {
        self.filtered_indices.len()
    }

    fn selected_record(&self) -> Option<&RunRecord> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|index| self.records.get(*index))
    }

    fn next_record(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.filtered_indices.len() - 1);
        self.detail_scroll = 0;
    }

    fn previous_record(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.detail_scroll = 0;
    }

    fn first_record(&mut self) {
        self.selected = 0;
        self.detail_scroll = 0;
    }

    fn last_record(&mut self) {
        if !self.filtered_indices.is_empty() {
            self.selected = self.filtered_indices.len() - 1;
            self.detail_scroll = 0;
        }
    }

    fn scroll_details_down(&mut self, lines: u16) {
        self.detail_scroll = self.detail_scroll.saturating_add(lines);
    }

    fn scroll_details_up(&mut self, lines: u16) {
        self.detail_scroll = self.detail_scroll.saturating_sub(lines);
    }

    fn flow_index(&self) -> usize {
        if self.filter != RunsFilter::All {
            2
        } else if self.detail_scroll > 0 {
            1
        } else {
            0
        }
    }
}

struct TuiSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    active: bool,
}

impl TuiSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, cursor::Hide)
            .context("Failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).context("Failed to initialize terminal")?;
        terminal.clear().context("Failed to clear terminal")?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn draw(&mut self, state: &RunHistoryUiState) -> Result<()> {
        self.terminal
            .draw(|frame| draw_run_history_ui(frame, state))
            .context("Failed to render runs UI")?;
        Ok(())
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(
                self.terminal.backend_mut(),
                LeaveAlternateScreen,
                cursor::Show
            );
            self.active = false;
        }
    }
}

pub fn run_history_interactive(records: Vec<RunRecord>) -> Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        anyhow::bail!("distill runs requires an interactive terminal for the TUI");
    }

    let mut state = RunHistoryUiState::new(records);
    let mut tui = TuiSession::enter()?;

    loop {
        tui.draw(&state)?;

        if !event::poll(Duration::from_millis(250)).context("Failed to poll terminal events")? {
            continue;
        }

        let Event::Key(key) = event::read().context("Failed to read terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => state.previous_record(),
            KeyCode::Down | KeyCode::Char('j') => state.next_record(),
            KeyCode::Home => state.first_record(),
            KeyCode::End => state.last_record(),
            KeyCode::PageUp => state.scroll_details_up(8),
            KeyCode::PageDown => state.scroll_details_down(8),
            KeyCode::Left => state.prev_filter(),
            KeyCode::Right => state.next_filter(),
            KeyCode::Char('a') | KeyCode::Char('1') => state.set_filter(RunsFilter::All),
            KeyCode::Char('s') | KeyCode::Char('2') => state.set_filter(RunsFilter::Success),
            KeyCode::Char('f') | KeyCode::Char('3') => state.set_filter(RunsFilter::Failure),
            _ => {}
        }
    }

    Ok(())
}

fn draw_run_history_ui(frame: &mut Frame<'_>, state: &RunHistoryUiState) {
    let accent = Color::Cyan;
    let muted = Color::DarkGray;
    let emphasis = Color::Yellow;
    let summary = summarize_run_records(&state.records);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
        .split(frame.area());

    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            "DISTILL RUNS   Previous scan and sync outcomes",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "Recorded: {} | Success: {} | Failure: {} | Showing: {} ({})",
            summary.total_runs,
            summary.successful_runs,
            summary.failed_runs,
            state.filtered_len(),
            state.filter.label()
        )),
        Line::from(format!(
            "Last success: {} | Last failure: {}",
            format_optional_timestamp(summary.last_success_at),
            format_optional_timestamp(summary.last_failure_at)
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(muted))
            .title("OVERVIEW"),
    );
    frame.render_widget(header, chunks[0]);

    let flow = Tabs::new(vec![
        Line::from(format!("1. Runs ({})", state.filtered_len())),
        Line::from("2. Inspect"),
        Line::from(format!("3. Filter ({})", state.filter.label())),
    ])
    .select(state.flow_index())
    .divider(" | ")
    .style(Style::default().fg(muted))
    .highlight_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(muted))
            .title("FLOW"),
    );
    frame.render_widget(flow, chunks[1]);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(chunks[2]);

    let items = state
        .filtered_indices
        .iter()
        .map(|index| ListItem::new(run_list_label(&state.records[*index])))
        .collect::<Vec<_>>();
    let runs = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(muted))
                .title("RUNS"),
        )
        .highlight_symbol("> ")
        .highlight_style(Style::default().fg(accent).add_modifier(Modifier::BOLD));
    let mut list_state = ListState::default();
    if !state.filtered_indices.is_empty() {
        list_state.select(Some(state.selected));
    }
    frame.render_stateful_widget(runs, body_chunks[0], &mut list_state);

    let details = state
        .selected_record()
        .map(format_run_details)
        .unwrap_or_else(|| "No runs match current filter.".to_string());
    let detail_pane = Paragraph::new(details)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(muted))
                .title("INSPECT"),
        )
        .wrap(Wrap { trim: false })
        .scroll((state.detail_scroll, 0));
    frame.render_widget(detail_pane, body_chunks[1]);

    let filter_chip = |filter: RunsFilter| -> Span<'static> {
        let marker = if state.filter == filter { "[x]" } else { "[ ]" };
        let label = format!("{marker} {}", filter.label());
        if state.filter == filter {
            Span::styled(
                label,
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, Style::default().fg(muted))
        }
    };

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("[Up/Down] ", Style::default().fg(accent)),
            Span::raw("Select run  "),
            Span::styled("[PageUp/PageDown] ", Style::default().fg(Color::Green)),
            Span::raw("Scroll details"),
        ]),
        Line::from(vec![
            Span::styled("[Left/Right] ", Style::default().fg(accent)),
            Span::raw("Change filter  "),
            Span::styled("[a/s/f] ", Style::default().fg(emphasis)),
            Span::raw("Quick filters  "),
            Span::styled("[q] ", Style::default().fg(Color::Red)),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            filter_chip(RunsFilter::All),
            Span::raw("  "),
            filter_chip(RunsFilter::Success),
            Span::raw("  "),
            filter_chip(RunsFilter::Failure),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(muted))
            .title("ACTIONS"),
    );
    frame.render_widget(footer, chunks[3]);
}

fn run_list_label(record: &RunRecord) -> String {
    let note = record
        .summary
        .as_deref()
        .or(record.error.as_deref())
        .unwrap_or("");
    format!(
        "{}  {:<14}  {:<7}  {}",
        format_timestamp(record.started_at),
        record.command.label(),
        if record.success { "ok" } else { "failed" },
        truncate_text(note.to_string(), 36)
    )
}

fn format_run_details(record: &RunRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("Command:      {}\n", record.command.label()));
    out.push_str(&format!("Trigger:      {}\n", record.trigger.label()));
    out.push_str(&format!("Status:       {}\n", record.status_label()));
    out.push_str(&format!(
        "Started:      {}\n",
        record.started_at.to_rfc3339()
    ));
    out.push_str(&format!(
        "Finished:     {}\n",
        record.finished_at.to_rfc3339()
    ));
    out.push_str(&format!("Duration:     {} ms\n", record.duration_ms));

    if let Some(summary) = &record.summary {
        out.push_str(&format!("Summary:      {summary}\n"));
    }
    if let Some(error) = &record.error {
        out.push_str(&format!("Error:        {error}\n"));
    }
    if let Some(path) = &record.debug_artifact_path {
        out.push_str(&format!("Debug path:   {}\n", path.display()));
    }

    append_metrics_lines(&mut out, &record.metrics, "");

    if !record.stages.is_empty() {
        out.push_str("\nStages:\n");
        for stage in &record.stages {
            out.push_str(&format!(
                "- {} [{}]\n",
                stage.name,
                if stage.success { "ok" } else { "failed" }
            ));
            if let Some(summary) = &stage.summary {
                out.push_str(&format!("  Summary: {summary}\n"));
            }
            if let Some(error) = &stage.error {
                out.push_str(&format!("  Error: {error}\n"));
            }
            if let Some(path) = &stage.debug_artifact_path {
                out.push_str(&format!("  Debug path: {}\n", path.display()));
            }
            append_metrics_lines(&mut out, &stage.metrics, "  ");
        }
    }

    out
}

fn append_metrics_lines(out: &mut String, metrics: &RunMetrics, indent: &str) {
    let rows = [
        ("Proposals written", metrics.proposals_written),
        ("Proposals skipped", metrics.proposals_skipped_pending),
        ("Backlog remaining", metrics.backlog_remaining),
        ("Batches run", metrics.batches_run),
        ("Projects evaluated", metrics.projects_evaluated),
        ("Projects updated", metrics.projects_updated),
        ("Projects unchanged", metrics.projects_unchanged),
        ("Projects skipped", metrics.projects_skipped),
    ];

    for (label, value) in rows {
        if let Some(value) = value {
            out.push_str(&format!("{indent}{label}: {value}\n"));
        }
    }
}

fn format_optional_timestamp(value: Option<DateTime<Utc>>) -> String {
    value
        .map(format_timestamp)
        .unwrap_or_else(|| "never".to_string())
}

fn format_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%SZ").to_string()
}

fn truncate_text(input: String, max_chars: usize) -> String {
    let trimmed = input.trim();
    let mut output = String::new();
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ratatui::{Terminal, backend::TestBackend};

    fn timestamp(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 15, hour, 0, 0)
            .single()
            .unwrap()
    }

    fn render_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let area = *buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn test_append_and_load_run_records_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let record = RunRecord::succeeded(
            RunCommandKind::Scan,
            RunTrigger::Manual,
            timestamp(9),
            timestamp(10),
            Some("Scan completed successfully.".to_string()),
            RunMetrics {
                proposals_written: Some(2),
                backlog_remaining: Some(0),
                ..RunMetrics::default()
            },
            vec![stage_record(
                "scan",
                true,
                Some("Drained backlog".to_string()),
                None,
                None,
                RunMetrics {
                    proposals_written: Some(2),
                    backlog_remaining: Some(0),
                    ..RunMetrics::default()
                },
            )],
        );

        append_run_record(dir.path(), &record).unwrap();
        let loaded = load_run_records(dir.path()).unwrap();

        assert_eq!(loaded, vec![record]);
    }

    #[test]
    fn test_load_run_records_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let older = RunRecord::succeeded(
            RunCommandKind::Scan,
            RunTrigger::Scheduled,
            timestamp(8),
            timestamp(9),
            None,
            RunMetrics::default(),
            vec![],
        );
        let newer = RunRecord::failed(
            RunCommandKind::SyncAgents,
            RunTrigger::Manual,
            timestamp(10),
            timestamp(11),
            Some("sync-agents failed".to_string()),
            "boom",
            Some(PathBuf::from("/tmp/debug")),
            RunMetrics::default(),
            vec![],
        );

        append_run_record(dir.path(), &older).unwrap();
        append_run_record(dir.path(), &newer).unwrap();

        let loaded = load_run_records(dir.path()).unwrap();
        assert_eq!(loaded[0], newer);
        assert_eq!(loaded[1], older);
    }

    #[test]
    fn test_load_run_records_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_run_records(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_summarize_run_records_counts_successes_and_failures() {
        let success = RunRecord::succeeded(
            RunCommandKind::Scan,
            RunTrigger::Manual,
            timestamp(8),
            timestamp(9),
            None,
            RunMetrics::default(),
            vec![],
        );
        let failure = RunRecord::failed(
            RunCommandKind::ScheduledRun,
            RunTrigger::Scheduled,
            timestamp(10),
            timestamp(11),
            None,
            "failed",
            None,
            RunMetrics::default(),
            vec![],
        );

        let summary = summarize_run_records(&[failure.clone(), success.clone()]);
        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.successful_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.last_success_at, Some(success.finished_at));
        assert_eq!(summary.last_failure_at, Some(failure.finished_at));
    }

    #[test]
    fn test_failed_record_truncates_error_text() {
        let error = "x".repeat(MAX_ERROR_CHARS + 50);
        let record = RunRecord::failed(
            RunCommandKind::Scan,
            RunTrigger::Manual,
            timestamp(8),
            timestamp(9),
            None,
            error,
            None,
            RunMetrics::default(),
            vec![],
        );

        assert_eq!(
            record.error.as_ref().unwrap().chars().count(),
            MAX_ERROR_CHARS
        );
    }

    #[test]
    fn test_run_history_ui_filter_cycles() {
        let mut state = RunHistoryUiState::new(vec![]);
        assert_eq!(state.filter, RunsFilter::All);

        state.next_filter();
        assert_eq!(state.filter, RunsFilter::Success);

        state.next_filter();
        assert_eq!(state.filter, RunsFilter::Failure);

        state.prev_filter();
        assert_eq!(state.filter, RunsFilter::Success);
    }

    #[test]
    fn test_draw_run_history_ui_renders_summary_and_details() {
        let records = vec![
            RunRecord::failed(
                RunCommandKind::ScheduledRun,
                RunTrigger::Scheduled,
                timestamp(10),
                timestamp(11),
                Some("scheduled-run failed".to_string()),
                "sync failed",
                Some(PathBuf::from("/tmp/distill/scan-debug/last-failed")),
                RunMetrics {
                    proposals_written: Some(2),
                    batches_run: Some(1),
                    ..RunMetrics::default()
                },
                vec![stage_record(
                    "sync-agents",
                    false,
                    Some("Git evidence failed".to_string()),
                    Some("agent invocation failed".to_string()),
                    None,
                    RunMetrics {
                        projects_evaluated: Some(1),
                        projects_skipped: Some(1),
                        ..RunMetrics::default()
                    },
                )],
            ),
            RunRecord::succeeded(
                RunCommandKind::Scan,
                RunTrigger::Manual,
                timestamp(8),
                timestamp(9),
                Some("Scan completed successfully.".to_string()),
                RunMetrics {
                    proposals_written: Some(3),
                    backlog_remaining: Some(0),
                    ..RunMetrics::default()
                },
                vec![],
            ),
        ];
        let state = RunHistoryUiState::new(records);
        let backend = TestBackend::new(140, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_run_history_ui(frame, &state))
            .unwrap();

        let rendered = render_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("DISTILL RUNS"));
        assert!(rendered.contains("Recorded: 2 | Success: 1 | Failure: 1"));
        assert!(rendered.contains("scheduled-run"));
        assert!(rendered.contains("sync-agents"));
        assert!(rendered.contains("Debug path:   /tmp/distill/scan-debug/last-failed"));
    }

    #[test]
    fn test_draw_run_history_ui_renders_empty_filter_state() {
        let records = vec![RunRecord::succeeded(
            RunCommandKind::Scan,
            RunTrigger::Manual,
            timestamp(8),
            timestamp(9),
            Some("Scan completed successfully.".to_string()),
            RunMetrics::default(),
            vec![],
        )];
        let mut state = RunHistoryUiState::new(records);
        state.set_filter(RunsFilter::Failure);

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_run_history_ui(frame, &state))
            .unwrap();

        let rendered = render_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("No runs match current filter."));
        assert!(rendered.contains("[x] failure"));
    }
}
