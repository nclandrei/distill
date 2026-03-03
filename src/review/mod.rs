// Review flow — interactive proposal review via a keyboard-driven TUI.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::agents::{from_kind, AgentKind};
use crate::config::Config;
use crate::proposals::{Proposal, ProposalType};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A user's decision for a single proposal.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum ReviewDecision {
    Accept,
    Reject,
    Skip,
}

/// A record of a proposal decision written to the history log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub proposal_filename: String,
    pub decision: String, // "accepted" or "rejected"
    pub decided_at: DateTime<Utc>,
}

/// Summary of a completed review session.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewSummary {
    pub accepted: usize,
    pub rejected: usize,
    pub skipped: usize,
}

// ---------------------------------------------------------------------------
// Core logic (path-based, fully testable without stdin)
// ---------------------------------------------------------------------------

/// Load all `.md` files from `proposals_dir`, parsing each as a `Proposal`.
/// Sets the `filename` field on each parsed proposal.
/// Returns an empty `Vec` (no error) if the directory does not exist.
pub fn load_proposals(proposals_dir: &Path) -> Result<Vec<Proposal>> {
    if !proposals_dir.exists() {
        return Ok(vec![]);
    }

    let mut proposals = Vec::new();

    for entry in fs::read_dir(proposals_dir)?.flatten() {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown.md")
            .to_string();

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read proposal: {}", path.display()))?;

        match Proposal::from_markdown(&content) {
            Ok(mut proposal) => {
                proposal.filename = Some(filename);
                proposals.push(proposal);
            }
            Err(e) => {
                // Skip malformed proposals with a warning rather than aborting.
                eprintln!("Warning: skipping malformed proposal {filename}: {e}");
            }
        }
    }

    proposals.sort_by(|a, b| {
        let a_name = a.filename.as_deref().unwrap_or("");
        let b_name = b.filename.as_deref().unwrap_or("");
        a_name.cmp(b_name)
    });

    Ok(proposals)
}

/// Append a JSON line to `history_dir/decisions.jsonl`.
/// Creates the file and parent directories if they do not yet exist.
pub fn log_decision(history_dir: &Path, entry: &HistoryEntry) -> Result<()> {
    fs::create_dir_all(history_dir).with_context(|| {
        format!(
            "Failed to create history directory: {}",
            history_dir.display()
        )
    })?;

    let decisions_path = history_dir.join("decisions.jsonl");
    let line = serde_json::to_string(entry).context("Failed to serialize history entry")?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&decisions_path)
        .with_context(|| format!("Failed to open {}", decisions_path.display()))?;

    writeln!(file, "{}", line)
        .with_context(|| format!("Failed to write to {}", decisions_path.display()))?;

    Ok(())
}

/// Determine the skill filename for a proposal.
///
/// Priority:
/// 1. `target_skill` frontmatter field — normalised to a slug, with `.md` appended.
/// 2. The proposal's own filename (kept as-is, since it already ends with `.md`).
/// 3. A timestamp-based fallback.
fn normalize_target_skill_filename(target: &str) -> String {
    let slug = target.trim().to_lowercase().replace([' ', '_'], "-");
    if slug.ends_with(".md") {
        slug
    } else {
        format!("{slug}.md")
    }
}

fn skill_filename_for(proposal: &Proposal) -> String {
    if let Some(target) = &proposal.frontmatter.target_skill {
        normalize_target_skill_filename(target)
    } else if let Some(filename) = &proposal.filename {
        filename.clone()
    } else {
        format!("skill-{}.md", Utc::now().timestamp())
    }
}

/// Accept a proposal: write its body as a skill, log the decision, and delete the proposal file.
pub fn accept_proposal(
    proposal: &Proposal,
    skills_dir: &Path,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<()> {
    let proposal_filename = proposal
        .filename
        .clone()
        .unwrap_or_else(|| skill_filename_for(proposal));

    match proposal.frontmatter.proposal_type {
        ProposalType::Remove => {
            let target_skill = proposal
                .frontmatter
                .target_skill
                .as_deref()
                .context("Remove proposal is missing target_skill")?;
            let skill_file = normalize_target_skill_filename(target_skill);
            let skill_path = skills_dir.join(&skill_file);
            if skill_path.exists() {
                fs::remove_file(&skill_path)
                    .with_context(|| format!("Failed to remove skill {}", skill_path.display()))?;
            }
        }
        ProposalType::New | ProposalType::Improve | ProposalType::Edit => {
            let skill_file = skill_filename_for(proposal);
            fs::create_dir_all(skills_dir).with_context(|| {
                format!(
                    "Failed to create skills directory: {}",
                    skills_dir.display()
                )
            })?;
            let skill_path = skills_dir.join(&skill_file);
            fs::write(&skill_path, &proposal.body)
                .with_context(|| format!("Failed to write skill to {}", skill_path.display()))?;
        }
    }

    // Log the decision.
    let entry = HistoryEntry {
        proposal_filename: proposal_filename.clone(),
        decision: "accepted".to_string(),
        decided_at: Utc::now(),
    };
    log_decision(history_dir, &entry)?;

    // Delete the proposal file.
    let proposal_path = proposals_dir.join(&proposal_filename);
    if proposal_path.exists() {
        fs::remove_file(&proposal_path)
            .with_context(|| format!("Failed to delete proposal: {}", proposal_path.display()))?;
    }

    Ok(())
}

/// Reject a proposal: log the decision and delete the proposal file.
pub fn reject_proposal(
    proposal: &Proposal,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<()> {
    let proposal_filename = proposal
        .filename
        .clone()
        .unwrap_or_else(|| format!("unknown-{}.md", Utc::now().timestamp()));

    // Log the decision.
    let entry = HistoryEntry {
        proposal_filename: proposal_filename.clone(),
        decision: "rejected".to_string(),
        decided_at: Utc::now(),
    };
    log_decision(history_dir, &entry)?;

    // Delete the proposal file.
    let proposal_path = proposals_dir.join(&proposal_filename);
    if proposal_path.exists() {
        fs::remove_file(&proposal_path)
            .with_context(|| format!("Failed to delete proposal: {}", proposal_path.display()))?;
    }

    Ok(())
}

/// Process a slice of proposals with pre-determined decisions.
///
/// This is the core testable logic — no stdin required.
/// Use `run_review_interactive` for the user-facing flow.
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_review(
    proposals: &[Proposal],
    decisions: &[ReviewDecision],
    skills_dir: &Path,
    history_dir: &Path,
    proposals_dir: &Path,
) -> Result<ReviewSummary> {
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut skipped = 0usize;

    for (proposal, decision) in proposals.iter().zip(decisions.iter()) {
        match decision {
            ReviewDecision::Accept => {
                accept_proposal(proposal, skills_dir, history_dir, proposals_dir)?;
                accepted += 1;
            }
            ReviewDecision::Reject => {
                reject_proposal(proposal, history_dir, proposals_dir)?;
                rejected += 1;
            }
            ReviewDecision::Skip => {
                skipped += 1;
            }
        }
    }

    Ok(ReviewSummary {
        accepted,
        rejected,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Interactive TUI helpers
// ---------------------------------------------------------------------------

struct ReviewUiState {
    pending: Vec<Proposal>,
    selected: usize,
    content_scroll: u16,
    accepted: usize,
    rejected: usize,
    skipped: usize,
    status_line: String,
    confirmation: Option<PendingConfirmation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiIntent {
    MoveUp,
    MoveDown,
    ScrollUp,
    ScrollDown,
    ScrollHome,
    Accept,
    Reject,
    Snooze,
    Edit,
    AcceptAll,
    Quit,
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingConfirmation {
    AcceptRemove { proposal_filename: String },
    AcceptAllWithRemovals { remove_count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfirmationResolution {
    Proceed { clear_existing: bool },
    Await(PendingConfirmation),
}

impl ReviewUiState {
    fn new(proposals: Vec<Proposal>) -> Self {
        Self {
            pending: proposals,
            selected: 0,
            content_scroll: 0,
            accepted: 0,
            rejected: 0,
            skipped: 0,
            status_line: "Select a proposal and choose an action.".to_string(),
            confirmation: None,
        }
    }

    fn selected_proposal(&self) -> Option<&Proposal> {
        self.pending.get(self.selected)
    }

    fn select_prev(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.content_scroll = 0;
    }

    fn select_next(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let max_idx = self.pending.len().saturating_sub(1);
        self.selected = (self.selected + 1).min(max_idx);
        self.content_scroll = 0;
    }

    fn remove_selected(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.pending.remove(self.selected);
        if self.selected >= self.pending.len() && !self.pending.is_empty() {
            self.selected = self.pending.len() - 1;
        }
        self.content_scroll = 0;
    }

    fn clear_confirmation(&mut self) {
        self.confirmation = None;
    }
}

fn intent_from_key(code: KeyCode) -> UiIntent {
    match code {
        KeyCode::Up | KeyCode::Char('k') => UiIntent::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => UiIntent::MoveDown,
        KeyCode::PageUp => UiIntent::ScrollUp,
        KeyCode::PageDown => UiIntent::ScrollDown,
        KeyCode::Home => UiIntent::ScrollHome,
        KeyCode::Char('a') => UiIntent::Accept,
        KeyCode::Char('r') => UiIntent::Reject,
        KeyCode::Char('s') => UiIntent::Snooze,
        KeyCode::Char('e') => UiIntent::Edit,
        KeyCode::Char('A') => UiIntent::AcceptAll,
        KeyCode::Char('q') | KeyCode::Esc => UiIntent::Quit,
        _ => UiIntent::Noop,
    }
}

fn required_confirmation_for_intent(
    intent: UiIntent,
    selected_proposal: Option<&Proposal>,
    pending: &[Proposal],
) -> Option<PendingConfirmation> {
    match intent {
        UiIntent::Accept => {
            let proposal = selected_proposal?;
            if proposal.frontmatter.proposal_type == ProposalType::Remove {
                let name = proposal.filename.as_deref().unwrap_or("(unknown)");
                Some(PendingConfirmation::AcceptRemove {
                    proposal_filename: name.to_string(),
                })
            } else {
                None
            }
        }
        UiIntent::AcceptAll => {
            let remove_count = pending
                .iter()
                .filter(|proposal| proposal.frontmatter.proposal_type == ProposalType::Remove)
                .count();
            if remove_count > 0 {
                Some(PendingConfirmation::AcceptAllWithRemovals { remove_count })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn resolve_confirmation(
    current: Option<&PendingConfirmation>,
    required: Option<PendingConfirmation>,
) -> ConfirmationResolution {
    match required {
        Some(required) if current == Some(&required) => ConfirmationResolution::Proceed {
            clear_existing: true,
        },
        Some(required) => ConfirmationResolution::Await(required),
        None => ConfirmationResolution::Proceed {
            clear_existing: current.is_some(),
        },
    }
}

fn confirmation_prompt(confirmation: &PendingConfirmation) -> String {
    match confirmation {
        PendingConfirmation::AcceptRemove { proposal_filename } => {
            format!("Confirm remove accept for {proposal_filename}: press 'a' again.")
        }
        PendingConfirmation::AcceptAllWithRemovals { remove_count } => format!(
            "Accept-all includes {remove_count} remove proposal(s). Press 'A' again to confirm."
        ),
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

    fn draw(&mut self, state: &ReviewUiState, skills_dir: &Path) -> Result<()> {
        self.terminal
            .draw(|frame| draw_review_ui(frame, state, skills_dir))
            .context("Failed to render review UI")?;
        Ok(())
    }

    fn suspend(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        disable_raw_mode().context("Failed to disable raw mode")?;
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            cursor::Show
        )
        .context("Failed to suspend TUI")?;
        self.active = false;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.active {
            return Ok(());
        }
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            cursor::Hide
        )
        .context("Failed to resume TUI")?;
        enable_raw_mode().context("Failed to re-enable raw mode")?;
        self.terminal.clear().context("Failed to clear terminal")?;
        self.active = true;
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

fn proposal_label(proposal: &Proposal) -> String {
    use crate::proposals::ProposalType;

    let filename = proposal.filename.as_deref().unwrap_or("(unknown)");
    let kind = match proposal.frontmatter.proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    };
    format!("{filename} [{kind}]")
}

fn proposal_details_text(proposal: &Proposal, skills_dir: &Path) -> String {
    use crate::proposals::{Confidence, ProposalType};

    let filename = proposal.filename.as_deref().unwrap_or("(unknown)");
    let proposal_type = match proposal.frontmatter.proposal_type {
        ProposalType::New => "new",
        ProposalType::Improve => "improve",
        ProposalType::Edit => "edit",
        ProposalType::Remove => "remove",
    };
    let confidence = match proposal.frontmatter.confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    };
    let target = proposal
        .frontmatter
        .target_skill
        .as_deref()
        .unwrap_or("(new skill)");

    let mut text = format!(
        "File: {filename}\nType: {proposal_type}\nConfidence: {confidence}\nTarget: {target}\nCreated: {}\n",
        proposal.frontmatter.created.to_rfc3339(),
    );

    if !proposal.frontmatter.evidence.is_empty() {
        text.push_str("\nEvidence:\n");
        for ev in &proposal.frontmatter.evidence {
            text.push_str(&format!("- {} ({})\n", ev.pattern, ev.session));
        }
    }

    if proposal.frontmatter.proposal_type == ProposalType::Remove {
        text.push_str("\nWarning:\n");
        if let Some(target_skill) = proposal.frontmatter.target_skill.as_deref() {
            let skill_file = normalize_target_skill_filename(target_skill);
            let skill_path = skills_dir.join(&skill_file);
            if !skill_path.exists() {
                text.push_str(&format!(
                    "- Target skill file not found: {}\n",
                    skill_path.display()
                ));
            } else {
                text.push_str("- This will permanently delete the target skill file.\n");
            }
        } else {
            text.push_str("- Remove proposal is missing target_skill and will fail.\n");
        }
    }

    text.push_str("\n--- Content ---\n");
    text.push_str(&proposal.body);
    text
}

fn draw_review_ui(frame: &mut Frame<'_>, state: &ReviewUiState, skills_dir: &Path) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(chunks[1]);

    let header = Paragraph::new(format!(
        "Pending: {} | Accepted: {} | Rejected: {} | Snoozed: {}",
        state.pending.len(),
        state.accepted,
        state.rejected,
        state.skipped,
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("distill review"),
    );
    frame.render_widget(header, chunks[0]);

    let items: Vec<ListItem<'_>> = state
        .pending
        .iter()
        .enumerate()
        .map(|(idx, proposal)| {
            ListItem::new(format!("{:>2}. {}", idx + 1, proposal_label(proposal)))
        })
        .collect();
    let proposals = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Proposals"))
        .highlight_symbol("> ")
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    let mut list_state = ListState::default();
    if !state.pending.is_empty() {
        list_state.select(Some(state.selected));
    }
    frame.render_stateful_widget(proposals, body_chunks[0], &mut list_state);

    let details = state
        .selected_proposal()
        .map(|proposal| proposal_details_text(proposal, skills_dir))
        .unwrap_or_else(|| "No pending proposals.".to_string());
    let detail_pane = Paragraph::new(details)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: false })
        .scroll((state.content_scroll, 0));
    frame.render_widget(detail_pane, body_chunks[1]);

    let footer = Paragraph::new(vec![
        Line::from(
            "a accept | r reject | e edit | s snooze | A accept-all | q quit | arrows move | PgUp/PgDn scroll",
        ),
        Line::from(state.status_line.clone()),
    ])
    .block(Block::default().borders(Borders::ALL).title("Actions"));
    frame.render_widget(footer, chunks[2]);

    if let Some(confirmation) = state.confirmation.as_ref() {
        let modal = centered_rect(60, 22, frame.area());
        let text = vec![
            Line::from("Confirm Action"),
            Line::from(""),
            Line::from(confirmation_prompt(confirmation)),
            Line::from(""),
            Line::from("Press the same action key again to confirm."),
            Line::from("Any other action key cancels this confirmation."),
        ];
        let widget = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Confirmation"))
            .wrap(Wrap { trim: true });
        frame.render_widget(Clear, modal);
        frame.render_widget(widget, modal);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn shell_quote_single(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

fn edit_file(path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let escaped = shell_quote_single(&path.to_string_lossy());
    let command = format!("{editor} {escaped}");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .status()
        .with_context(|| format!("Failed to launch editor command: {command}"))?;
    if !status.success() {
        anyhow::bail!("Editor exited with status: {status}");
    }
    Ok(())
}

fn reload_edited_proposal(path: &Path, filename: &str) -> Result<Proposal> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read edited proposal: {}", path.display()))?;
    let mut proposal = Proposal::from_markdown(&content)
        .with_context(|| format!("Failed to parse edited proposal: {}", path.display()))?;
    proposal.filename = Some(filename.to_string());
    Ok(proposal)
}

/// Load config and sync all skills in `skills_dir` to all enabled agents.
fn sync_after_review(skills_dir: &Path) -> Result<crate::sync::SyncReport> {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Load config to discover enabled agents; fall back to the default if config is absent.
    let config = Config::load().unwrap_or_default();

    let agents: Vec<Box<dyn crate::agents::Agent>> = config
        .agents
        .iter()
        .filter(|a| a.enabled)
        .filter_map(|a| match a.name.as_str() {
            "claude" => Some(from_kind(AgentKind::Claude, home.clone())),
            "codex" => Some(from_kind(AgentKind::Codex, home.clone())),
            _ => None,
        })
        .collect();

    crate::sync::run_sync(skills_dir, &agents)
}

/// Run the full interactive review flow.
///
/// Shows a TUI with a proposal list and details pane.
/// Actions:
/// - `a`: accept selected proposal
/// - `r`: reject selected proposal
/// - `e`: edit selected proposal in `$VISUAL`/`$EDITOR`
/// - `s`: snooze selected proposal (skip this run)
/// - `A`: accept all remaining proposals
pub fn run_review_interactive(
    proposals_dir: &Path,
    skills_dir: &Path,
    history_dir: &Path,
) -> Result<ReviewSummary> {
    let proposals = load_proposals(proposals_dir)?;

    if proposals.is_empty() {
        println!("No pending proposals to review.");
        return Ok(ReviewSummary {
            accepted: 0,
            rejected: 0,
            skipped: 0,
        });
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!("distill review requires an interactive terminal for the TUI");
    }

    let mut state = ReviewUiState::new(proposals);
    let mut tui = TuiSession::enter()?;

    while !state.pending.is_empty() {
        tui.draw(&state, skills_dir)?;

        if !event::poll(Duration::from_millis(200)).context("Failed to poll terminal events")? {
            continue;
        }

        let Event::Key(key) = event::read().context("Failed to read terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let intent = intent_from_key(key.code);
        if intent == UiIntent::Noop {
            continue;
        }

        let required =
            required_confirmation_for_intent(intent, state.selected_proposal(), &state.pending);
        match resolve_confirmation(state.confirmation.as_ref(), required) {
            ConfirmationResolution::Await(confirmation) => {
                state.status_line = confirmation_prompt(&confirmation);
                state.confirmation = Some(confirmation);
                continue;
            }
            ConfirmationResolution::Proceed { clear_existing } => {
                if clear_existing {
                    state.clear_confirmation();
                    if !matches!(intent, UiIntent::Accept | UiIntent::AcceptAll) {
                        state.status_line = "Confirmation cancelled.".to_string();
                    }
                }
            }
        }

        match intent {
            UiIntent::MoveUp => state.select_prev(),
            UiIntent::MoveDown => state.select_next(),
            UiIntent::ScrollUp => state.content_scroll = state.content_scroll.saturating_sub(5),
            UiIntent::ScrollDown => state.content_scroll = state.content_scroll.saturating_add(5),
            UiIntent::ScrollHome => state.content_scroll = 0,
            UiIntent::Accept => {
                if let Some(proposal) = state.selected_proposal().cloned() {
                    let name = proposal
                        .filename
                        .as_deref()
                        .unwrap_or("(unknown)")
                        .to_string();
                    match accept_proposal(&proposal, skills_dir, history_dir, proposals_dir) {
                        Ok(_) => {
                            state.accepted += 1;
                            state.remove_selected();
                            state.status_line = format!("Accepted {name}");
                        }
                        Err(e) => {
                            state.status_line = format!("Failed to accept {name}: {e:#}");
                        }
                    }
                }
            }
            UiIntent::Reject => {
                if let Some(proposal) = state.selected_proposal().cloned() {
                    let name = proposal
                        .filename
                        .as_deref()
                        .unwrap_or("(unknown)")
                        .to_string();
                    match reject_proposal(&proposal, history_dir, proposals_dir) {
                        Ok(_) => {
                            state.rejected += 1;
                            state.remove_selected();
                            state.status_line = format!("Rejected {name}");
                        }
                        Err(e) => {
                            state.status_line = format!("Failed to reject {name}: {e:#}");
                        }
                    }
                }
            }
            UiIntent::Snooze => {
                if let Some(proposal) = state.selected_proposal() {
                    let name = proposal
                        .filename
                        .as_deref()
                        .unwrap_or("(unknown)")
                        .to_string();
                    state.skipped += 1;
                    state.remove_selected();
                    state.status_line = format!("Snoozed {name}");
                }
            }
            UiIntent::Edit => {
                let Some(proposal) = state.selected_proposal().cloned() else {
                    continue;
                };
                let Some(filename) = proposal.filename.clone() else {
                    state.status_line = "Cannot edit proposal without a filename.".to_string();
                    continue;
                };
                let path = proposals_dir.join(&filename);
                if !path.exists() {
                    state.status_line = format!("Proposal file not found: {}", path.display());
                    continue;
                }

                tui.suspend()?;
                let edit_result = edit_file(&path);
                let resume_result = tui.resume();
                if let Err(e) = resume_result {
                    return Err(e).context("Failed to restore terminal after editing");
                }

                match edit_result {
                    Ok(_) => match reload_edited_proposal(&path, &filename) {
                        Ok(updated) => {
                            state.pending[state.selected] = updated;
                            state.content_scroll = 0;
                            state.status_line = format!("Edited {filename}");
                        }
                        Err(e) => {
                            state.status_line =
                                format!("Edited file but failed to parse {filename}: {e:#}");
                        }
                    },
                    Err(e) => {
                        state.status_line = format!("Edit failed for {filename}: {e:#}");
                    }
                }
            }
            UiIntent::AcceptAll => {
                let total = state.pending.len();
                let mut failed = Vec::new();
                let mut accepted_now = 0usize;

                for proposal in state.pending.drain(..) {
                    match accept_proposal(&proposal, skills_dir, history_dir, proposals_dir) {
                        Ok(_) => {
                            state.accepted += 1;
                            accepted_now += 1;
                        }
                        Err(_) => failed.push(proposal),
                    }
                }

                state.pending = failed;
                state.selected = 0;
                state.content_scroll = 0;

                if state.pending.is_empty() {
                    state.status_line =
                        format!("Accepted all remaining proposals ({accepted_now}).");
                } else {
                    state.status_line = format!(
                        "Accepted {accepted_now}/{total}. {} proposal(s) still pending due to errors.",
                        state.pending.len()
                    );
                }
            }
            UiIntent::Quit => {
                let remaining = state.pending.len();
                state.skipped += remaining;
                state.pending.clear();
                state.status_line =
                    format!("Exited review. Snoozed {remaining} remaining proposal(s).");
            }
            UiIntent::Noop => {}
        }
    }

    drop(tui);

    println!();
    println!("Review complete.");
    println!("  Accepted : {}", state.accepted);
    println!("  Rejected : {}", state.rejected);
    println!("  Skipped  : {}", state.skipped);

    // Sync accepted skills to all configured agents.
    if state.accepted > 0 {
        println!();
        println!("Syncing skills to agents...");
        match sync_after_review(skills_dir) {
            Ok(report) => {
                println!(
                    "  Synced {} operation(s) across agents ({} skipped).",
                    report.synced, report.skipped,
                );
                if !report.errors.is_empty() {
                    for err in &report.errors {
                        eprintln!("  Sync warning: {err}");
                    }
                }
            }
            Err(e) => {
                eprintln!("  Sync failed (skills saved, but not propagated to agents): {e}");
            }
        }
    }

    Ok(ReviewSummary {
        accepted: state.accepted,
        rejected: state.rejected,
        skipped: state.skipped,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposals::{Confidence, Evidence, ProposalFrontmatter, ProposalType};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal valid `Proposal` for testing.
    fn make_proposal(filename: &str, target_skill: Option<&str>, body: &str) -> Proposal {
        Proposal {
            frontmatter: ProposalFrontmatter {
                proposal_type: ProposalType::New,
                confidence: Confidence::High,
                target_skill: target_skill.map(|s| s.to_string()),
                evidence: vec![Evidence {
                    session: "test-session.jsonl".into(),
                    pattern: "test pattern".into(),
                }],
                created: Utc::now(),
            },
            body: body.to_string(),
            filename: Some(filename.to_string()),
        }
    }

    /// Write a `Proposal` as a `.md` file inside `dir`, using its `filename` field.
    fn write_proposal_file(dir: &Path, proposal: &Proposal) {
        let md = proposal.to_markdown().unwrap();
        let fname = proposal.filename.as_deref().unwrap();
        fs::write(dir.join(fname), md).unwrap();
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

    // -----------------------------------------------------------------------
    // load_proposals
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_proposals_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let proposal = make_proposal("git-workflow.md", None, "# Git Workflow\nAlways rebase.");
        write_proposal_file(proposals_dir, &proposal);

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].filename, Some("git-workflow.md".to_string()));
        assert!(proposals[0].body.contains("Git Workflow"));
    }

    #[test]
    fn test_load_proposals_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let proposals = load_proposals(dir.path()).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_load_proposals_nonexistent_dir() {
        let proposals = load_proposals(Path::new("/nonexistent/proposals/dir")).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_load_proposals_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let proposal = make_proposal("real.md", None, "# Real\nContent.");
        write_proposal_file(proposals_dir, &proposal);
        fs::write(proposals_dir.join("notes.txt"), "not a proposal").unwrap();
        fs::write(proposals_dir.join("data.json"), "{}").unwrap();

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].filename, Some("real.md".to_string()));
    }

    #[test]
    fn test_load_proposals_sets_filename_field() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let p1 = make_proposal("alpha.md", None, "# Alpha\nContent.");
        let p2 = make_proposal("beta.md", None, "# Beta\nContent.");
        write_proposal_file(proposals_dir, &p1);
        write_proposal_file(proposals_dir, &p2);

        let proposals = load_proposals(proposals_dir).unwrap();
        assert_eq!(proposals.len(), 2);

        let filenames: Vec<&str> = proposals
            .iter()
            .map(|p| p.filename.as_deref().unwrap())
            .collect();
        assert!(filenames.contains(&"alpha.md"));
        assert!(filenames.contains(&"beta.md"));
    }

    #[test]
    fn test_load_proposals_sorted_by_filename() {
        let dir = tempfile::tempdir().unwrap();
        let proposals_dir = dir.path();

        let p1 = make_proposal("zeta.md", None, "# Zeta\nContent.");
        let p2 = make_proposal("alpha.md", None, "# Alpha\nContent.");
        let p3 = make_proposal("mid.md", None, "# Mid\nContent.");
        write_proposal_file(proposals_dir, &p1);
        write_proposal_file(proposals_dir, &p2);
        write_proposal_file(proposals_dir, &p3);

        let proposals = load_proposals(proposals_dir).unwrap();
        let filenames: Vec<&str> = proposals
            .iter()
            .map(|p| p.filename.as_deref().unwrap())
            .collect();

        assert_eq!(filenames, vec!["alpha.md", "mid.md", "zeta.md"]);
    }

    // -----------------------------------------------------------------------
    // accept_proposal
    // -----------------------------------------------------------------------

    #[test]
    fn test_accept_proposal_writes_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("new-skill.md", None, "# New Skill\nDo the thing.");
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let skill_path = skills_dir.join("new-skill.md");
        assert!(skill_path.exists(), "skill file should be created");
        let content = fs::read_to_string(&skill_path).unwrap();
        assert!(content.contains("New Skill"));
        assert!(content.contains("Do the thing."));
    }

    #[test]
    fn test_accept_proposal_removes_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("delete-me.md", None, "# Delete Me\nContent.");
        let proposal_path = proposals_dir.join("delete-me.md");
        write_proposal_file(&proposals_dir, &proposal);

        assert!(
            proposal_path.exists(),
            "proposal file should exist before accept"
        );
        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();
        assert!(
            !proposal_path.exists(),
            "proposal file should be deleted after accept"
        );
    }

    #[test]
    fn test_accept_proposal_logs_to_history() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("logged.md", None, "# Logged\nContent.");
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let decisions_path = history_dir.join("decisions.jsonl");
        assert!(decisions_path.exists(), "decisions.jsonl should be created");
        let content = fs::read_to_string(&decisions_path).unwrap();
        assert!(
            content.contains("\"accepted\""),
            "should log accepted decision"
        );
        assert!(
            content.contains("logged.md"),
            "should include proposal filename"
        );
    }

    #[test]
    fn test_accept_proposal_uses_target_skill_filename() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        // target_skill set: skill should be written to that name, not the proposal filename.
        let proposal = make_proposal(
            "improve-123.md",
            Some("git-workflow"),
            "# Git Workflow\nUpdated content.",
        );
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        let skill_path = skills_dir.join("git-workflow.md");
        assert!(
            skill_path.exists(),
            "skill should be written to the target_skill path"
        );
        let other_path = skills_dir.join("improve-123.md");
        assert!(
            !other_path.exists(),
            "should NOT write to the proposal filename when target_skill is set"
        );
    }

    #[test]
    fn test_accept_remove_proposal_deletes_target_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(&proposals_dir).unwrap();

        let target_skill = skills_dir.join("stale-skill.md");
        fs::write(&target_skill, "# Stale\nOld content.").unwrap();

        let mut proposal = make_proposal(
            "remove-123.md",
            Some("stale-skill"),
            "# Remove stale skill\nNo longer needed.",
        );
        proposal.frontmatter.proposal_type = ProposalType::Remove;
        let proposal_path = proposals_dir.join("remove-123.md");
        write_proposal_file(&proposals_dir, &proposal);

        accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir).unwrap();

        assert!(!target_skill.exists(), "remove should delete target skill");
        assert!(
            !proposal_path.exists(),
            "accepted remove proposal should be deleted"
        );
        assert!(
            !skills_dir.join("remove-123.md").exists(),
            "remove should not write proposal body as a new skill file"
        );
    }

    #[test]
    fn test_accept_remove_proposal_requires_target_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let mut proposal = make_proposal("remove-missing-target.md", None, "# Missing target");
        proposal.frontmatter.proposal_type = ProposalType::Remove;
        let proposal_path = proposals_dir.join("remove-missing-target.md");
        write_proposal_file(&proposals_dir, &proposal);

        let result = accept_proposal(&proposal, &skills_dir, &history_dir, &proposals_dir);
        assert!(result.is_err(), "remove without target_skill should fail");
        assert!(
            proposal_path.exists(),
            "failed accept should keep proposal file for later review"
        );
        assert!(
            !history_dir.join("decisions.jsonl").exists(),
            "failed accept should not log an accepted decision"
        );
    }

    // -----------------------------------------------------------------------
    // UI rendering
    // -----------------------------------------------------------------------

    #[test]
    fn test_proposal_details_warns_when_remove_target_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut proposal = make_proposal("remove.md", Some("missing-skill"), "# Remove");
        proposal.frontmatter.proposal_type = ProposalType::Remove;

        let details = proposal_details_text(&proposal, dir.path());
        assert!(
            details.contains("Warning:"),
            "remove proposals should include a warning section"
        );
        assert!(
            details.contains("Target skill file not found"),
            "remove proposals with missing targets should show an explicit warning"
        );
    }

    #[test]
    fn test_draw_review_ui_renders_footer_actions_and_status() {
        use ratatui::{backend::TestBackend, Terminal};

        let proposal = make_proposal("alpha.md", None, "# Alpha");
        let mut state = ReviewUiState::new(vec![proposal]);
        state.status_line = "Accepted alpha.md".to_string();

        let skills_dir = tempfile::tempdir().unwrap();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_review_ui(frame, &state, skills_dir.path()))
            .unwrap();

        let rendered = render_buffer_text(terminal.backend().buffer());
        assert!(
            rendered.contains("a accept | r reject"),
            "actions row must render"
        );
        assert!(
            rendered.contains("Accepted alpha.md"),
            "status line should be visible in footer"
        );
    }

    #[test]
    fn test_required_confirmation_for_accept_remove() {
        let mut proposal = make_proposal("remove-me.md", Some("legacy"), "# Remove");
        proposal.frontmatter.proposal_type = ProposalType::Remove;
        let pending = vec![proposal.clone()];

        let required =
            required_confirmation_for_intent(UiIntent::Accept, Some(&proposal), &pending);
        assert_eq!(
            required,
            Some(PendingConfirmation::AcceptRemove {
                proposal_filename: "remove-me.md".to_string()
            })
        );
    }

    #[test]
    fn test_required_confirmation_for_accept_all_with_removals() {
        let mut remove = make_proposal("remove.md", Some("legacy"), "# Remove");
        remove.frontmatter.proposal_type = ProposalType::Remove;
        let keep = make_proposal("new.md", None, "# New");
        let pending = vec![remove, keep];

        let required = required_confirmation_for_intent(
            UiIntent::AcceptAll,
            pending.first(),
            pending.as_slice(),
        );
        assert_eq!(
            required,
            Some(PendingConfirmation::AcceptAllWithRemovals { remove_count: 1 })
        );
    }

    #[test]
    fn test_resolve_confirmation_needs_first_confirmation() {
        let required = Some(PendingConfirmation::AcceptRemove {
            proposal_filename: "remove.md".to_string(),
        });
        let outcome = resolve_confirmation(None, required);
        assert_eq!(
            outcome,
            ConfirmationResolution::Await(PendingConfirmation::AcceptRemove {
                proposal_filename: "remove.md".to_string()
            })
        );
    }

    #[test]
    fn test_resolve_confirmation_allows_on_second_press() {
        let pending = PendingConfirmation::AcceptAllWithRemovals { remove_count: 2 };
        let outcome = resolve_confirmation(Some(&pending), Some(pending.clone()));
        assert_eq!(
            outcome,
            ConfirmationResolution::Proceed {
                clear_existing: true
            }
        );
    }

    #[test]
    fn test_resolve_confirmation_clears_on_other_action() {
        let pending = PendingConfirmation::AcceptRemove {
            proposal_filename: "remove.md".to_string(),
        };
        let outcome = resolve_confirmation(Some(&pending), None);
        assert_eq!(
            outcome,
            ConfirmationResolution::Proceed {
                clear_existing: true
            }
        );
    }

    #[test]
    fn test_draw_review_ui_renders_confirmation_modal() {
        use ratatui::{backend::TestBackend, Terminal};

        let proposal = make_proposal("alpha.md", None, "# Alpha");
        let mut state = ReviewUiState::new(vec![proposal]);
        state.confirmation = Some(PendingConfirmation::AcceptAllWithRemovals { remove_count: 1 });

        let skills_dir = tempfile::tempdir().unwrap();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_review_ui(frame, &state, skills_dir.path()))
            .unwrap();

        let rendered = render_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Confirmation"));
        assert!(rendered.contains("Confirm Action"));
        assert!(rendered.contains("Press 'A' again"));
    }

    // -----------------------------------------------------------------------
    // reject_proposal
    // -----------------------------------------------------------------------

    #[test]
    fn test_reject_proposal_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("reject-me.md", None, "# Reject Me\nContent.");
        let proposal_path = proposals_dir.join("reject-me.md");
        write_proposal_file(&proposals_dir, &proposal);

        assert!(
            proposal_path.exists(),
            "proposal file should exist before reject"
        );
        reject_proposal(&proposal, &history_dir, &proposals_dir).unwrap();
        assert!(
            !proposal_path.exists(),
            "proposal file should be deleted after reject"
        );
    }

    #[test]
    fn test_reject_proposal_logs_to_history() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let proposal = make_proposal("reject-log.md", None, "# Reject Log\nContent.");
        write_proposal_file(&proposals_dir, &proposal);

        reject_proposal(&proposal, &history_dir, &proposals_dir).unwrap();

        let decisions_path = history_dir.join("decisions.jsonl");
        assert!(decisions_path.exists(), "decisions.jsonl should be created");
        let content = fs::read_to_string(&decisions_path).unwrap();
        assert!(
            content.contains("\"rejected\""),
            "should log rejected decision"
        );
        assert!(
            content.contains("reject-log.md"),
            "should include proposal filename"
        );
    }

    // -----------------------------------------------------------------------
    // log_decision
    // -----------------------------------------------------------------------

    #[test]
    fn test_log_decision_appends() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");

        let entry1 = HistoryEntry {
            proposal_filename: "first.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };
        let entry2 = HistoryEntry {
            proposal_filename: "second.md".to_string(),
            decision: "rejected".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry1).unwrap();
        log_decision(&history_dir, &entry2).unwrap();

        let path = history_dir.join("decisions.jsonl");
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 2, "should have exactly 2 lines");
        assert!(lines[0].contains("first.md"));
        assert!(lines[0].contains("accepted"));
        assert!(lines[1].contains("second.md"));
        assert!(lines[1].contains("rejected"));
    }

    #[test]
    fn test_log_decision_creates_history_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Deeply nested path that does not yet exist.
        let history_dir = dir.path().join("a").join("b").join("history");

        let entry = HistoryEntry {
            proposal_filename: "p.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry).unwrap();
        assert!(
            history_dir.join("decisions.jsonl").exists(),
            "decisions.jsonl should be created even in a deeply nested directory"
        );
    }

    #[test]
    fn test_log_decision_entry_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");

        let entry = HistoryEntry {
            proposal_filename: "valid-json.md".to_string(),
            decision: "accepted".to_string(),
            decided_at: Utc::now(),
        };

        log_decision(&history_dir, &entry).unwrap();

        let path = history_dir.join("decisions.jsonl");
        let content = fs::read_to_string(&path).unwrap();
        for line in content.lines() {
            let parsed: serde_json::Value =
                serde_json::from_str(line).expect("each line should be valid JSON");
            assert_eq!(
                parsed["proposal_filename"].as_str().unwrap(),
                "valid-json.md"
            );
            assert_eq!(parsed["decision"].as_str().unwrap(), "accepted");
        }
    }

    // -----------------------------------------------------------------------
    // run_review (summary counts — no stdin required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_review_summary_counts() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("p1.md", None, "# P1\nContent one.");
        let p2 = make_proposal("p2.md", None, "# P2\nContent two.");
        let p3 = make_proposal("p3.md", None, "# P3\nContent three.");

        for p in [&p1, &p2, &p3] {
            write_proposal_file(&proposals_dir, p);
        }

        let proposals = vec![p1, p2, p3];
        let decisions = vec![
            ReviewDecision::Accept,
            ReviewDecision::Reject,
            ReviewDecision::Skip,
        ];

        let summary = run_review(
            &proposals,
            &decisions,
            &skills_dir,
            &history_dir,
            &proposals_dir,
        )
        .unwrap();

        assert_eq!(summary.accepted, 1);
        assert_eq!(summary.rejected, 1);
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn test_review_all_accept() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("a1.md", None, "# A1\nContent.");
        let p2 = make_proposal("a2.md", None, "# A2\nContent.");
        write_proposal_file(&proposals_dir, &p1);
        write_proposal_file(&proposals_dir, &p2);

        let proposals = vec![p1, p2];
        let decisions = vec![ReviewDecision::Accept, ReviewDecision::Accept];

        let summary = run_review(
            &proposals,
            &decisions,
            &skills_dir,
            &history_dir,
            &proposals_dir,
        )
        .unwrap();

        assert_eq!(summary.accepted, 2);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.skipped, 0);

        // Both skill files should exist.
        assert!(skills_dir.join("a1.md").exists());
        assert!(skills_dir.join("a2.md").exists());
    }

    #[test]
    fn test_review_all_reject() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("r1.md", None, "# R1\nContent.");
        let p2 = make_proposal("r2.md", None, "# R2\nContent.");
        write_proposal_file(&proposals_dir, &p1);
        write_proposal_file(&proposals_dir, &p2);

        let proposals = vec![p1, p2];
        let decisions = vec![ReviewDecision::Reject, ReviewDecision::Reject];

        let summary = run_review(
            &proposals,
            &decisions,
            &skills_dir,
            &history_dir,
            &proposals_dir,
        )
        .unwrap();

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.rejected, 2);
        assert_eq!(summary.skipped, 0);

        // No skill files should have been created.
        assert!(!skills_dir.exists() || fs::read_dir(&skills_dir).unwrap().next().is_none());
    }

    #[test]
    fn test_review_all_skip() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let history_dir = dir.path().join("history");
        let proposals_dir = dir.path().join("proposals");
        fs::create_dir_all(&proposals_dir).unwrap();

        let p1 = make_proposal("s1.md", None, "# S1\nContent.");
        // Do not write to disk — skip should not touch the filesystem at all.

        let proposals = vec![p1];
        let decisions = vec![ReviewDecision::Skip];

        let summary = run_review(
            &proposals,
            &decisions,
            &skills_dir,
            &history_dir,
            &proposals_dir,
        )
        .unwrap();

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.skipped, 1);

        // History should be untouched.
        assert!(!history_dir.join("decisions.jsonl").exists());
    }
}
