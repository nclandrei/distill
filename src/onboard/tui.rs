// Ratatui-based interactive onboarding wizard.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table},
};
use std::io::{self, IsTerminal};

use super::{OnboardingAnswers, build_config, detect_agents};
use crate::agents::AgentKind;
use crate::config::{Config, Interval, NotificationPref, ShellType};

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    SelectAgents,
    SelectInterval,
    SelectProposal,
    SelectShell,
    SelectNotifications,
    Summary,
    Done,
    Cancelled,
}

struct WizardState {
    step: Step,
    detected_agents: Vec<(AgentKind, bool)>,
    agents_enabled: Vec<bool>,
    scan_interval_idx: usize,
    proposal_agent_idx: usize,
    shell_idx: usize,
    notification_idx: usize,
    cursor: usize,
}

const INTERVALS: &[&str] = &["daily", "weekly", "monthly"];
const SHELLS: &[&str] = &["zsh", "bash", "fish", "other"];
const NOTIFICATIONS: &[&str] = &["terminal", "native", "both", "none"];

impl WizardState {
    fn new(detected: Vec<(AgentKind, bool)>) -> Self {
        let agents_enabled: Vec<bool> = detected.iter().map(|(_, found)| *found).collect();

        // Default shell index from detection.
        let shell_idx = match ShellType::detect() {
            ShellType::Zsh => 0,
            ShellType::Bash => 1,
            ShellType::Fish => 2,
            ShellType::Other => 3,
        };

        Self {
            step: Step::Welcome,
            detected_agents: detected,
            agents_enabled,
            scan_interval_idx: 1, // weekly
            proposal_agent_idx: 0,
            shell_idx,
            notification_idx: 2, // both
            cursor: 0,
        }
    }

    /// Number of items on the current screen (for cursor clamping).
    fn item_count(&self) -> usize {
        match self.step {
            Step::SelectAgents => self.detected_agents.len(),
            Step::SelectInterval => INTERVALS.len(),
            Step::SelectProposal => self.enabled_agents().len(),
            Step::SelectShell => SHELLS.len(),
            Step::SelectNotifications => NOTIFICATIONS.len(),
            _ => 0,
        }
    }

    fn enabled_agents(&self) -> Vec<AgentKind> {
        AgentKind::all()
            .into_iter()
            .zip(self.agents_enabled.iter())
            .filter(|(_, enabled)| **enabled)
            .map(|(kind, _)| kind)
            .collect()
    }

    /// Total visible steps (Welcome counts as 1, Summary as last).
    /// When proposal step is skipped we show 6 steps, otherwise 7.
    fn total_steps(&self) -> usize {
        if self.should_skip_proposal() { 6 } else { 7 }
    }

    /// 1-based step number for display.
    fn step_number(&self) -> usize {
        match self.step {
            Step::Welcome => 1,
            Step::SelectAgents => 2,
            Step::SelectInterval => 3,
            Step::SelectProposal => 4,
            Step::SelectShell => {
                if self.should_skip_proposal() {
                    4
                } else {
                    5
                }
            }
            Step::SelectNotifications => {
                if self.should_skip_proposal() {
                    5
                } else {
                    6
                }
            }
            Step::Summary => self.total_steps(),
            Step::Done | Step::Cancelled => self.total_steps(),
        }
    }

    fn should_skip_proposal(&self) -> bool {
        self.enabled_agents().len() <= 1
    }

    fn advance(&mut self) {
        self.step = match self.step {
            Step::Welcome => Step::SelectAgents,
            Step::SelectAgents => {
                // If no agents enabled, enable all as fallback.
                if self.enabled_agents().is_empty() {
                    for v in &mut self.agents_enabled {
                        *v = true;
                    }
                }
                Step::SelectInterval
            }
            Step::SelectInterval => {
                if self.should_skip_proposal() {
                    // Auto-select the single enabled agent (or first).
                    self.proposal_agent_idx = 0;
                    Step::SelectShell
                } else {
                    Step::SelectProposal
                }
            }
            Step::SelectProposal => Step::SelectShell,
            Step::SelectShell => Step::SelectNotifications,
            Step::SelectNotifications => Step::Summary,
            Step::Summary => Step::Done,
            other => other,
        };
        self.cursor = match self.step {
            Step::SelectAgents => 0,
            Step::SelectInterval => self.scan_interval_idx,
            Step::SelectProposal => self.proposal_agent_idx,
            Step::SelectShell => self.shell_idx,
            Step::SelectNotifications => self.notification_idx,
            _ => 0,
        };
    }

    fn go_back(&mut self) {
        self.step = match self.step {
            Step::Welcome => Step::Cancelled,
            Step::SelectAgents => Step::Welcome,
            Step::SelectInterval => Step::SelectAgents,
            Step::SelectProposal => Step::SelectInterval,
            Step::SelectShell => {
                if self.should_skip_proposal() {
                    Step::SelectInterval
                } else {
                    Step::SelectProposal
                }
            }
            Step::SelectNotifications => Step::SelectShell,
            Step::Summary => Step::SelectNotifications,
            other => other,
        };
        self.cursor = match self.step {
            Step::SelectAgents => 0,
            Step::SelectInterval => self.scan_interval_idx,
            Step::SelectProposal => self.proposal_agent_idx,
            Step::SelectShell => self.shell_idx,
            Step::SelectNotifications => self.notification_idx,
            _ => 0,
        };
    }

    fn cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn cursor_down(&mut self) {
        let max = self.item_count().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// Handle space (toggle) on the SelectAgents screen.
    fn toggle_agent(&mut self) {
        if self.step == Step::SelectAgents && self.cursor < self.agents_enabled.len() {
            self.agents_enabled[self.cursor] = !self.agents_enabled[self.cursor];
        }
    }

    /// Commit cursor selection on single-select screens.
    fn commit_selection(&mut self) {
        match self.step {
            Step::SelectInterval => self.scan_interval_idx = self.cursor,
            Step::SelectProposal => self.proposal_agent_idx = self.cursor,
            Step::SelectShell => self.shell_idx = self.cursor,
            Step::SelectNotifications => self.notification_idx = self.cursor,
            _ => {}
        }
    }

    fn into_answers(self) -> OnboardingAnswers {
        let enabled = self.enabled_agents();

        let scan_interval = match self.scan_interval_idx {
            0 => Interval::Daily,
            2 => Interval::Monthly,
            _ => Interval::Weekly,
        };

        let proposal_agent = enabled
            .get(self.proposal_agent_idx)
            .copied()
            .unwrap_or(AgentKind::Claude);

        let shell = match self.shell_idx {
            0 => ShellType::Zsh,
            1 => ShellType::Bash,
            2 => ShellType::Fish,
            _ => ShellType::Other,
        };

        let notifications = match self.notification_idx {
            0 => NotificationPref::Terminal,
            1 => NotificationPref::Native,
            2 => NotificationPref::Both,
            _ => NotificationPref::None,
        };

        OnboardingAnswers {
            detected_agents: self.detected_agents,
            enabled_agents: enabled,
            scan_interval,
            proposal_agent,
            shell,
            notifications,
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(frame: &mut Frame, state: &WizardState) {
    let outer = Block::default()
        .title(" distill setup ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = outer.inner(frame.area());
    frame.render_widget(outer, frame.area());

    // Layout: header, content, footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);

    // -- Header: progress --
    let header_text = format!("Step {} of {}", state.step_number(), state.total_steps());
    let header = Paragraph::new(header_text).style(Style::default().fg(Color::Cyan));
    frame.render_widget(header, chunks[0]);

    // -- Content --
    match state.step {
        Step::Welcome => render_welcome(frame, chunks[1], state),
        Step::SelectAgents => render_select_agents(frame, chunks[1], state),
        Step::SelectInterval => render_single_select(
            frame,
            chunks[1],
            "How often should distill scan for new skills?",
            INTERVALS,
            state.cursor,
        ),
        Step::SelectProposal => {
            let labels: Vec<String> = state
                .enabled_agents()
                .iter()
                .map(|k| k.to_string())
                .collect();
            let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            render_single_select(
                frame,
                chunks[1],
                "Which agent should generate skill proposals?",
                &label_refs,
                state.cursor,
            );
        }
        Step::SelectShell => render_single_select(
            frame,
            chunks[1],
            "Confirm your shell:",
            SHELLS,
            state.cursor,
        ),
        Step::SelectNotifications => render_single_select(
            frame,
            chunks[1],
            "How would you like to receive notifications?",
            NOTIFICATIONS,
            state.cursor,
        ),
        Step::Summary => render_summary(frame, chunks[1], state),
        _ => {}
    }

    // -- Footer: key hints --
    let hints = match state.step {
        Step::Welcome => "Enter: continue  |  Esc: quit",
        Step::SelectAgents => "Up/Down: move  |  Space: toggle  |  Enter: continue  |  Esc: back",
        Step::Summary => "Enter: save  |  Esc: back",
        _ => "Up/Down: move  |  Enter: select  |  Esc: back",
    };
    let footer = Paragraph::new(hints).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, chunks[2]);
}

fn render_welcome(frame: &mut Frame, area: Rect, state: &WizardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let welcome = Paragraph::new("Welcome to distill! Let's set things up.\n\nDetected agents:");
    frame.render_widget(welcome, chunks[0]);

    let rows: Vec<Row> = state
        .detected_agents
        .iter()
        .map(|(kind, found)| {
            let status = if *found { "found" } else { "not found" };
            let style = if *found {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Row::new(vec![kind.to_string(), status.to_string()]).style(style)
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Length(12)]).header(
        Row::new(vec!["Agent", "Status"]).style(Style::default().add_modifier(Modifier::BOLD)),
    );
    frame.render_widget(table, chunks[1]);
}

fn render_select_agents(frame: &mut Frame, area: Rect, state: &WizardState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);

    let title = Paragraph::new("Select agents to monitor:");
    frame.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = AgentKind::all()
        .iter()
        .enumerate()
        .map(|(i, kind)| {
            let checked = if state.agents_enabled[i] {
                "[x]"
            } else {
                "[ ]"
            };
            let detected = state
                .detected_agents
                .iter()
                .find(|(k, _)| k == kind)
                .map(|(_, v)| *v)
                .unwrap_or(false);
            let suffix = if detected { "" } else { " (not detected)" };
            let label = format!("{checked} {kind}{suffix}");
            let style = if i == state.cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if state.agents_enabled[i] {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[1]);
}

fn render_single_select(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    options: &[&str],
    cursor: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);

    let title_widget = Paragraph::new(title);
    frame.render_widget(title_widget, chunks[0]);

    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let prefix = if i == cursor { "> " } else { "  " };
            let style = if i == cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(format!("{prefix}{label}")).style(style)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, chunks[1]);
}

fn render_summary(frame: &mut Frame, area: Rect, state: &WizardState) {
    let enabled = state.enabled_agents();
    let enabled_display = if enabled.is_empty() {
        "(none)".to_string()
    } else {
        enabled
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let interval = INTERVALS[state.scan_interval_idx];
    let proposal = enabled
        .get(state.proposal_agent_idx)
        .map(|k| k.to_string())
        .unwrap_or_else(|| "claude".to_string());
    let shell = SHELLS[state.shell_idx];
    let notif = NOTIFICATIONS[state.notification_idx];

    let rows = vec![
        Row::new(vec!["Agents monitored", &enabled_display]),
        Row::new(vec!["Scan interval", interval]),
        Row::new(vec!["Proposal agent", &proposal]),
        Row::new(vec!["Shell", shell]),
        Row::new(vec!["Notifications", notif]),
    ];

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);

    let title = Paragraph::new("Review your choices:");
    frame.render_widget(title, chunks[0]);

    let table = Table::new(rows, [Constraint::Length(20), Constraint::Min(20)]).header(
        Row::new(vec!["Setting", "Value"]).style(Style::default().add_modifier(Modifier::BOLD)),
    );
    frame.render_widget(table, chunks[1]);
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

fn run_wizard(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut WizardState,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, state))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc => state.go_back(),
                KeyCode::Enter => {
                    state.commit_selection();
                    state.advance();
                }
                KeyCode::Up | KeyCode::Char('k') => state.cursor_up(),
                KeyCode::Down | KeyCode::Char('j') => state.cursor_down(),
                KeyCode::Char(' ') => state.toggle_agent(),
                _ => {}
            }
        }

        if state.step == Step::Done || state.step == Step::Cancelled {
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run_interactive() -> Result<()> {
    // Non-TTY fallback — keeps tests passing.
    if !io::stdout().is_terminal() {
        println!("Welcome to distill! Run in an interactive terminal to complete setup.");
        return Ok(());
    }

    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    let detected = detect_agents(&home);

    // Install panic hook that restores terminal.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let mut terminal = setup_terminal()?;
    let mut state = WizardState::new(detected);

    let result = run_wizard(&mut terminal, &mut state);

    restore_terminal(&mut terminal)?;

    // Restore the default panic hook.
    let _ = std::panic::take_hook();

    result?;

    if state.step == Step::Cancelled {
        println!("Setup cancelled.");
        return Ok(());
    }

    let answers = state.into_answers();
    let config = build_config(&answers);
    config.save()?;

    println!("Configuration saved to {}", Config::config_path().display());

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (state machine only — no terminal needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn two_agent_detected() -> Vec<(AgentKind, bool)> {
        vec![(AgentKind::Claude, true), (AgentKind::Codex, true)]
    }

    fn one_agent_detected() -> Vec<(AgentKind, bool)> {
        vec![(AgentKind::Claude, true), (AgentKind::Codex, false)]
    }

    fn no_agent_detected() -> Vec<(AgentKind, bool)> {
        vec![(AgentKind::Claude, false), (AgentKind::Codex, false)]
    }

    // -- Step transitions: full flow with two agents --

    #[test]
    fn test_full_flow_two_agents() {
        let mut s = WizardState::new(two_agent_detected());
        assert_eq!(s.step, Step::Welcome);

        s.advance(); // -> SelectAgents
        assert_eq!(s.step, Step::SelectAgents);

        s.advance(); // -> SelectInterval
        assert_eq!(s.step, Step::SelectInterval);

        s.advance(); // -> SelectProposal (two agents enabled)
        assert_eq!(s.step, Step::SelectProposal);

        s.advance(); // -> SelectShell
        assert_eq!(s.step, Step::SelectShell);

        s.advance(); // -> SelectNotifications
        assert_eq!(s.step, Step::SelectNotifications);

        s.advance(); // -> Summary
        assert_eq!(s.step, Step::Summary);

        s.advance(); // -> Done
        assert_eq!(s.step, Step::Done);
    }

    // -- Step transitions: single agent skips proposal --

    #[test]
    fn test_full_flow_single_agent_skips_proposal() {
        let mut s = WizardState::new(one_agent_detected());
        assert_eq!(s.step, Step::Welcome);

        s.advance(); // -> SelectAgents
        assert_eq!(s.step, Step::SelectAgents);

        s.advance(); // -> SelectInterval
        assert_eq!(s.step, Step::SelectInterval);

        s.advance(); // -> SelectShell (skips SelectProposal)
        assert_eq!(s.step, Step::SelectShell);

        s.advance(); // -> SelectNotifications
        assert_eq!(s.step, Step::SelectNotifications);

        s.advance(); // -> Summary
        assert_eq!(s.step, Step::Summary);

        s.advance(); // -> Done
        assert_eq!(s.step, Step::Done);
    }

    // -- go_back from each step --

    #[test]
    fn test_go_back_from_welcome_cancels() {
        let mut s = WizardState::new(two_agent_detected());
        assert_eq!(s.step, Step::Welcome);
        s.go_back();
        assert_eq!(s.step, Step::Cancelled);
    }

    #[test]
    fn test_go_back_from_select_agents() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // -> SelectAgents
        s.go_back();
        assert_eq!(s.step, Step::Welcome);
    }

    #[test]
    fn test_go_back_from_select_interval() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.go_back();
        assert_eq!(s.step, Step::SelectAgents);
    }

    #[test]
    fn test_go_back_from_select_proposal() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.advance(); // SelectProposal
        assert_eq!(s.step, Step::SelectProposal);
        s.go_back();
        assert_eq!(s.step, Step::SelectInterval);
    }

    #[test]
    fn test_go_back_from_shell_skips_proposal_when_single_agent() {
        let mut s = WizardState::new(one_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.advance(); // SelectShell (proposal skipped)
        assert_eq!(s.step, Step::SelectShell);
        s.go_back();
        assert_eq!(s.step, Step::SelectInterval);
    }

    #[test]
    fn test_go_back_from_shell_goes_to_proposal_when_two_agents() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.advance(); // SelectProposal
        s.advance(); // SelectShell
        assert_eq!(s.step, Step::SelectShell);
        s.go_back();
        assert_eq!(s.step, Step::SelectProposal);
    }

    #[test]
    fn test_go_back_from_notifications() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.advance(); // SelectProposal
        s.advance(); // SelectShell
        s.advance(); // SelectNotifications
        assert_eq!(s.step, Step::SelectNotifications);
        s.go_back();
        assert_eq!(s.step, Step::SelectShell);
    }

    #[test]
    fn test_go_back_from_summary() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.advance(); // SelectProposal
        s.advance(); // SelectShell
        s.advance(); // SelectNotifications
        s.advance(); // Summary
        assert_eq!(s.step, Step::Summary);
        s.go_back();
        assert_eq!(s.step, Step::SelectNotifications);
    }

    // -- Cursor bounds --

    #[test]
    fn test_cursor_up_saturates_at_zero() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.cursor = 0;
        s.cursor_up();
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn test_cursor_down_clamps_at_max() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents (2 items)
        s.cursor = 1; // last item
        s.cursor_down();
        assert_eq!(s.cursor, 1); // clamped
    }

    #[test]
    fn test_cursor_moves_within_range() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectInterval; // 3 items
        s.cursor = 0;
        s.cursor_down();
        assert_eq!(s.cursor, 1);
        s.cursor_down();
        assert_eq!(s.cursor, 2);
        s.cursor_down();
        assert_eq!(s.cursor, 2); // clamped
        s.cursor_up();
        assert_eq!(s.cursor, 1);
    }

    // -- Toggle agent --

    #[test]
    fn test_toggle_agent() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectAgents;
        assert!(s.agents_enabled[0]); // Claude on
        s.cursor = 0;
        s.toggle_agent();
        assert!(!s.agents_enabled[0]); // toggled off
        s.toggle_agent();
        assert!(s.agents_enabled[0]); // back on
    }

    #[test]
    fn test_toggle_agent_only_on_select_agents_step() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectInterval;
        s.cursor = 0;
        let before = s.agents_enabled.clone();
        s.toggle_agent(); // should be no-op
        assert_eq!(s.agents_enabled, before);
    }

    // -- Commit selection --

    #[test]
    fn test_commit_selection_interval() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectInterval;
        s.cursor = 2; // monthly
        s.commit_selection();
        assert_eq!(s.scan_interval_idx, 2);
    }

    #[test]
    fn test_commit_selection_shell() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectShell;
        s.cursor = 1; // bash
        s.commit_selection();
        assert_eq!(s.shell_idx, 1);
    }

    #[test]
    fn test_commit_selection_notification() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectNotifications;
        s.cursor = 3; // none
        s.commit_selection();
        assert_eq!(s.notification_idx, 3);
    }

    #[test]
    fn test_commit_selection_proposal() {
        let mut s = WizardState::new(two_agent_detected());
        s.step = Step::SelectProposal;
        s.cursor = 1;
        s.commit_selection();
        assert_eq!(s.proposal_agent_idx, 1);
    }

    // -- into_answers mapping --

    #[test]
    fn test_into_answers_defaults() {
        let s = WizardState::new(two_agent_detected());
        let a = s.into_answers();
        assert_eq!(a.scan_interval, Interval::Weekly);
        assert_eq!(a.proposal_agent, AgentKind::Claude);
        assert_eq!(a.notifications, NotificationPref::Both);
        assert_eq!(a.enabled_agents.len(), 2);
    }

    #[test]
    fn test_into_answers_daily_interval() {
        let mut s = WizardState::new(two_agent_detected());
        s.scan_interval_idx = 0;
        let a = s.into_answers();
        assert_eq!(a.scan_interval, Interval::Daily);
    }

    #[test]
    fn test_into_answers_monthly_interval() {
        let mut s = WizardState::new(two_agent_detected());
        s.scan_interval_idx = 2;
        let a = s.into_answers();
        assert_eq!(a.scan_interval, Interval::Monthly);
    }

    #[test]
    fn test_into_answers_shell_variants() {
        for (idx, expected) in [
            (0, ShellType::Zsh),
            (1, ShellType::Bash),
            (2, ShellType::Fish),
            (3, ShellType::Other),
        ] {
            let mut s = WizardState::new(two_agent_detected());
            s.shell_idx = idx;
            let a = s.into_answers();
            assert_eq!(a.shell, expected);
        }
    }

    #[test]
    fn test_into_answers_notification_variants() {
        for (idx, expected) in [
            (0, NotificationPref::Terminal),
            (1, NotificationPref::Native),
            (2, NotificationPref::Both),
            (3, NotificationPref::None),
        ] {
            let mut s = WizardState::new(two_agent_detected());
            s.notification_idx = idx;
            let a = s.into_answers();
            assert_eq!(a.notifications, expected);
        }
    }

    #[test]
    fn test_into_answers_proposal_agent_codex() {
        let mut s = WizardState::new(two_agent_detected());
        s.proposal_agent_idx = 1;
        let a = s.into_answers();
        assert_eq!(a.proposal_agent, AgentKind::Codex);
    }

    #[test]
    fn test_into_answers_with_disabled_agent() {
        let mut s = WizardState::new(two_agent_detected());
        s.agents_enabled[1] = false; // disable Codex
        let a = s.into_answers();
        assert_eq!(a.enabled_agents, vec![AgentKind::Claude]);
    }

    // -- No agents detected: advance enables all as fallback --

    #[test]
    fn test_no_agents_detected_fallback_enables_all() {
        let mut s = WizardState::new(no_agent_detected());
        assert!(!s.agents_enabled[0]);
        assert!(!s.agents_enabled[1]);

        s.advance(); // Welcome -> SelectAgents
        s.advance(); // SelectAgents -> SelectInterval (triggers fallback)
        assert!(s.agents_enabled[0]);
        assert!(s.agents_enabled[1]);
    }

    // -- Total steps and step numbers --

    #[test]
    fn test_total_steps_with_two_agents() {
        let s = WizardState::new(two_agent_detected());
        assert_eq!(s.total_steps(), 7);
    }

    #[test]
    fn test_total_steps_with_one_agent() {
        let s = WizardState::new(one_agent_detected());
        assert_eq!(s.total_steps(), 6);
    }

    #[test]
    fn test_step_numbers_two_agents() {
        let mut s = WizardState::new(two_agent_detected());
        assert_eq!(s.step_number(), 1); // Welcome
        s.advance();
        assert_eq!(s.step_number(), 2); // SelectAgents
        s.advance();
        assert_eq!(s.step_number(), 3); // SelectInterval
        s.advance();
        assert_eq!(s.step_number(), 4); // SelectProposal
        s.advance();
        assert_eq!(s.step_number(), 5); // SelectShell
        s.advance();
        assert_eq!(s.step_number(), 6); // SelectNotifications
        s.advance();
        assert_eq!(s.step_number(), 7); // Summary
    }

    #[test]
    fn test_step_numbers_single_agent() {
        let mut s = WizardState::new(one_agent_detected());
        assert_eq!(s.step_number(), 1); // Welcome
        s.advance();
        assert_eq!(s.step_number(), 2); // SelectAgents
        s.advance();
        assert_eq!(s.step_number(), 3); // SelectInterval
        s.advance();
        assert_eq!(s.step_number(), 4); // SelectShell (proposal skipped)
        s.advance();
        assert_eq!(s.step_number(), 5); // SelectNotifications
        s.advance();
        assert_eq!(s.step_number(), 6); // Summary
    }

    // -- Cursor restored on go_back --

    #[test]
    fn test_cursor_restored_on_go_back() {
        let mut s = WizardState::new(two_agent_detected());
        s.advance(); // SelectAgents
        s.advance(); // SelectInterval
        s.cursor = 2;
        s.commit_selection(); // scan_interval_idx = 2
        s.advance(); // SelectProposal
        s.go_back(); // back to SelectInterval
        assert_eq!(s.cursor, 2); // restored
    }
}
