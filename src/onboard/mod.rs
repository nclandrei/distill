// Onboarding flow — interactive first-run setup.

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs, Wrap,
    },
};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::Duration;

use crate::agents::{AgentKind, from_kind};
use crate::config::{AgentEntry, Config, Interval, NotificationPref, ShellType};
use crate::schedule;
use crate::shell::{self, HookStatus};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Holds all choices gathered during the onboarding flow.
/// The struct is deliberately decoupled from I/O so that `build_config` can be
/// exercised in unit tests without any stdin/stdout interaction.
pub struct OnboardingAnswers {
    /// Every known agent paired with whether its config directory was found on disk.
    pub detected_agents: Vec<(AgentKind, bool)>,
    /// Subset of agents the user chose to enable.
    pub enabled_agents: Vec<AgentKind>,
    /// How often to run a scan.
    pub scan_interval: Interval,
    /// Which agent generates skill proposals.
    pub proposal_agent: AgentKind,
    /// The user's shell.
    pub shell: ShellType,
    /// How to deliver notifications.
    pub notifications: NotificationPref,
}

// ---------------------------------------------------------------------------
// Pure logic functions (testable without I/O)
// ---------------------------------------------------------------------------

/// Detect which agents are installed by checking whether their config
/// directories exist under `home`.
///
/// Returns a `Vec<(AgentKind, bool)>` where `bool` is `true` when the
/// agent's config directory is present.
pub fn detect_agents(home: &Path) -> Vec<(AgentKind, bool)> {
    AgentKind::all()
        .into_iter()
        .map(|kind| {
            let installed = from_kind(kind, home.to_path_buf()).is_installed();
            (kind, installed)
        })
        .collect()
}

/// Build a `Config` from the user's onboarding answers.
///
/// This is a pure function — no side-effects, fully testable.
pub fn build_config(answers: &OnboardingAnswers) -> Config {
    // One AgentEntry per detected agent; enabled if the user selected it.
    let agents: Vec<AgentEntry> = answers
        .detected_agents
        .iter()
        .map(|(kind, _installed)| AgentEntry {
            name: kind.to_string(),
            enabled: answers.enabled_agents.contains(kind),
        })
        .collect();

    Config {
        agents,
        scan_interval: answers.scan_interval.clone(),
        proposal_agent: answers.proposal_agent.to_string(),
        shell: answers.shell.clone(),
        notifications: answers.notifications.clone(),
        notification_icon: None,
    }
}

/// Side effects applied after onboarding choices are persisted.
pub struct PostSetupResult {
    /// Shell hook install result (or `None` when user skipped installation).
    pub hook_status: Option<HookStatus>,
    /// Path to the scheduler file created during installation.
    pub scheduler_path: std::path::PathBuf,
}

/// Apply post-onboarding setup:
/// * Optionally install shell hook
/// * Always install scheduler with the chosen interval
pub fn apply_post_onboarding_setup(
    config: &Config,
    home: &Path,
    install_shell_hook: bool,
) -> Result<PostSetupResult> {
    let hook_status = if install_shell_hook {
        Some(shell::install_hook(&config.shell, home)?)
    } else {
        None
    };

    #[cfg(test)]
    let scheduler = schedule::create_scheduler_for_tests(home.to_path_buf());
    #[cfg(not(test))]
    let scheduler = schedule::create_scheduler(home.to_path_buf());
    scheduler.install(&config.scan_interval)?;
    let scheduler_path = scheduler.plist_or_unit_path();

    Ok(PostSetupResult {
        hook_status,
        scheduler_path,
    })
}

// ---------------------------------------------------------------------------
// Interactive TUI helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnboardingStep {
    Agents,
    Interval,
    ProposalAgent,
    Shell,
    Hook,
    Notifications,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    Save,
    Back,
    Cancel,
}

#[derive(Debug, Clone)]
struct OnboardingUiState {
    detected_agents: Vec<(AgentKind, bool)>,
    all_agents: Vec<AgentKind>,
    selected_agents: Vec<AgentKind>,
    proposal_agent: AgentKind,
    step: OnboardingStep,
    agent_cursor: usize,
    interval_cursor: usize,
    shell_cursor: usize,
    notif_cursor: usize,
    install_shell_hook: bool,
    confirm_action: ConfirmAction,
    status_line: String,
}

impl OnboardingUiState {
    fn new(detected_agents: Vec<(AgentKind, bool)>) -> Self {
        let all_agents = AgentKind::all();
        let installed_agents: Vec<AgentKind> = detected_agents
            .iter()
            .filter(|(_, installed)| *installed)
            .map(|(kind, _)| *kind)
            .collect();

        let selected_agents = if installed_agents.is_empty() {
            all_agents.clone()
        } else {
            installed_agents.clone()
        };

        let detected_shell = ShellType::detect();
        let shell_cursor = shell_to_index(&detected_shell);

        let mut state = Self {
            proposal_agent: selected_agents
                .first()
                .copied()
                .unwrap_or(AgentKind::Claude),
            detected_agents,
            all_agents,
            selected_agents,
            step: OnboardingStep::Agents,
            agent_cursor: 0,
            interval_cursor: 1, // weekly
            shell_cursor,
            notif_cursor: 2, // both
            install_shell_hook: true,
            confirm_action: ConfirmAction::Save,
            status_line: "Ready.".to_string(),
        };
        state.ensure_proposal_agent_valid();
        state
    }

    fn selected_shell(&self) -> ShellType {
        index_to_shell(self.shell_cursor)
    }

    fn selected_interval(&self) -> Interval {
        match self.interval_cursor {
            0 => Interval::Daily,
            2 => Interval::Monthly,
            _ => Interval::Weekly,
        }
    }

    fn selected_notifications(&self) -> NotificationPref {
        match self.notif_cursor {
            0 => NotificationPref::Terminal,
            1 => NotificationPref::Native,
            3 => NotificationPref::None,
            _ => NotificationPref::Both,
        }
    }

    fn installed_agents(&self) -> Vec<AgentKind> {
        self.detected_agents
            .iter()
            .filter(|(_, installed)| *installed)
            .map(|(kind, _)| *kind)
            .collect()
    }

    fn proposal_options(&self) -> Vec<AgentKind> {
        if !self.selected_agents.is_empty() {
            self.selected_agents.clone()
        } else {
            let installed = self.installed_agents();
            if installed.is_empty() {
                self.all_agents.clone()
            } else {
                installed
            }
        }
    }

    fn ensure_proposal_agent_valid(&mut self) {
        let options = self.proposal_options();
        if !options.contains(&self.proposal_agent)
            && let Some(first) = options.first()
        {
            self.proposal_agent = *first;
        }
    }

    fn step_sequence(&self) -> Vec<OnboardingStep> {
        let mut steps = vec![
            OnboardingStep::Agents,
            OnboardingStep::Interval,
            OnboardingStep::ProposalAgent,
            OnboardingStep::Shell,
        ];
        if self.selected_shell() != ShellType::Other {
            steps.push(OnboardingStep::Hook);
        }
        steps.push(OnboardingStep::Notifications);
        steps.push(OnboardingStep::Confirm);
        steps
    }

    fn step_index(&self) -> usize {
        self.step_sequence()
            .iter()
            .position(|step| *step == self.step)
            .unwrap_or(0)
    }

    fn step_short_label(step: OnboardingStep) -> &'static str {
        match step {
            OnboardingStep::Agents => "Agents",
            OnboardingStep::Interval => "Cadence",
            OnboardingStep::ProposalAgent => "Generator",
            OnboardingStep::Shell => "Shell",
            OnboardingStep::Hook => "Hook",
            OnboardingStep::Notifications => "Alerts",
            OnboardingStep::Confirm => "Review",
        }
    }

    fn progress(&self) -> (usize, usize) {
        let total = self.step_sequence().len();
        (self.step_index() + 1, total)
    }

    fn step_title(&self) -> &'static str {
        match self.step {
            OnboardingStep::Agents => "Choose agents to monitor",
            OnboardingStep::Interval => "Choose scan interval",
            OnboardingStep::ProposalAgent => "Choose proposal agent",
            OnboardingStep::Shell => "Confirm shell",
            OnboardingStep::Hook => "Install terminal hook",
            OnboardingStep::Notifications => "Choose notifications",
            OnboardingStep::Confirm => "Review and confirm",
        }
    }

    fn selected_agents_label(&self) -> String {
        if self.selected_agents.is_empty() {
            "(none)".to_string()
        } else {
            self.selected_agents
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    fn install_hook_effective(&self) -> bool {
        self.selected_shell() != ShellType::Other && self.install_shell_hook
    }

    fn active_summary_row(&self) -> Option<usize> {
        match self.step {
            OnboardingStep::Agents => Some(0),
            OnboardingStep::Interval => Some(1),
            OnboardingStep::ProposalAgent => Some(2),
            OnboardingStep::Shell => Some(3),
            OnboardingStep::Hook => Some(4),
            OnboardingStep::Notifications => Some(5),
            OnboardingStep::Confirm => None,
        }
    }

    fn jump_to_step_number(&mut self, number: usize) -> bool {
        if number == 0 {
            return false;
        }
        let steps = self.step_sequence();
        let Some(step) = steps.get(number - 1) else {
            return false;
        };
        self.step = *step;
        self.on_step_changed();
        true
    }

    fn normalize_step_after_shell_change(&mut self) {
        if self.selected_shell() == ShellType::Other && self.step == OnboardingStep::Hook {
            self.step = OnboardingStep::Notifications;
        }
    }

    fn contextual_help(&self) -> Vec<String> {
        match self.step {
            OnboardingStep::Agents => {
                if self.selected_agents.is_empty() {
                    vec![
                        "No monitored agents selected: scans will not collect sessions."
                            .to_string(),
                        "Proposal generation falls back to detected/default agents.".to_string(),
                        "Tip: keep at least one agent selected so generated skills match your workflow."
                            .to_string(),
                    ]
                } else {
                    vec![
                        "Only checked agents are scanned for new session patterns.".to_string(),
                        "Disable an agent if you don't want its skills mixed in.".to_string(),
                        "Tip: use [a] to select all or [n] to clear quickly.".to_string(),
                    ]
                }
            }
            OnboardingStep::Interval => match self.selected_interval() {
                Interval::Daily => vec![
                    "Fastest feedback loop with higher background activity.".to_string(),
                    "Best when your skill set changes frequently across many sessions.".to_string(),
                    "Tip: if notifications feel noisy, switch to weekly later.".to_string(),
                ],
                Interval::Weekly => vec![
                    "Balanced cadence for most repos and teams.".to_string(),
                    "Collects enough evidence while keeping proposal volume manageable."
                        .to_string(),
                    "Tip: weekly works well with Friday review habits.".to_string(),
                ],
                Interval::Monthly => vec![
                    "Lowest noise, but proposal turnaround will be slower.".to_string(),
                    "Good for stable projects where workflows rarely change.".to_string(),
                    "Tip: use 'distill scan --now' whenever you want instant proposals."
                        .to_string(),
                ],
            },
            OnboardingStep::ProposalAgent => vec![
                format!(
                    "'{}' will format and generate proposed skills from collected sessions.",
                    self.proposal_agent
                ),
                "Choose the agent whose outputs you trust most for writing reusable instructions."
                    .to_string(),
            ],
            OnboardingStep::Shell => match self.selected_shell() {
                ShellType::Other => vec![
                    "Auto hook install is disabled for 'other' shells.".to_string(),
                    "Use 'distill notify --check' manually from your prompt flow.".to_string(),
                    "Tip: you can still keep native notifications enabled below.".to_string(),
                ],
                shell => vec![
                    format!(
                        "Hook snippets and integration paths will target the {} shell.",
                        shell
                    ),
                    "Detecting the wrong shell? Set $SHELL before rerunning onboarding."
                        .to_string(),
                ],
            },
            OnboardingStep::Hook => {
                if self.install_hook_effective() {
                    vec![
                        "A hook will run 'distill notify --check' at prompt boundaries."
                            .to_string(),
                        "You can uninstall later via the watch/shell commands.".to_string(),
                        "Tip: keep this on if you want immediate pending-proposal reminders."
                            .to_string(),
                    ]
                } else {
                    vec![
                        "No shell file changes will be made during setup.".to_string(),
                        "Notifications still work via native alerts/scheduled scans.".to_string(),
                        "Tip: you can enable the shell hook later without rerunning onboarding."
                            .to_string(),
                    ]
                }
            }
            OnboardingStep::Notifications => match self.selected_notifications() {
                NotificationPref::Terminal => vec![
                    "Terminal messages appear on your next prompt; no OS banners.".to_string(),
                    "Best for users who keep terminal focus and want low interruption.".to_string(),
                ],
                NotificationPref::Native => vec![
                    "OS notifications are shown; terminal stays clean.".to_string(),
                    "Useful when scans run in the background and terminal is closed.".to_string(),
                ],
                NotificationPref::Both => vec![
                    "Terminal + native notifications for maximum visibility.".to_string(),
                    "Recommended when you're still calibrating scan cadence.".to_string(),
                ],
                NotificationPref::None => vec![
                    "No runtime alerts. You'll check status/review manually.".to_string(),
                    "Use this when you prefer explicit review rituals over real-time nudges."
                        .to_string(),
                ],
            },
            OnboardingStep::Confirm => vec![
                "Save writes ~/.distill/config.yaml and installs scheduler integration."
                    .to_string(),
                "Cancel leaves your environment unchanged.".to_string(),
                "You can rerun onboarding any time by removing the config file.".to_string(),
            ],
        }
    }

    fn next_step(&mut self) {
        let steps = self.step_sequence();
        let idx = self.step_index();
        if idx + 1 < steps.len() {
            self.step = steps[idx + 1];
            self.on_step_changed();
        }
    }

    fn previous_step(&mut self) {
        let steps = self.step_sequence();
        let idx = self.step_index();
        if idx > 0 {
            self.step = steps[idx - 1];
            self.on_step_changed();
        }
    }

    fn on_step_changed(&mut self) {
        if self.step == OnboardingStep::Confirm {
            self.confirm_action = ConfirmAction::Save;
        }
    }

    fn cycle_confirm_action_prev(&mut self) {
        self.confirm_action = match self.confirm_action {
            ConfirmAction::Save => ConfirmAction::Cancel,
            ConfirmAction::Back => ConfirmAction::Save,
            ConfirmAction::Cancel => ConfirmAction::Back,
        };
    }

    fn cycle_confirm_action_next(&mut self) {
        self.confirm_action = match self.confirm_action {
            ConfirmAction::Save => ConfirmAction::Back,
            ConfirmAction::Back => ConfirmAction::Cancel,
            ConfirmAction::Cancel => ConfirmAction::Save,
        };
    }

    fn move_up(&mut self) {
        match self.step {
            OnboardingStep::Agents => {
                self.agent_cursor = cycle_prev(self.agent_cursor, self.all_agents.len())
            }
            OnboardingStep::Interval => self.interval_cursor = cycle_prev(self.interval_cursor, 3),
            OnboardingStep::ProposalAgent => {
                let options = self.proposal_options();
                let current = options
                    .iter()
                    .position(|kind| *kind == self.proposal_agent)
                    .unwrap_or(0);
                let next = cycle_prev(current, options.len());
                if let Some(kind) = options.get(next) {
                    self.proposal_agent = *kind;
                }
            }
            OnboardingStep::Shell => self.shell_cursor = cycle_prev(self.shell_cursor, 4),
            OnboardingStep::Hook => self.install_shell_hook = true,
            OnboardingStep::Notifications => self.notif_cursor = cycle_prev(self.notif_cursor, 4),
            OnboardingStep::Confirm => {}
        }
        self.normalize_step_after_shell_change();
    }

    fn move_down(&mut self) {
        match self.step {
            OnboardingStep::Agents => {
                self.agent_cursor = cycle_next(self.agent_cursor, self.all_agents.len())
            }
            OnboardingStep::Interval => self.interval_cursor = cycle_next(self.interval_cursor, 3),
            OnboardingStep::ProposalAgent => {
                let options = self.proposal_options();
                let current = options
                    .iter()
                    .position(|kind| *kind == self.proposal_agent)
                    .unwrap_or(0);
                let next = cycle_next(current, options.len());
                if let Some(kind) = options.get(next) {
                    self.proposal_agent = *kind;
                }
            }
            OnboardingStep::Shell => self.shell_cursor = cycle_next(self.shell_cursor, 4),
            OnboardingStep::Hook => self.install_shell_hook = false,
            OnboardingStep::Notifications => self.notif_cursor = cycle_next(self.notif_cursor, 4),
            OnboardingStep::Confirm => {}
        }
        self.normalize_step_after_shell_change();
    }

    fn toggle_current(&mut self) {
        match self.step {
            OnboardingStep::Agents => {
                if self.all_agents.is_empty() {
                    return;
                }
                let kind = self.all_agents[self.agent_cursor];
                if let Some(pos) = self
                    .selected_agents
                    .iter()
                    .position(|selected| *selected == kind)
                {
                    self.selected_agents.remove(pos);
                } else {
                    self.selected_agents.push(kind);
                    self.selected_agents.sort_by_key(|selected| {
                        self.all_agents
                            .iter()
                            .position(|candidate| candidate == selected)
                            .unwrap_or(usize::MAX)
                    });
                }
                self.ensure_proposal_agent_valid();
            }
            OnboardingStep::Hook => {
                self.install_shell_hook = !self.install_shell_hook;
            }
            _ => {}
        }
    }
}

fn cycle_prev(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else if index == 0 {
        len - 1
    } else {
        index - 1
    }
}

fn cycle_next(index: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (index + 1) % len }
}

fn shell_to_index(shell: &ShellType) -> usize {
    match shell {
        ShellType::Zsh => 0,
        ShellType::Bash => 1,
        ShellType::Fish => 2,
        ShellType::Other => 3,
    }
}

fn index_to_shell(index: usize) -> ShellType {
    match index {
        1 => ShellType::Bash,
        2 => ShellType::Fish,
        3 => ShellType::Other,
        _ => ShellType::Zsh,
    }
}

fn shell_hook_target_path(shell: ShellType) -> Option<&'static str> {
    match shell {
        ShellType::Zsh => Some("~/.zshrc"),
        ShellType::Bash => Some("~/.bashrc"),
        ShellType::Fish => Some("~/.config/fish/conf.d/distill.fish"),
        ShellType::Other => None,
    }
}

#[cfg(target_os = "macos")]
fn scheduler_target_preview() -> &'static str {
    "~/Library/LaunchAgents/com.distill.agent.plist"
}

#[cfg(target_os = "linux")]
fn scheduler_target_preview() -> &'static str {
    "~/.config/systemd/user/distill.service"
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn scheduler_target_preview() -> &'static str {
    "platform-specific scheduler target"
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup_width = width.min(area.width.saturating_sub(2)).max(10);
    let popup_height = height.min(area.height.saturating_sub(2)).max(6);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
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

    fn draw(&mut self, state: &OnboardingUiState) -> Result<()> {
        self.terminal
            .draw(|frame| draw_onboarding_ui(frame, state))
            .context("Failed to render onboarding UI")?;
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

enum OnboardingExit {
    Completed(OnboardingAnswers, bool),
    Canceled,
}

fn draw_onboarding_ui(frame: &mut Frame<'_>, state: &OnboardingUiState) {
    const ACCENT: Color = Color::Cyan;
    const EMPHASIS: Color = Color::Yellow;
    const MUTED: Color = Color::DarkGray;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let (step_num, step_total) = state.progress();

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "DISTILL ONBOARDING",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("Step {step_num}/{step_total}"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            state.step_title(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    frame.render_widget(header, chunks[0]);

    let current_idx = state.step_index();
    let tabs = Tabs::new(
        state
            .step_sequence()
            .iter()
            .enumerate()
            .map(|(idx, step)| {
                let marker = if idx < current_idx { "✓ " } else { "" };
                format!(
                    "{marker}{}. {}",
                    idx + 1,
                    OnboardingUiState::step_short_label(*step)
                )
            })
            .collect::<Vec<_>>(),
    )
    .select(current_idx)
    .divider("│")
    .style(Style::default().fg(MUTED))
    .highlight_style(Style::default().fg(EMPHASIS).add_modifier(Modifier::BOLD))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MUTED))
            .title(Span::styled(
                "FLOW",
                Style::default().add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(tabs, chunks[1]);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(chunks[2]);

    if state.step == OnboardingStep::Confirm {
        let confirm_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(6)])
            .split(body_chunks[0]);

        let hook_plan = match shell_hook_target_path(state.selected_shell()) {
            Some(path) if state.install_hook_effective() => format!("update {path}"),
            Some(path) => format!("skip (disabled, target: {path})"),
            None => "skip (shell=other)".to_string(),
        };
        let monitored_count = state.selected_agents.len();
        let monitored_plan = if monitored_count == 0 {
            "no direct agent scans (fallback proposal source)".to_string()
        } else {
            format!("{monitored_count} monitored agent(s)")
        };
        let impact_rows = vec![
            Row::new(vec!["Config", "write ~/.distill/config.yaml"]),
            Row::new(vec!["Scheduler", scheduler_target_preview()]),
            Row::new(vec!["Shell hook", &hook_plan]),
            Row::new(vec!["Scan source", &monitored_plan]),
        ];
        let impact_table = Table::new(impact_rows, [Constraint::Length(12), Constraint::Min(20)])
            .header(
                Row::new(vec![Cell::from("Change"), Cell::from("Result")])
                    .style(Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)),
            )
            .column_spacing(2)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(MUTED))
                    .title(Span::styled(
                        "APPLY IMPACT",
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
            );
        frame.render_widget(impact_table, confirm_chunks[0]);

        let controls = Paragraph::new(vec![
            Line::from("Review looks good? Apply now."),
            Line::from(""),
            Line::from(vec![
                Span::styled("[Left/Right] ", Style::default().fg(ACCENT)),
                Span::raw("Change focused action"),
            ]),
            Line::from(vec![
                Span::styled("[Enter] ", Style::default().fg(Color::Green)),
                Span::raw("Run focused action"),
            ]),
            Line::from(vec![
                Span::styled("[1-7] ", Style::default().fg(ACCENT)),
                Span::raw("Jump directly to a section"),
            ]),
            Line::from(vec![
                Span::styled("[q] ", Style::default().fg(Color::Red)),
                Span::raw("Cancel with no changes"),
            ]),
        ])
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(MUTED))
                .title(Span::styled(
                    "CONTROLS",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
        );
        frame.render_widget(controls, confirm_chunks[1]);
    } else {
        let (options_title, options, selected_idx): (&str, Vec<String>, Option<usize>) =
            match state.step {
                OnboardingStep::Agents => (
                    "Agents",
                    state
                        .all_agents
                        .iter()
                        .map(|kind| {
                            let checked = if state.selected_agents.contains(kind) {
                                "[x]"
                            } else {
                                "[ ]"
                            };
                            let installed = state
                                .detected_agents
                                .iter()
                                .find(|(detected_kind, _)| detected_kind == kind)
                                .map(|(_, installed)| *installed)
                                .unwrap_or(false);
                            let suffix = if installed { "" } else { " (not detected)" };
                            format!("{checked} {kind}{suffix}")
                        })
                        .collect(),
                    Some(state.agent_cursor),
                ),
                OnboardingStep::Interval => (
                    "Scan Interval",
                    vec![
                        "daily".to_string(),
                        "weekly (recommended)".to_string(),
                        "monthly".to_string(),
                    ],
                    Some(state.interval_cursor),
                ),
                OnboardingStep::ProposalAgent => {
                    let options = state
                        .proposal_options()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    let selected = state
                        .proposal_options()
                        .iter()
                        .position(|kind| *kind == state.proposal_agent)
                        .unwrap_or(0);
                    ("Proposal Agent", options, Some(selected))
                }
                OnboardingStep::Shell => (
                    "Shell",
                    vec![
                        "zsh".to_string(),
                        "bash".to_string(),
                        "fish".to_string(),
                        "other".to_string(),
                    ],
                    Some(state.shell_cursor),
                ),
                OnboardingStep::Hook => (
                    "Terminal Hook",
                    vec![
                        "yes - install notification hook".to_string(),
                        "no - skip hook install".to_string(),
                    ],
                    Some(usize::from(!state.install_shell_hook)),
                ),
                OnboardingStep::Notifications => (
                    "Notifications",
                    vec![
                        "terminal".to_string(),
                        "native".to_string(),
                        "both (recommended)".to_string(),
                        "none".to_string(),
                    ],
                    Some(state.notif_cursor),
                ),
                OnboardingStep::Confirm => unreachable!(),
            };

        let option_items = options
            .iter()
            .map(|line| ListItem::new(line.clone()))
            .collect::<Vec<_>>();
        let option_title = options_title.to_uppercase();
        let option_list = List::new(option_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT))
                    .title(Span::styled(
                        option_title,
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
            )
            .highlight_symbol("▸ ")
            .highlight_style(Style::default().fg(EMPHASIS).add_modifier(Modifier::BOLD));

        let mut list_state = ListState::default();
        if let Some(idx) = selected_idx {
            list_state.select(Some(idx));
        }
        frame.render_stateful_widget(option_list, body_chunks[0], &mut list_state);
    }

    let details_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(11), Constraint::Min(8)])
        .split(body_chunks[1]);

    let fallback_note = if state.selected_agents.is_empty() {
        "fallback mode"
    } else {
        "from monitored"
    };
    let summary_rows = [
        ("Agents", state.selected_agents_label()),
        ("Interval", state.selected_interval().to_string()),
        (
            "Proposal",
            format!("{} ({fallback_note})", state.proposal_agent),
        ),
        ("Shell", state.selected_shell().to_string()),
        (
            "Hook install",
            if state.selected_shell() == ShellType::Other {
                "n/a for shell=other".to_string()
            } else if state.install_hook_effective() {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            },
        ),
        ("Notifications", state.selected_notifications().to_string()),
    ];

    let summary_table_rows = summary_rows
        .iter()
        .enumerate()
        .map(|(idx, (label, value))| {
            let mut style = Style::default();
            if Some(idx) == state.active_summary_row() {
                style = style.fg(EMPHASIS).add_modifier(Modifier::BOLD);
            } else if idx == 4 && state.selected_shell() == ShellType::Other {
                style = style.fg(MUTED);
            }
            Row::new(vec![Cell::from(*label), Cell::from(value.clone())]).style(style)
        })
        .collect::<Vec<_>>();

    let summary_table = Table::new(
        summary_table_rows,
        [Constraint::Length(14), Constraint::Min(16)],
    )
    .column_spacing(2)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MUTED))
            .title(Span::styled(
                "CURRENT SETUP",
                Style::default().add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(summary_table, details_chunks[0]);

    let help_lines = state
        .contextual_help()
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let bullet = if idx == 0 { "→ " } else { "• " };
            Line::from(format!("{bullet}{line}"))
        })
        .collect::<Vec<_>>();
    let help = Paragraph::new(help_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    "WHY THIS CHOICE",
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(MUTED)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(help, details_chunks[1]);

    let primary_help = match state.step {
        OnboardingStep::Agents => {
            "Up/Down move | Space toggle | a all | n clear | Enter next | q cancel"
        }
        OnboardingStep::Hook => "Up/Down or Space toggle yes/no | Enter next | q cancel",
        OnboardingStep::Confirm => "Left/Right choose action | Enter apply | 1-7 jump | q cancel",
        _ => "Up/Down change selection | Enter next | Backspace previous | q cancel",
    };
    let secondary_help = if state.status_line == "Ready." {
        "Tip: use 1-7 to jump across sections."
    } else {
        state.status_line.as_str()
    };

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Keys: ", Style::default().fg(ACCENT)),
            Span::raw(primary_help),
        ]),
        Line::from(Span::styled(secondary_help, Style::default().fg(MUTED))),
    ])
    .block(Block::default().borders(Borders::TOP))
    .wrap(Wrap { trim: true });
    frame.render_widget(footer, chunks[3]);

    if state.step == OnboardingStep::Confirm {
        let popup_area = centered_rect(frame.area(), 64, 9);
        frame.render_widget(Clear, popup_area);

        let button = |action: ConfirmAction, label: &str| -> Span<'static> {
            let marker = if state.confirm_action == action {
                "[x]"
            } else {
                "[ ]"
            };
            if state.confirm_action == action {
                Span::styled(
                    format!("{marker} {label}"),
                    Style::default()
                        .fg(Color::Black)
                        .bg(EMPHASIS)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!("{marker} {label}"), Style::default().fg(MUTED))
            }
        };

        let modal = Paragraph::new(vec![
            Line::from("Final confirmation"),
            Line::from("Choose an action, then press Enter."),
            Line::from(""),
            Line::from(vec![
                button(ConfirmAction::Save, "Save"),
                Span::raw("   "),
                button(ConfirmAction::Back, "Back"),
                Span::raw("   "),
                button(ConfirmAction::Cancel, "Cancel"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Tip: Tab/Shift-Tab also cycles the focused action.",
                Style::default().fg(MUTED),
            )),
        ])
        .alignment(ratatui::layout::Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(Span::styled(
                    "FINAL CHECK",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
        );
        frame.render_widget(modal, popup_area);
    }
}

fn run_tui_flow(state: &mut OnboardingUiState) -> Result<OnboardingExit> {
    let mut tui = TuiSession::enter()?;

    loop {
        tui.draw(state)?;

        if !event::poll(Duration::from_millis(200)).context("Failed to poll terminal events")? {
            continue;
        }

        let Event::Key(key) = event::read().context("Failed to read terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(OnboardingExit::Canceled),
            KeyCode::Up | KeyCode::Char('k') => state.move_up(),
            KeyCode::Down | KeyCode::Char('j') => state.move_down(),
            KeyCode::Left | KeyCode::Char('h') => {
                if state.step == OnboardingStep::Confirm {
                    state.cycle_confirm_action_prev();
                } else if state.step == OnboardingStep::Hook {
                    state.install_shell_hook = true;
                } else {
                    state.previous_step();
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if state.step == OnboardingStep::Confirm {
                    state.cycle_confirm_action_next();
                } else if state.step == OnboardingStep::Hook {
                    state.install_shell_hook = false;
                } else {
                    state.next_step();
                }
            }
            KeyCode::Backspace => state.previous_step(),
            KeyCode::BackTab => {
                if state.step == OnboardingStep::Confirm {
                    state.cycle_confirm_action_prev();
                } else {
                    state.previous_step();
                }
            }
            KeyCode::Tab => {
                if state.step == OnboardingStep::Confirm {
                    state.cycle_confirm_action_next();
                } else {
                    state.next_step();
                }
            }
            KeyCode::Char('a') if state.step == OnboardingStep::Agents => {
                state.selected_agents = state.all_agents.clone();
                state.ensure_proposal_agent_valid();
                state.status_line = "Selected all agents.".to_string();
            }
            KeyCode::Char('n') if state.step == OnboardingStep::Agents => {
                state.selected_agents.clear();
                state.ensure_proposal_agent_valid();
                state.status_line = "Cleared monitored agents.".to_string();
            }
            KeyCode::Char('y') if state.step == OnboardingStep::Hook => {
                state.install_shell_hook = true;
                state.status_line = "Shell hook enabled.".to_string();
            }
            KeyCode::Char('n') if state.step == OnboardingStep::Hook => {
                state.install_shell_hook = false;
                state.status_line = "Shell hook disabled.".to_string();
            }
            KeyCode::Char(' ') => state.toggle_current(),
            KeyCode::Char(d @ '1'..='7') => {
                let step_no = (d as u8 - b'0') as usize;
                if state.jump_to_step_number(step_no) {
                    state.status_line = format!("Jumped to step {step_no}.");
                } else {
                    state.status_line = format!("Step {step_no} is not available for this setup.");
                }
            }
            KeyCode::Enter => {
                if state.step == OnboardingStep::Confirm {
                    match state.confirm_action {
                        ConfirmAction::Save => {
                            let answers = OnboardingAnswers {
                                detected_agents: state.detected_agents.clone(),
                                enabled_agents: state.selected_agents.clone(),
                                scan_interval: state.selected_interval(),
                                proposal_agent: state.proposal_agent,
                                shell: state.selected_shell(),
                                notifications: state.selected_notifications(),
                            };
                            return Ok(OnboardingExit::Completed(
                                answers,
                                state.install_hook_effective(),
                            ));
                        }
                        ConfirmAction::Back => {
                            state.previous_step();
                            state.status_line = "Returned from final check.".to_string();
                        }
                        ConfirmAction::Cancel => return Ok(OnboardingExit::Canceled),
                    }
                } else {
                    state.next_step();
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive entry point
// ---------------------------------------------------------------------------

/// Run the full interactive onboarding flow.
pub fn run_interactive() -> Result<()> {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        println!();
        println!("Welcome to distill! Let's set things up.");
        println!("Run 'distill' in an interactive terminal to complete onboarding.");
        return Ok(());
    }

    let detected = detect_agents(&home);
    let mut state = OnboardingUiState::new(detected);

    let exit = run_tui_flow(&mut state)?;
    let OnboardingExit::Completed(answers, install_shell_hook) = exit else {
        println!();
        println!("Onboarding canceled. No configuration was written.");
        return Ok(());
    };

    let config = build_config(&answers);
    config.save()?;
    let post_setup = apply_post_onboarding_setup(&config, &home, install_shell_hook)?;

    let enabled_names: Vec<&str> = config
        .agents
        .iter()
        .filter(|a| a.enabled)
        .map(|a| a.name.as_str())
        .collect();
    let enabled_display = if enabled_names.is_empty() {
        "(none)".to_string()
    } else {
        enabled_names.join(", ")
    };

    println!();
    println!("Configuration saved to {}", Config::config_path().display());
    println!();
    println!("Summary:");
    println!("  Agents monitored : {enabled_display}");
    println!("  Scan interval    : {}", config.scan_interval);
    println!("  Proposal agent   : {}", config.proposal_agent);
    println!("  Shell            : {}", config.shell);
    println!("  Notifications    : {}", config.notifications);
    println!(
        "  Notification icon: {}",
        config.notification_icon.as_deref().unwrap_or("none")
    );
    println!();
    println!("Setup:");
    match post_setup.hook_status {
        Some(HookStatus::Installed) => println!("  Shell hook       : installed"),
        Some(HookStatus::AlreadyInstalled) => println!("  Shell hook       : already installed"),
        Some(HookStatus::Unsupported) => {
            println!("  Shell hook       : unsupported shell (manual setup required)")
        }
        Some(HookStatus::Removed) | Some(HookStatus::NotFound) => {
            println!("  Shell hook       : not installed")
        }
        None => println!("  Shell hook       : skipped"),
    }
    println!(
        "  Scheduler        : installed ({})",
        post_setup.scheduler_path.display()
    );
    println!();
    println!("Run 'distill scan --now' to start your first scan.");
    println!("Run 'distill review' to review pending proposals.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Interval, NotificationPref, ShellType};

    // --- detect_agents ---

    #[test]
    fn test_detect_agents_none_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Neither .claude nor .codex exists.
        let detected = detect_agents(&home);
        assert_eq!(
            detected.len(),
            2,
            "should report an entry for every known agent"
        );
        for (_, installed) in &detected {
            assert!(!installed, "no agent should be detected");
        }
    }

    #[test]
    fn test_detect_agents_claude_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let detected = detect_agents(&home);
        let claude = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Claude)
            .unwrap();
        let codex = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Codex)
            .unwrap();
        assert!(claude.1, "Claude should be detected");
        assert!(!codex.1, "Codex should not be detected");
    }

    #[test]
    fn test_detect_agents_codex_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let claude = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Claude)
            .unwrap();
        let codex = detected
            .iter()
            .find(|(k, _)| *k == AgentKind::Codex)
            .unwrap();
        assert!(!claude.1, "Claude should not be detected");
        assert!(codex.1, "Codex should be detected");
    }

    #[test]
    fn test_detect_agents_both_installed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        assert_eq!(detected.len(), 2);
        for (_, installed) in &detected {
            assert!(installed, "both agents should be detected");
        }
    }

    // --- build_config: basic field mapping ---

    #[test]
    fn test_build_config_default_answers() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert_eq!(config.proposal_agent, "claude");
        assert_eq!(config.shell, ShellType::Zsh);
        assert_eq!(config.notifications, NotificationPref::Both);
    }

    // --- build_config: default interval is weekly ---

    #[test]
    fn test_build_config_default_interval_is_weekly() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::default(), // should be Weekly
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Weekly);
    }

    // --- build_config: only selected agents are enabled ---

    #[test]
    fn test_build_config_enables_only_selected_agents() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude], // only Claude enabled
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Bash,
            notifications: NotificationPref::Terminal,
        };

        let config = build_config(&answers);
        let claude_entry = config.agents.iter().find(|a| a.name == "claude").unwrap();
        let codex_entry = config.agents.iter().find(|a| a.name == "codex").unwrap();
        assert!(claude_entry.enabled, "Claude should be enabled");
        assert!(!codex_entry.enabled, "Codex should be disabled");
    }

    // --- build_config: agent entries match detected agents ---

    #[test]
    fn test_build_config_agent_entries_match_detected() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        // Only Claude dir exists, but user enables both.
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(
            config.agents.len(),
            2,
            "should produce one entry per detected agent"
        );
        let claude = config.agents.iter().find(|a| a.name == "claude").unwrap();
        let codex = config.agents.iter().find(|a| a.name == "codex").unwrap();
        assert!(claude.enabled);
        assert!(codex.enabled);
    }

    // --- build_config: various interval and notification combinations ---

    #[test]
    fn test_build_config_daily_interval() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::Daily,
            proposal_agent: AgentKind::Claude,
            shell: ShellType::Zsh,
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Daily);
    }

    #[test]
    fn test_build_config_monthly_interval_and_native_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Codex],
            scan_interval: Interval::Monthly,
            proposal_agent: AgentKind::Codex,
            shell: ShellType::Fish,
            notifications: NotificationPref::Native,
        };

        let config = build_config(&answers);
        assert_eq!(config.scan_interval, Interval::Monthly);
        assert_eq!(config.proposal_agent, "codex");
        assert_eq!(config.shell, ShellType::Fish);
        assert_eq!(config.notifications, NotificationPref::Native);
    }

    // --- build_config: shell detection is used ---

    #[test]
    fn test_build_config_shell_detection_used() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());

        let detected_shell = ShellType::detect();
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Claude,
            shell: detected_shell.clone(),
            notifications: NotificationPref::Both,
        };

        let config = build_config(&answers);
        assert_eq!(config.shell, detected_shell);
    }

    // --- build_config: proposal agent is codex ---

    #[test]
    fn test_build_config_proposal_agent_codex() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        let detected = detect_agents(&home);
        let answers = OnboardingAnswers {
            detected_agents: detected,
            enabled_agents: vec![AgentKind::Claude, AgentKind::Codex],
            scan_interval: Interval::Weekly,
            proposal_agent: AgentKind::Codex,
            shell: ShellType::Bash,
            notifications: NotificationPref::None,
        };

        let config = build_config(&answers);
        assert_eq!(config.proposal_agent, "codex");
        assert_eq!(config.notifications, NotificationPref::None);
    }

    #[test]
    fn test_step_sequence_hides_hook_for_other_shell() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());
        let mut state = OnboardingUiState::new(detected);
        state.shell_cursor = shell_to_index(&ShellType::Other);

        let steps = state.step_sequence();
        assert!(!steps.contains(&OnboardingStep::Hook));
        assert_eq!(state.progress(), (1, 6));
    }

    #[test]
    fn test_jump_to_step_number_with_other_shell_skips_hook_slot() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());
        let mut state = OnboardingUiState::new(detected);
        state.shell_cursor = shell_to_index(&ShellType::Other);

        assert!(state.jump_to_step_number(5));
        assert_eq!(state.step, OnboardingStep::Notifications);
        assert!(state.jump_to_step_number(6));
        assert_eq!(state.step, OnboardingStep::Confirm);
        assert!(!state.jump_to_step_number(7));
    }

    #[test]
    fn test_confirm_action_cycles_in_expected_order() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());
        let mut state = OnboardingUiState::new(detected);

        state.step = OnboardingStep::Confirm;
        state.confirm_action = ConfirmAction::Save;
        state.cycle_confirm_action_next();
        assert_eq!(state.confirm_action, ConfirmAction::Back);
        state.cycle_confirm_action_next();
        assert_eq!(state.confirm_action, ConfirmAction::Cancel);
        state.cycle_confirm_action_next();
        assert_eq!(state.confirm_action, ConfirmAction::Save);
    }

    #[test]
    fn test_jump_to_confirm_resets_action_to_save() {
        let dir = tempfile::tempdir().unwrap();
        let detected = detect_agents(dir.path());
        let mut state = OnboardingUiState::new(detected);

        state.confirm_action = ConfirmAction::Cancel;
        assert!(state.jump_to_step_number(state.step_sequence().len()));
        assert_eq!(state.step, OnboardingStep::Confirm);
        assert_eq!(state.confirm_action, ConfirmAction::Save);
    }

    // --- post-onboarding setup side effects ---

    #[test]
    fn test_apply_post_onboarding_setup_installs_hook_and_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Zsh,
            scan_interval: Interval::Weekly,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, true).unwrap();
        assert_eq!(result.hook_status, Some(HookStatus::Installed));
        assert!(
            home.join(".zshrc").exists(),
            "expected .zshrc to be created"
        );
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }

    #[test]
    fn test_apply_post_onboarding_setup_skips_hook_when_declined() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Bash,
            scan_interval: Interval::Weekly,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, false).unwrap();
        assert_eq!(result.hook_status, None);
        assert!(
            !home.join(".bashrc").exists(),
            "expected .bashrc to remain untouched when hook is skipped"
        );
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }

    #[test]
    fn test_apply_post_onboarding_setup_reports_unsupported_shell() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let config = Config {
            shell: ShellType::Other,
            scan_interval: Interval::Daily,
            ..Config::default()
        };

        let result = apply_post_onboarding_setup(&config, home, true).unwrap();
        assert_eq!(result.hook_status, Some(HookStatus::Unsupported));
        assert!(
            result.scheduler_path.exists(),
            "expected scheduler file to be installed"
        );
    }
}
