//! Application state for the TUI shell.
//!
//! [`App::update`] changes state and returns [`UiCommand`]s. It does not draw
//! and it does not touch the clipboard. The Hosts screen handles its own keys;
//! anything it does not recognise (quit, help, copy) is handled here.
//! Other tabs are placeholders until their phases.

use crate::config::{Config, HealthConfig};
use crate::domain::health::{HealthStatus, now_secs};
use crate::domain::host::{EffectiveHost, Host, Inventory, Problem};
use crate::error::Error;
use crate::infra::remote_fs::{DirEntry, join, normalize};
use crate::infra::ssh::command::{
    SshCommandOptions, build_ssh_command, command_line, exec_arguments,
};
use crate::infra::ssh::system::{self, missing_ssh_message};
use crate::infra::ssh_config;
use crate::infra::state_store::{self, AppState};
use crate::paths::Paths;
use crate::services::connect::{self, ConnectPlan};
use crate::services::exec::{self as job, ExecSettings};
use crate::services::files::TransferEvent;
use crate::services::health::{
    self as health_svc, HealthControl, HealthEvent, HealthSettings, HostHealth,
};
use crate::services::inventory::{self, InventoryBundle};
use crate::infra::pty::{KeyModes, encode_key, encode_paste};
use crate::services::sessions::{SessionEvent, SessionId};
use crate::tui::action::{
    Action, ConnectHandoff, EditorHandoff, FilesCommand, Handoff, SessionPlace, SessionsCommand,
    UiCommand,
};
use crate::tui::keymap::{self, Keymap};
use crate::tui::screens::exec as exec_ui;
use crate::tui::screens::exec::{Confirm, Prompt, PromptEvent, Results, ResultsEvent};
use crate::tui::screens::files as files_ui;
use crate::tui::screens::files::{FilesEvent, NameKind, PaneFocus, Viewer};
use crate::tui::screens::health as health_ui;
use crate::tui::screens::health::{HealthEvent as HealthKey, HealthView};
use crate::tui::screens::host_form::{self, HostFormEvent};
use crate::tui::screens::hosts::{self, HostsView, ViewOptions};
use crate::tui::screens::import::{self, ImportEvent};
use crate::tui::screens::sessions as sessions_ui;
use crate::tui::screens::sessions::{SessionKey, SessionsView};
use crate::tui::theme::Theme;
use chrono::Utc;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

const TABS: [&str; 5] = ["Hosts", "Health", "Files", "Sessions", "Log"];

/// Which popup is on top of the Hosts screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Key list. Also includes validation problems when there are any.
    Help,
    /// The validation problems from the last load.
    Problems,
    /// A failure the user has to dismiss, such as ssh exiting 255.
    Error(String),
    /// Add, edit, or duplicate a host.
    HostForm(Box<host_form::HostForm>),
    /// Confirm deletion of one host.
    Delete { name: String },
    /// Checklist of hosts found in an OpenSSH config.
    Import(Box<import::ImportView>),
    /// Quit while embedded sessions are still live.
    QuitSessions,
}

/// Shell state: inventory, keymap, the Hosts screen, and the open modal.
pub struct App {
    inventory: Inventory,
    hosts: Vec<EffectiveHost>,
    problems: Vec<Problem>,
    hosts_path: PathBuf,
    theme: Theme,
    show_detail: bool,
    multiplexing: bool,
    control_dir: PathBuf,
    screen: u8,
    keymap: Keymap,
    view: HostsView,
    modal: Option<Modal>,
    toast: Option<(String, u8)>,
    quit: bool,
    /// Set when something visible changed. The event loop skips redraws otherwise.
    pub dirty: bool,
    terminal_width: u16,
    terminal_height: u16,
    mouse: bool,
    frame: Duration,
    state: AppState,
    state_path: PathBuf,
    ssh: Option<PathBuf>,
    parallel: usize,
    confirm_threshold: usize,
    protected_tags: Vec<String>,
    exec_prompt: Option<Prompt>,
    exec_confirm: Option<Confirm>,
    exec_results: Option<Results>,
    exec_visible: bool,
    health: HealthView,
    health_config: HealthConfig,
    health_history: usize,
    health_refresh: Duration,
    pub(crate) files: files_ui::FilesView,
    /// Embedded sessions UI (PTY children live in the event loop).
    pub(crate) sessions: SessionsView,
    /// Prefix chord from config (`ctrl-]` by default).
    session_prefix: (KeyCode, KeyModifiers),
    /// Scrollback lines for new sessions.
    pub(crate) session_scrollback: usize,
}

impl App {
    /// Build the shell from a loaded inventory and config.
    ///
    /// `no_color` is true when `--no-color` was passed or `NO_COLOR` is set.
    pub fn new(
        bundle: InventoryBundle,
        config: Config,
        paths: Paths,
        no_color: bool,
    ) -> Result<Self, Error> {
        let theme = Theme::from_config(&config.theme, no_color)?;
        let keymap = Keymap::from_config(&config.keys)?;
        let view = HostsView::new(&bundle.hosts);
        let files = files_ui::FilesView::new(files_home());
        let frame = Duration::from_millis(1000 / u64::from(config.tui.fps.max(1)));
        let state_path = paths.state_file();
        let (state, warning) = state_store::load(&state_path);
        let ssh = system::find_program("ssh", std::env::var_os("PATH").as_deref());
        let mut app = Self {
            inventory: bundle.inventory.clone(),
            hosts: bundle.hosts,
            problems: bundle.problems,
            hosts_path: paths.hosts_file(),
            theme,
            show_detail: config.tui.show_detail_pane,
            multiplexing: config.multiplexing,
            control_dir: paths.control_dir(),
            screen: 1,
            keymap,
            view,
            modal: None,
            toast: None,
            quit: false,
            dirty: true,
            terminal_width: 80,
            terminal_height: 24,
            mouse: config.tui.mouse,
            frame,
            state,
            state_path,
            ssh,
            parallel: config.parallel.max(1),
            confirm_threshold: config.confirm_threshold,
            protected_tags: config.protected_tags.clone(),
            exec_prompt: None,
            exec_confirm: None,
            exec_results: None,
            exec_visible: false,
            health: HealthView::new(config.health.refresh_seconds.max(5)),
            health_config: config.health.clone(),
            health_history: config.health.history_samples.max(2),
            health_refresh: Duration::from_secs(config.health.refresh_seconds.max(5)),
            files,
            sessions: SessionsView::new(),
            session_prefix: keymap::parse_chord(&config.sessions.prefix_key)
                .unwrap_or((KeyCode::Char(']'), KeyModifiers::CONTROL)),
            session_scrollback: config.sessions.scrollback_lines.max(1),
        };
        app.seed_health_cache();
        app.refresh_groups();
        app.apply_start_screen(&config.tui.start_screen);
        if app.ssh.is_none() {
            app.modal = Some(Modal::Error(missing_ssh_message().to_string()));
        }
        if let Some(warning) = warning {
            app.toast(warning);
        }
        Ok(app)
    }

    /// Whether setup should enable mouse capture.
    pub fn mouse(&self) -> bool {
        self.mouse
    }

    /// Delay between redraws, from `config.toml`'s `fps`.
    pub fn frame_time(&self) -> Duration {
        self.frame
    }

    /// True after `q` or Ctrl-C.
    pub fn quit(&self) -> bool {
        self.quit
    }

    /// Translate one key into an [`Action`] and apply it.
    ///
    /// Ctrl-C always quits. While the search box is open, characters edit the
    /// query instead of running table commands. Esc during search clears the
    /// filter; Esc otherwise clears the multi-selection.
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.force_quit();
        }
        if self.exec_confirm.is_some() {
            return self.exec_confirm_key(code);
        }
        if self.exec_prompt.is_some() {
            return self.exec_prompt_key(code, modifiers);
        }
        if self.modal.is_some() {
            if matches!(self.modal, Some(Modal::HostForm(_))) {
                return self.host_form_key(code, modifiers);
            }
            if matches!(self.modal, Some(Modal::Import(_))) {
                return self.import_key(code, modifiers);
            }
            if matches!(self.modal, Some(Modal::Delete { .. })) {
                return self.delete_key(code);
            }
            if matches!(self.modal, Some(Modal::QuitSessions)) {
                return self.quit_sessions_key(code);
            }
            if code == KeyCode::Esc {
                return self.update(Action::CloseModal);
            }
            return match self.keymap.lookup(code, modifiers) {
                Some(action @ (Action::Quit | Action::ToggleHelp | Action::ToggleProblems)) => {
                    self.update(action)
                }
                _ => Vec::new(),
            };
        }
        if self.exec_visible {
            return self.exec_results_key(code);
        }
        if self.screen == 4 && self.modal.is_none() {
            let session_cmds = self.sessions_key(code, modifiers);
            if !session_cmds.is_empty() || self.sessions.focused || self.sessions.prefix_until.is_some()
            {
                return session_cmds;
            }
        }
        if self.screen == 2 && self.modal.is_none() {
            return self.health_key(code, modifiers);
        }
        if self.screen == 3 && self.modal.is_none() && !self.exec_visible {
            return self.files_key(code, modifiers);
        }
        if self.screen == 1 && self.view.editing && self.modal.is_none() {
            return match code {
                KeyCode::Esc => self.update(Action::SearchClear),
                KeyCode::Enter => self.update(Action::SearchSubmit),
                KeyCode::Backspace => self.update(Action::SearchBackspace),
                KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    self.update(Action::SearchInsert(ch))
                }
                _ => Vec::new(),
            };
        }
        match self.keymap.lookup(code, modifiers) {
            Some(action) => self.update(action),
            None => Vec::new(),
        }
    }

    /// Apply `action` and return any side effects the event loop should run.
    pub fn update(&mut self, action: Action) -> Vec<UiCommand> {
        self.dirty = true;
        if self.screen == 1 && self.view.handle(&action, &self.hosts) {
            return Vec::new();
        }
        match action {
            Action::Quit => {
                if self.files.queue.active() {
                    self.toast("transfers running — press C to cancel transfers, then q to quit");
                    return Vec::new();
                }
                if self.sessions.has_live() {
                    self.modal = Some(Modal::QuitSessions);
                    return Vec::new();
                }
                return self.finish_quit();
            }
            Action::Tick => {
                if let Some(results) = &mut self.exec_results
                    && results.running
                {
                    results.spinner = results.spinner.wrapping_add(1);
                }
                if self.sessions.tick_prefix() {
                    self.dirty = true;
                }
                if let Some((_, left)) = &mut self.toast {
                    if *left == 0 {
                        self.toast = None;
                    } else {
                        *left -= 1;
                    }
                } else if !self.sessions.focused {
                    self.dirty = false;
                }
            }
            Action::ToggleHelp => {
                self.modal = match self.modal {
                    Some(Modal::Help) => None,
                    _ => Some(Modal::Help),
                };
            }
            Action::ToggleProblems => {
                self.modal = match self.modal {
                    Some(Modal::Problems) => None,
                    _ => Some(Modal::Problems),
                };
            }
            Action::CloseModal => self.modal = None,
            Action::SwitchScreen(screen) => {
                if (1..=5).contains(&screen) {
                    self.screen = screen;
                    if screen == 3 {
                        return vec![UiCommand::Files(FilesCommand::Refresh {
                            local: true,
                            remote: self.files.host.is_some(),
                        })];
                    }
                }
            }
            Action::OpenFiles => {
                self.screen = 3;
                let Some(name) = self.active_cursor_name() else {
                    self.toast("No host selected");
                    return vec![UiCommand::Files(FilesCommand::Refresh {
                        local: true,
                        remote: false,
                    })];
                };
                self.files.host = Some(name.clone());
                return vec![
                    UiCommand::Files(FilesCommand::Disconnect),
                    UiCommand::Files(FilesCommand::Connect(name)),
                    UiCommand::Files(FilesCommand::Refresh {
                        local: true,
                        remote: true,
                    }),
                ];
            }
            Action::OpenSession => return self.open_sessions(),
            Action::CopySshCommand => return self.copy_command(),
            Action::Connect => return self.connect_command(),
            Action::ToggleFavorite => return self.toggle_favorite(),
            Action::ToggleSort => {
                self.view.sort_frecency = !self.view.sort_frecency;
                self.toast(if self.view.sort_frecency {
                    "sorted by frecency"
                } else {
                    "sorted in inventory order"
                });
            }
            Action::EditInventory => return self.edit_inventory(),
            Action::AddHost => self.open_add(),
            Action::EditHost => self.open_edit(),
            Action::DuplicateHost => self.open_duplicate(),
            Action::DeleteHost => self.open_delete(),
            Action::ImportHosts => self.open_import(),
            Action::Exec => self.open_exec(),
            Action::ShowExec => self.show_exec(),
            Action::FocusTree
            | Action::FocusTable
            | Action::MoveUp
            | Action::MoveDown
            | Action::MoveTop
            | Action::MoveBottom
            | Action::PageUp
            | Action::PageDown
            | Action::OpenSearch
            | Action::SearchInsert(_)
            | Action::SearchBackspace
            | Action::SearchSubmit
            | Action::SearchClear
            | Action::ToggleSelect
            | Action::SelectAll
            | Action::ClearSelection => {}
        }
        Vec::new()
    }

    /// Remember the terminal size. A narrow terminal drops focus back to the table.
    pub fn note_size(&mut self, width: u16, height: u16) {
        self.terminal_width = width;
        self.terminal_height = height;
        if width < 60 {
            self.view.focus = hosts::Focus::Table;
        }
        self.dirty = true;
    }

    /// Resize commands for panes that changed size after a layout or terminal resize.
    pub fn session_resize_commands(&mut self) -> Vec<UiCommand> {
        let area = self.session_content_area();
        self.sessions
            .take_resizes(area)
            .into_iter()
            .map(|(id, rows, cols)| UiCommand::Sessions(SessionsCommand::Resize { id, rows, cols }))
            .collect()
    }

    fn session_content_area(&self) -> ratatui::layout::Rect {
        let height = self.terminal_height.saturating_sub(2);
        // Sub-tab bar uses one row inside the content area.
        ratatui::layout::Rect::new(0, 0, self.terminal_width.max(1), height.max(1))
    }

    /// Show `message` on the hint line for about two seconds.
    pub fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), 8));
        self.dirty = true;
    }

    /// Draw the tab bar, the active screen, the hint line, and a modal if one is open.
    ///
    /// `session_manager` supplies vt100 screens when the Sessions tab is visible.
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        session_manager: Option<&crate::services::sessions::SessionManager>,
    ) {
        let area = frame.area();
        self.terminal_width = area.width;
        self.terminal_height = area.height;
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        self.draw_header(frame, chunks[0]);
        if self.screen == 1 {
            let show_detail = self.show_detail;
            let terminal_width = self.terminal_width;
            let terminal_height = self.terminal_height;
            let multiplexing = self.multiplexing;
            let theme = self.theme.clone();
            let hosts_path = self.hosts_path.clone();
            let control_dir = self.control_dir.clone();
            let health_status = self.health_status_map();
            let options = ViewOptions {
                theme: &theme,
                hosts_path: &hosts_path,
                show_detail,
                terminal_width,
                terminal_height,
                multiplexing,
                control_dir: &control_dir,
                health: &health_status,
            };
            hosts::draw(frame, chunks[1], &mut self.view, &self.hosts, &options);
        } else if self.screen == 2 {
            health_ui::draw(frame, chunks[1], &mut self.health, &self.hosts, &self.theme);
        } else if self.screen == 3 {
            files_ui::draw(frame, chunks[1], &mut self.files, &self.hosts, &self.theme);
        } else if self.screen == 4 {
            let theme = self.theme.clone();
            sessions_ui::draw(
                frame,
                chunks[1],
                &self.sessions,
                &theme,
                &mut |id, callback| {
                    if let Some(manager) = session_manager {
                        manager.with_parser(id, |parser| {
                            callback(parser.screen());
                        });
                    }
                },
            );
        } else {
            frame.render_widget(Paragraph::new("Log arrives in a later phase."), chunks[1]);
        }
        self.draw_hints(frame, chunks[2]);
        if self.exec_visible
            && let Some(results) = &self.exec_results
        {
            exec_ui::draw_results(frame, area, results, &self.theme);
        }
        if let Some(prompt) = &self.exec_prompt {
            exec_ui::draw_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(confirm) = &self.exec_confirm {
            exec_ui::draw_confirm(frame, area, confirm, &self.theme);
        }
        if let Some(modal) = self.modal.clone() {
            self.draw_modal(frame, area, modal);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![Span::raw(" vcon ")];
        for (index, name) in TABS.iter().enumerate() {
            let label = if index == 3 && self.sessions.tab_count() > 0 {
                format!(" {} {}({}) ", index + 1, name, self.sessions.tab_count())
            } else {
                format!(" {} {} ", index + 1, name)
            };
            let style = if self.screen == index as u8 + 1 {
                self.theme.selected()
            } else {
                ratatui::style::Style::new()
            };
            spans.push(Span::styled(label, style));
        }
        if !self.problems.is_empty() {
            spans.push(Span::styled(
                format!("  ⚠ {} ", self.problems.len()),
                self.theme.warning(),
            ));
        }
        let (_ok, warn, crit, down) = self.health.counts();
        if warn > 0 {
            spans.push(Span::styled(format!(" ⚠{warn} "), self.theme.warning()));
        }
        if crit + down > 0 {
            spans.push(Span::styled(
                format!(" ✖{} ", crit + down),
                self.theme.error(),
            ));
        }
        if self.files.queue.active() {
            let active = self
                .files
                .queue
                .jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        crate::services::files::TransferState::Queued
                            | crate::services::files::TransferState::Running
                    )
                })
                .count();
            spans.push(Span::styled(format!(" ↑{active} "), self.theme.ok()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_hints(&self, frame: &mut Frame, area: Rect) {
        let text = if let Some((message, _)) = &self.toast {
            message.clone()
        } else if self.screen == 2 {
            let pause = if self.health.paused { "  paused" } else { "" };
            format!(
                " ⏎ details   s sort   p pause   r refresh   R all   / filter   c connect   x exec   q quit{pause}"
            )
        } else if self.screen == 3 {
            " Tab panes   ⏎ open   h up   space mark   c copy   m mkdir   r rename   d delete   v view   H host   C cancel   q quit".to_string()
        } else if self.screen == 4 {
            if self.sessions.focused {
                let host = self
                    .sessions
                    .focused_host()
                    .unwrap_or_else(|| "session".into());
                let prefix = format_prefix(self.session_prefix);
                format!(" SESSION  {host}  —  {prefix} for vcon")
            } else if self.sessions.tab_count() == 0 {
                " o on a host opens a session   4 jumps here   q quit".to_string()
            } else {
                " ⏎ focus   o open   q quit".to_string()
            }
        } else {
            let mut hint =
                " ⏎ connect   o session   x exec   a add   e edit   d del   ? help   q quit"
                    .to_string();
            if self
                .exec_results
                .as_ref()
                .is_some_and(|results| results.running && !self.exec_visible)
            {
                let done = self
                    .exec_results
                    .as_ref()
                    .map(|results| {
                        results
                            .rows
                            .iter()
                            .filter(|row| row.outcome.is_some())
                            .count()
                    })
                    .unwrap_or(0);
                let total = self
                    .exec_results
                    .as_ref()
                    .map(|results| results.rows.len())
                    .unwrap_or(0);
                hint = format!(" exec {done}/{total}   {hint}");
            }
            if !self.problems.is_empty() {
                hint.push_str("   v problems");
            }
            hint
        };
        frame.render_widget(Paragraph::new(text).style(self.theme.muted()), area);
    }

    fn draw_modal(&self, frame: &mut Frame, area: Rect, modal: Modal) {
        let (title, lines) = match modal {
            Modal::Help => ("Help", self.help_lines()),
            Modal::Problems => ("Validation", self.problem_lines()),
            Modal::Error(message) => ("Error", vec![Line::from(message)]),
            Modal::HostForm(form) => {
                host_form::draw(frame, area, &form, &self.theme);
                return;
            }
            Modal::Import(view) => {
                import::draw(frame, area, &view, &self.theme);
                return;
            }
            Modal::Delete { name } => (
                "Delete host",
                vec![
                    Line::from(format!("Delete {name}?")),
                    Line::from("y confirms   n or Esc cancels"),
                ],
            ),
            Modal::QuitSessions => (
                "Quit",
                vec![
                    Line::from(format!(
                        "{} live session(s) are still open.",
                        self.sessions
                            .hosts
                            .keys()
                            .filter(|id| !self.sessions.ended.contains_key(id))
                            .count()
                    )),
                    Line::from("y quits and closes them   n or Esc cancels"),
                ],
            ),
        };
        crate::tui::widgets::modal::render(frame, area, title, lines, &self.theme);
    }

    fn help_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from("j/k or arrows    move"),
            Line::from("g / G            top / bottom"),
            Line::from("PgUp / PgDn      page"),
            Line::from("h/l              tag tree / table"),
            Line::from("/                search; Esc clears, Enter keeps it"),
            Line::from("space            toggle select"),
            Line::from("ctrl-a           select visible hosts"),
            Line::from("Esc              clear selection"),
            Line::from("y                copy the ssh command"),
            Line::from("Enter            connect"),
            Line::from("o                open an embedded session"),
            Line::from("f                open files for the cursor host"),
            Line::from("a                add a host"),
            Line::from("e                edit the cursor host"),
            Line::from("D                duplicate the cursor host"),
            Line::from("d                delete the cursor host"),
            Line::from("i                import ~/.ssh/config"),
            Line::from("x                run a command on the selection"),
            Line::from("X                reopen exec results"),
            Line::from("*                toggle favorite"),
            Line::from("S                sort by frecency"),
            Line::from("E                edit hosts.toml"),
            Line::from("v                validation problems"),
            Line::from("?                this help"),
            Line::from("q                quit"),
        ];
        if !self.problems.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::styled("Validation problems", self.theme.warning()));
            lines.extend(self.problem_lines());
        }
        lines
    }

    fn problem_lines(&self) -> Vec<Line<'static>> {
        if self.problems.is_empty() {
            return vec![Line::from("No validation problems.")];
        }
        self.problems
            .iter()
            .map(|problem| Line::from(problem.to_string()))
            .collect()
    }

    fn copy_command(&mut self) -> Vec<UiCommand> {
        let multiplexing = self.multiplexing;
        let control = self.control_dir.display().to_string();
        let Some(name) = self.active_cursor_name() else {
            self.toast("No host selected");
            return Vec::new();
        };
        let Some(host) = self.hosts.iter().find(|host| host.name == name) else {
            self.toast("No host selected");
            return Vec::new();
        };
        let line = command_line(&build_ssh_command(
            host,
            &self.hosts,
            &SshCommandOptions {
                batch_mode: false,
                connect_timeout: None,
                multiplexing,
                control_path_dir: multiplexing.then_some(control),
            },
        ));
        vec![UiCommand::Copy(line)]
    }

    fn connect_command(&mut self) -> Vec<UiCommand> {
        let Some(program) = self.ssh.clone() else {
            self.modal = Some(Modal::Error(missing_ssh_message().to_string()));
            return Vec::new();
        };
        let host = if self.screen == 2 {
            self.health
                .cursor_name
                .as_ref()
                .and_then(|name| self.hosts.iter().find(|host| &host.name == name))
                .cloned()
        } else if self.screen == 3 {
            self.files
                .host
                .as_ref()
                .and_then(|name| self.hosts.iter().find(|host| &host.name == name))
                .cloned()
        } else {
            self.view.cursor(&self.hosts).cloned()
        };
        let Some(host) = host else {
            self.toast("No host selected");
            return Vec::new();
        };
        let ConnectPlan {
            host_name,
            banner,
            args,
            ..
        } = connect::plan(&host, &self.hosts, &self.session_options());
        self.state.record_connect(&host_name, Utc::now());
        self.refresh_groups();
        vec![
            UiCommand::Persist,
            UiCommand::Suspend(Handoff::Connect(ConnectHandoff {
                host_name,
                banner,
                program,
                args,
            })),
        ]
    }

    fn toggle_favorite(&mut self) -> Vec<UiCommand> {
        let Some(name) = self.view.cursor_name.clone() else {
            self.toast("No host selected");
            return Vec::new();
        };
        let favorite = self.state.toggle_favorite(&name);
        self.refresh_groups();
        self.toast(if favorite {
            format!("{name} is a favorite")
        } else {
            format!("{name} is no longer a favorite")
        });
        vec![UiCommand::Persist]
    }

    fn edit_inventory(&mut self) -> Vec<UiCommand> {
        let (program, args) = connect::editor_command(&self.hosts_path);
        vec![UiCommand::Suspend(Handoff::Editor(EditorHandoff {
            program,
            args,
        }))]
    }

    fn session_options(&self) -> SshCommandOptions {
        connect::session_options(self.multiplexing, &self.control_dir)
    }

    /// Copy recent, favorite, and frecency data onto the host table.
    pub fn refresh_groups(&mut self) {
        let now = Utc::now();
        let present: BTreeSet<String> = self.hosts.iter().map(|host| host.name.clone()).collect();
        let recent_order = self
            .state
            .recent_names()
            .into_iter()
            .filter(|name| present.contains(name))
            .collect();
        let favorites = self
            .state
            .favorite_names()
            .into_iter()
            .filter(|name| present.contains(name))
            .collect();
        let mut last_seen = BTreeMap::new();
        let mut scores = BTreeMap::new();
        for host in &self.hosts {
            if let Some(label) = self.state.age_label(&host.name, now) {
                last_seen.insert(host.name.clone(), label);
            }
            let score = self.state.score(&host.name, now);
            if score > 0 {
                scores.insert(host.name.clone(), score);
            }
        }
        self.view
            .apply_groups(recent_order, favorites, last_seen, scores);
    }

    fn apply_start_screen(&mut self, start_screen: &str) {
        let saved = start_screen == "last";
        let name = if saved {
            self.state.ui.last_screen.as_str()
        } else {
            start_screen
        };
        if saved && !self.state.ui.last_filter.is_empty() {
            self.view.query = self.state.ui.last_filter.clone();
            self.view.refresh(&self.hosts);
        }
        if let Some(screen) = screen_index(name) {
            self.screen = screen;
        }
    }

    /// Reload `hosts.toml` after the editor closes.
    pub fn reload_inventory(&mut self) {
        match inventory::load_hosts(&self.hosts_path, &crate::paths::local_user()) {
            Ok(bundle) => {
                let problems = bundle.problems.len();
                self.inventory = bundle.inventory;
                self.hosts = bundle.hosts;
                self.problems = bundle.problems;
                self.view
                    .selected
                    .retain(|name| self.hosts.iter().any(|host| &host.name == name));
                self.refresh_groups();
                self.view.refresh(&self.hosts);
                if problems == 0 {
                    self.toast("reloaded hosts.toml");
                } else {
                    self.toast(format!("reloaded hosts.toml with {problems} problems"));
                }
            }
            Err(error) => self.modal = Some(Modal::Error(error.to_string())),
        }
        self.dirty = true;
    }

    fn open_add(&mut self) {
        if self.screen != 1 && self.screen != 2 && self.screen != 3 {
            return;
        }
        let form = host_form::add_form(&self.inventory, &crate::paths::local_user());
        self.modal = Some(Modal::HostForm(Box::new(form)));
    }

    fn open_edit(&mut self) {
        if self.screen != 1 && self.screen != 2 && self.screen != 3 {
            return;
        }
        let Some(host) = self.raw_host().cloned() else {
            self.toast("No host selected");
            return;
        };
        let form = host_form::edit_form(&host, &self.inventory, &crate::paths::local_user());
        self.modal = Some(Modal::HostForm(Box::new(form)));
    }

    fn open_duplicate(&mut self) {
        if self.screen != 1 && self.screen != 2 && self.screen != 3 {
            return;
        }
        let Some(host) = self.raw_host().cloned() else {
            self.toast("No host selected");
            return;
        };
        let form = host_form::duplicate_form(&host, &self.inventory, &crate::paths::local_user());
        self.modal = Some(Modal::HostForm(Box::new(form)));
    }

    fn open_delete(&mut self) {
        if self.screen != 1 && self.screen != 2 && self.screen != 3 {
            return;
        }
        let Some(name) = self.active_cursor_name() else {
            self.toast("No host selected");
            return;
        };
        let blockers = crate::domain::host_form::deletion_blockers(&self.inventory, &name);
        if blockers.is_empty() {
            self.modal = Some(Modal::Delete { name });
        } else {
            self.modal = Some(Modal::Error(format!(
                "cannot delete {name}: used as a jump by {}",
                blockers.join(", ")
            )));
        }
    }

    fn open_import(&mut self) {
        if self.screen != 1 && self.screen != 2 && self.screen != 3 {
            return;
        }
        let path = ssh_config::default_path();
        if !path.exists() {
            self.modal = Some(Modal::Error(format!("no ssh config at {}", path.display())));
            return;
        }
        self.modal = Some(Modal::Import(Box::new(import::load(
            &path,
            &self.inventory,
        ))));
    }

    fn active_cursor_name(&self) -> Option<String> {
        if self.screen == 2 {
            self.health.cursor_name.clone()
        } else if self.screen == 3 {
            self.files
                .host
                .clone()
                .or_else(|| self.view.cursor_name.clone())
        } else {
            self.view.cursor_name.clone()
        }
    }

    fn raw_host(&self) -> Option<&Host> {
        let name = self.active_cursor_name()?;
        self.inventory.hosts.iter().find(|host| host.name == name)
    }

    fn host_form_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        let Some(Modal::HostForm(mut form)) = self.modal.take() else {
            return Vec::new();
        };
        match host_form::on_key(&mut form, &self.inventory, code, modifiers) {
            HostFormEvent::Continue => {
                self.modal = Some(Modal::HostForm(form));
                Vec::new()
            }
            HostFormEvent::Cancel => Vec::new(),
            HostFormEvent::Submit {
                original_name,
                host,
            } => self.save_host(*form, original_name, *host),
        }
    }

    fn save_host(
        &mut self,
        mut form: host_form::HostForm,
        original_name: Option<String>,
        host: Host,
    ) -> Vec<UiCommand> {
        let user = crate::paths::local_user();
        let saved = match &original_name {
            Some(old) => inventory::update_host(&self.hosts_path, &user, old, host.clone()),
            None => inventory::add_host(&self.hosts_path, &user, host.clone()),
        };
        match saved {
            Ok(bundle) => {
                let new_name = host.name.clone();
                if let Some(old) = original_name {
                    if old != new_name {
                        self.state.rename_host(&old, &new_name);
                        if self.view.selected.remove(&old) {
                            self.view.selected.insert(new_name.clone());
                        }
                        if self.view.cursor_name.as_deref() == Some(old.as_str()) {
                            self.view.cursor_name = Some(new_name);
                        }
                    }
                } else {
                    self.view.cursor_name = Some(new_name);
                }
                self.install_bundle(bundle);
                self.toast(format!("saved {}", host.name));
                vec![UiCommand::Persist]
            }
            Err(error) => {
                form.status = Some(error.to_string());
                self.modal = Some(Modal::HostForm(Box::new(form)));
                Vec::new()
            }
        }
    }

    fn import_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        let Some(Modal::Import(mut view)) = self.modal.take() else {
            return Vec::new();
        };
        match import::on_key(&mut view, code, modifiers) {
            ImportEvent::Continue => {
                self.modal = Some(Modal::Import(view));
                Vec::new()
            }
            ImportEvent::Cancel => Vec::new(),
            ImportEvent::Import(hosts) if hosts.is_empty() => {
                self.toast("nothing selected");
                self.modal = Some(Modal::Import(view));
                Vec::new()
            }
            ImportEvent::Import(hosts) => {
                match inventory::import_hosts(
                    &self.hosts_path,
                    &crate::paths::local_user(),
                    &hosts,
                    true,
                ) {
                    Ok(bundle) => {
                        let count = hosts.len();
                        self.install_bundle(bundle);
                        self.toast(format!("imported {count} hosts"));
                        vec![UiCommand::Persist]
                    }
                    Err(error) => {
                        self.modal = Some(Modal::Error(error.to_string()));
                        Vec::new()
                    }
                }
            }
        }
    }

    fn delete_key(&mut self, code: KeyCode) -> Vec<UiCommand> {
        let Some(Modal::Delete { name }) = self.modal.clone() else {
            return Vec::new();
        };
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.modal = None;
                match inventory::remove_host(&self.hosts_path, &crate::paths::local_user(), &name) {
                    Ok(bundle) => {
                        self.state.forget_host(&name);
                        self.view.selected.remove(&name);
                        if self.view.cursor_name.as_deref() == Some(name.as_str()) {
                            self.view.cursor_name = None;
                        }
                        self.install_bundle(bundle);
                        self.toast(format!("deleted {name}"));
                        vec![UiCommand::Persist]
                    }
                    Err(error) => {
                        self.modal = Some(Modal::Error(error.to_string()));
                        Vec::new()
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.modal = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn install_bundle(&mut self, bundle: InventoryBundle) {
        self.inventory = bundle.inventory;
        self.hosts = bundle.hosts;
        self.problems = bundle.problems;
        self.view
            .selected
            .retain(|name| self.hosts.iter().any(|host| &host.name == name));
        self.refresh_groups();
        self.view.refresh(&self.hosts);
        let cursor_missing = self
            .view
            .cursor_name
            .as_ref()
            .is_none_or(|name| !self.hosts.iter().any(|host| &host.name == name));
        if cursor_missing {
            self.view.cursor_name = self.hosts.first().map(|host| host.name.clone());
        }
    }

    fn open_exec(&mut self) {
        if self.screen != 1 && self.screen != 2 {
            return;
        }
        if self
            .exec_results
            .as_ref()
            .is_some_and(|results| results.running)
        {
            self.toast("an exec is already running");
            self.exec_visible = true;
            return;
        }
        let targets = self.exec_targets();
        if targets.is_empty() {
            self.toast("No host selected");
            return;
        }
        self.exec_prompt = Some(Prompt::new(
            targets.into_iter().map(|host| host.name).collect(),
            self.state.ui.exec_history.clone(),
        ));
        self.exec_confirm = None;
    }

    fn show_exec(&mut self) {
        if self.exec_results.is_some() {
            self.exec_visible = true;
        } else {
            self.toast("no exec to show");
        }
    }

    fn exec_targets(&self) -> Vec<EffectiveHost> {
        if self.screen == 2 {
            return self
                .health
                .cursor_name
                .as_ref()
                .and_then(|name| self.hosts.iter().find(|host| &host.name == name))
                .cloned()
                .into_iter()
                .collect();
        }
        if !self.view.selected.is_empty() {
            return self
                .hosts
                .iter()
                .filter(|host| self.view.selected.contains(&host.name))
                .cloned()
                .collect();
        }
        self.view.cursor(&self.hosts).cloned().into_iter().collect()
    }

    fn exec_prompt_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        let Some(mut prompt) = self.exec_prompt.take() else {
            return Vec::new();
        };
        match exec_ui::prompt_on_key(&mut prompt, code, modifiers) {
            PromptEvent::Continue => {
                self.exec_prompt = Some(prompt);
                Vec::new()
            }
            PromptEvent::Cancel => Vec::new(),
            PromptEvent::Submit {
                command,
                sudo,
                timeout,
            } => self.stage_exec(command, sudo, timeout),
        }
    }

    fn exec_confirm_key(&mut self, code: KeyCode) -> Vec<UiCommand> {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(confirm) = self.exec_confirm.take() else {
                    return Vec::new();
                };
                self.start_exec(
                    confirm.command,
                    confirm.sudo,
                    confirm.timeout,
                    confirm.hosts,
                )
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.exec_confirm = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn exec_results_key(&mut self, code: KeyCode) -> Vec<UiCommand> {
        let Some(results) = self.exec_results.as_mut() else {
            self.exec_visible = false;
            return Vec::new();
        };
        let event = exec_ui::results_on_key(results, code);
        match event {
            ResultsEvent::Continue => Vec::new(),
            ResultsEvent::Close => {
                self.exec_visible = false;
                Vec::new()
            }
            ResultsEvent::Cancel => vec![UiCommand::CancelExec],
            ResultsEvent::Copy(text) => vec![UiCommand::Copy(text)],
            ResultsEvent::Rerun(names) => {
                if self
                    .exec_results
                    .as_ref()
                    .is_some_and(|results| results.running)
                {
                    self.toast("an exec is already running");
                    return Vec::new();
                }
                let Some(results) = self.exec_results.clone() else {
                    return Vec::new();
                };
                let hosts = self
                    .hosts
                    .iter()
                    .filter(|host| names.iter().any(|name| name == &host.name))
                    .cloned()
                    .collect();
                self.start_exec(results.command, results.sudo, results.timeout, hosts)
            }
        }
    }

    fn stage_exec(&mut self, command: String, sudo: bool, timeout: Duration) -> Vec<UiCommand> {
        let hosts = self.exec_targets();
        if hosts.is_empty() {
            self.toast("No host selected");
            self.exec_prompt = None;
            return Vec::new();
        }
        let settings = ExecSettings {
            confirm_threshold: self.confirm_threshold,
            protected_tags: self.protected_tags.clone(),
        };
        if let Some(reason) = job::needs_confirmation(&hosts, &settings) {
            self.exec_prompt = None;
            self.exec_confirm = Some(Confirm {
                command,
                sudo,
                timeout,
                hosts,
                reason: reason.message(),
            });
            return Vec::new();
        }
        self.start_exec(command, sudo, timeout, hosts)
    }

    fn start_exec(
        &mut self,
        command: String,
        sudo: bool,
        timeout: Duration,
        hosts: Vec<EffectiveHost>,
    ) -> Vec<UiCommand> {
        let Some(program) = self.ssh.clone() else {
            self.show_error(missing_ssh_message().to_string());
            self.exec_prompt = None;
            self.exec_confirm = None;
            return Vec::new();
        };
        if hosts.is_empty() {
            self.toast("No host selected");
            return Vec::new();
        }
        let remote = job::remote_command(&command, sudo);
        let connect_timeout = (timeout.as_secs() > 0).then_some(timeout.as_secs());
        let control = self.control_dir.display().to_string();
        let names: Vec<String> = hosts.iter().map(|host| host.name.clone()).collect();
        let targets = hosts
            .iter()
            .map(|host| job::HostTarget {
                name: host.name.clone(),
                args: exec_arguments(
                    host,
                    &self.hosts,
                    connect_timeout,
                    self.multiplexing,
                    Some(&control),
                    &remote,
                ),
            })
            .collect();
        job::push_history(&mut self.state.ui.exec_history, &command);
        self.exec_results = Some(Results::starting(command, sudo, timeout, &names));
        self.exec_visible = true;
        self.exec_prompt = None;
        self.exec_confirm = None;
        vec![
            UiCommand::StartExec(job::ExecLaunch {
                program,
                targets,
                timeout,
                parallel: self.parallel,
            }),
            UiCommand::Persist,
        ]
    }

    /// Fold one exec event into the results view.
    pub fn apply_exec_event(&mut self, event: job::JobEvent) {
        if let Some(results) = &mut self.exec_results {
            results.apply(event);
        }
        self.dirty = true;
    }

    /// True while the results popup is on screen. The clipboard toast uses this.
    pub fn exec_visible(&self) -> bool {
        self.exec_visible
    }

    /// Fold one health update into the table and the state-file cache.
    pub fn apply_health_event(&mut self, event: HealthEvent) {
        let mut report = event.report;
        health_svc::mark_stale(&mut report, now_secs(), self.health_refresh * 2);
        self.health.apply(
            event.host.clone(),
            report.clone(),
            event.point,
            self.health_history,
        );
        self.state.remember_health(&event.host, report);
        self.dirty = true;
    }

    /// Targets, settings, and cache used to start the background scheduler.
    pub fn health_bootstrap(
        &self,
    ) -> Option<(
        Vec<job::HostTarget>,
        HealthSettings,
        BTreeMap<String, HostHealth>,
    )> {
        let program = self.ssh.clone()?;
        let settings = HealthSettings::from_config(&self.health_config, self.parallel, program);
        let targets =
            health_svc::health_targets(&self.hosts, None, self.multiplexing, &self.control_dir);
        Some((targets, settings, self.health.hosts.clone()))
    }

    fn health_status_map(&self) -> BTreeMap<String, HealthStatus> {
        self.health
            .hosts
            .iter()
            .filter_map(|(name, host)| {
                host.report
                    .as_ref()
                    .map(|report| (name.clone(), report.status))
            })
            .collect()
    }

    fn seed_health_cache(&mut self) {
        let mut cache = BTreeMap::new();
        for (name, record) in &self.state.hosts {
            if let Some(mut report) = record.last_health.clone() {
                report.stale = true;
                cache.insert(
                    name.clone(),
                    HostHealth {
                        report: Some(report),
                        ..HostHealth::default()
                    },
                );
            }
        }
        self.health.load_cache(cache, &self.hosts);
    }

    fn health_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        if !self.health.editing && self.health.detail.is_none() {
            match code {
                KeyCode::Char('q') => return self.update(Action::Quit),
                KeyCode::Char('?') => return self.update(Action::ToggleHelp),
                KeyCode::Char('v') => return self.update(Action::ToggleProblems),
                KeyCode::Char(digit @ '1'..='5') => {
                    let screen = digit.to_digit(10).unwrap_or(0) as u8;
                    return self.update(Action::SwitchScreen(screen));
                }
                _ => {}
            }
        }
        match health_ui::on_key(&mut self.health, code, &self.hosts) {
            HealthKey::None => {
                // Hosts actions such as `a` add still work from the Health tab.
                if !self.health.editing
                    && self.health.detail.is_none()
                    && let Some(
                        action @ (Action::AddHost
                        | Action::EditHost
                        | Action::DuplicateHost
                        | Action::DeleteHost
                        | Action::ImportHosts
                        | Action::EditInventory
                        | Action::ToggleFavorite
                        | Action::CopySshCommand
                        | Action::OpenFiles
                        | Action::OpenSession),
                    ) = self.keymap.lookup(code, modifiers)
                {
                    return self.update(action);
                }
                self.dirty = true;
                Vec::new()
            }
            HealthKey::CloseDetail
            | HealthKey::OpenDetail
            | HealthKey::OpenSearch
            | HealthKey::SearchSubmit
            | HealthKey::SearchClear
            | HealthKey::SearchInsert(_)
            | HealthKey::SearchBackspace => {
                self.dirty = true;
                Vec::new()
            }
            HealthKey::TogglePause => {
                self.dirty = true;
                vec![UiCommand::Health(if self.health.paused {
                    HealthControl::Pause
                } else {
                    HealthControl::Resume
                })]
            }
            HealthKey::RefreshOne => {
                self.dirty = true;
                let names = self
                    .health
                    .cursor_name
                    .clone()
                    .into_iter()
                    .collect::<Vec<_>>();
                vec![UiCommand::Health(HealthControl::Refresh(names))]
            }
            HealthKey::RefreshAll => {
                self.dirty = true;
                vec![UiCommand::Health(HealthControl::Refresh(Vec::new()))]
            }
            HealthKey::Connect => self.connect_command(),
            HealthKey::Exec => {
                self.open_exec();
                Vec::new()
            }
        }
    }

    /// Show a modal when ssh could not start a shell.
    pub fn note_session_exit(&mut self, status: &ExitStatus) {
        match status.code() {
            Some(255) => {
                self.modal = Some(Modal::Error(
                    "ssh exited with code 255. The session failed before a shell started."
                        .to_string(),
                ));
            }
            Some(_) => {}
            None => self.toast("ssh was interrupted"),
        }
        self.dirty = true;
    }

    /// Show a modal when the ssh process could not be spawned.
    pub fn note_session_error(&mut self, error: &std::io::Error) {
        self.show_error(format!("could not start ssh: {error}"));
    }

    /// Apply a directory listing to one files pane.
    pub fn apply_files_listing(&mut self, focus: PaneFocus, path: String, entries: Vec<DirEntry>) {
        self.files.set_listing(focus, path, entries);
        self.dirty = true;
    }

    /// Fold one transfer event into the queue panel.
    pub fn apply_transfer_event(&mut self, event: TransferEvent) {
        self.files.queue.apply(event);
        self.dirty = true;
    }

    /// Open the read-only file viewer with loaded text.
    pub fn apply_view_text(&mut self, path: String, text: String) {
        self.files.viewer = Some(Viewer {
            title: path,
            lines: text.lines().map(str::to_string).collect(),
            offset: 0,
        });
        self.dirty = true;
    }

    fn files_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        let overlay = self.files.viewer.is_some()
            || self.files.conflict.is_some()
            || self.files.delete.is_some()
            || self.files.prompt.is_some()
            || self.files.picker.is_some();
        if !overlay && !self.files.focused_ref().filtering {
            match code {
                KeyCode::Char('q') => return self.update(Action::Quit),
                KeyCode::Char('?') => return self.update(Action::ToggleHelp),
                KeyCode::Char('v') => return self.update(Action::ToggleProblems),
                KeyCode::Char(digit @ '1'..='5') => {
                    let screen = digit.to_digit(10).unwrap_or(0) as u8;
                    return self.update(Action::SwitchScreen(screen));
                }
                _ => {}
            }
            if let Some(
                action @ (Action::AddHost
                | Action::EditHost
                | Action::DuplicateHost
                | Action::DeleteHost
                | Action::ImportHosts
                | Action::EditInventory
                | Action::ToggleFavorite
                | Action::CopySshCommand
                | Action::Connect
                | Action::Exec
                | Action::OpenFiles
                | Action::OpenSession),
            ) = self.keymap.lookup(code, modifiers)
            {
                return self.update(action);
            }
        }
        match files_ui::on_key(&mut self.files, code, &self.hosts) {
            FilesEvent::None => {
                self.dirty = true;
                Vec::new()
            }
            FilesEvent::ReloadFocus => {
                let local = self.files.focus == PaneFocus::Local;
                vec![UiCommand::Files(FilesCommand::Refresh {
                    local,
                    remote: !local && self.files.host.is_some(),
                })]
            }
            FilesEvent::ReloadBoth => vec![UiCommand::Files(FilesCommand::Refresh {
                local: true,
                remote: self.files.host.is_some(),
            })],
            FilesEvent::Activate => self.files_activate(),
            FilesEvent::Parent => self.files_parent(),
            FilesEvent::Copy => self.files_copy(),
            FilesEvent::ConfirmDelete => {
                self.dirty = true;
                Vec::new()
            }
            FilesEvent::DoDelete => {
                let Some(delete) = self.files.delete.clone() else {
                    return Vec::new();
                };
                self.files.delete = None;
                vec![UiCommand::Files(FilesCommand::Delete {
                    paths: delete.paths,
                    remote: delete.remote,
                })]
            }
            FilesEvent::SubmitName => self.files_submit_name(),
            FilesEvent::CancelOverlay => {
                self.files.conflict = None;
                self.files.delete = None;
                self.files.prompt = None;
                self.files.picker = None;
                self.dirty = true;
                Vec::new()
            }
            FilesEvent::PickHost => self.files_pick_host(),
            FilesEvent::Conflict(action) => {
                self.files.conflict = None;
                self.dirty = true;
                let _ = action;
                Vec::new()
            }
            FilesEvent::CancelTransfers => vec![UiCommand::Files(FilesCommand::CancelTransfers)],
            FilesEvent::ToggleQueue => {
                self.files.queue_collapsed = !self.files.queue_collapsed;
                self.dirty = true;
                Vec::new()
            }
        }
    }

    fn files_activate(&mut self) -> Vec<UiCommand> {
        let focus = self.files.focus;
        let pane = self.files.focused_ref();
        let Some(entry) = pane.current() else {
            return Vec::new();
        };
        if entry.is_dir() {
            let path = join(&pane.path, &entry.name);
            match focus {
                PaneFocus::Local => self.files.local.path = path.clone(),
                PaneFocus::Remote => self.files.remote.path = path.clone(),
            }
            let local = focus == PaneFocus::Local;
            vec![UiCommand::Files(FilesCommand::Refresh {
                local,
                remote: !local,
            })]
        } else {
            let path = join(&pane.path, &entry.name);
            vec![UiCommand::Files(FilesCommand::View {
                path,
                remote: focus == PaneFocus::Remote,
            })]
        }
    }

    fn files_parent(&mut self) -> Vec<UiCommand> {
        let focus = self.files.focus;
        let parent = files_ui::go_parent(&self.files.focused_ref().path);
        match focus {
            PaneFocus::Local => self.files.local.path = parent,
            PaneFocus::Remote => self.files.remote.path = parent,
        }
        let local = focus == PaneFocus::Local;
        vec![UiCommand::Files(FilesCommand::Refresh {
            local,
            remote: !local && self.files.host.is_some(),
        })]
    }

    fn files_copy(&mut self) -> Vec<UiCommand> {
        let Some(host) = self.files.host.clone() else {
            self.toast("Choose a remote host (H)");
            return Vec::new();
        };
        let sources = self.files.selected_paths();
        if sources.is_empty() {
            self.toast("Nothing selected to copy");
            return Vec::new();
        }
        let dest_dir = self.files.other().path.clone();
        let direction = match self.files.focus {
            PaneFocus::Local => crate::services::files::TransferDirection::Upload,
            PaneFocus::Remote => crate::services::files::TransferDirection::Download,
        };
        vec![UiCommand::Files(FilesCommand::Copy {
            direction,
            sources,
            dest_dir,
            host,
        })]
    }

    fn files_submit_name(&mut self) -> Vec<UiCommand> {
        let Some(prompt) = self.files.prompt.take() else {
            return Vec::new();
        };
        let remote = self.files.focus == PaneFocus::Remote;
        let pane_path = self.files.focused_ref().path.clone();
        match prompt.kind {
            NameKind::Mkdir => {
                let path = join(&pane_path, &prompt.value);
                vec![
                    UiCommand::Files(FilesCommand::Mkdir { path, remote }),
                    UiCommand::Files(FilesCommand::Refresh {
                        local: !remote,
                        remote,
                    }),
                ]
            }
            NameKind::Rename => {
                let Some(entry) = self.files.focused_ref().current() else {
                    return Vec::new();
                };
                let from = join(&pane_path, &entry.name);
                let to = join(&pane_path, &prompt.value);
                vec![
                    UiCommand::Files(FilesCommand::Rename { from, to, remote }),
                    UiCommand::Files(FilesCommand::Refresh {
                        local: !remote,
                        remote,
                    }),
                ]
            }
            NameKind::Goto => {
                let path = normalize(&prompt.value);
                match self.files.focus {
                    PaneFocus::Local => self.files.local.path = path.clone(),
                    PaneFocus::Remote => self.files.remote.path = path.clone(),
                }
                vec![UiCommand::Files(FilesCommand::Refresh {
                    local: !remote,
                    remote,
                })]
            }
        }
    }

    fn files_pick_host(&mut self) -> Vec<UiCommand> {
        let Some(picker) = self.files.picker.take() else {
            return Vec::new();
        };
        let query = picker.query.to_ascii_lowercase();
        let names: Vec<String> = self
            .hosts
            .iter()
            .map(|host| host.name.clone())
            .filter(|name| query.is_empty() || name.to_ascii_lowercase().contains(&query))
            .collect();
        let Some(name) = names.get(picker.cursor).cloned() else {
            self.toast("No host selected");
            return Vec::new();
        };
        self.files.host = Some(name.clone());
        vec![
            UiCommand::Files(FilesCommand::Disconnect),
            UiCommand::Files(FilesCommand::Connect(name)),
            UiCommand::Files(FilesCommand::Refresh {
                local: false,
                remote: true,
            }),
        ]
    }

    /// Open an error modal. Esc dismisses it.
    fn open_sessions(&mut self) -> Vec<UiCommand> {
        if self.ssh.is_none() {
            self.modal = Some(Modal::Error(missing_ssh_message().to_string()));
            return Vec::new();
        }
        let hosts = self.session_target_hosts();
        if hosts.is_empty() {
            self.toast("No host selected");
            return Vec::new();
        }
        self.screen = 4;
        let (rows, cols) = self.default_session_size();
        hosts
            .into_iter()
            .map(|host| {
                UiCommand::Sessions(SessionsCommand::Open {
                    host,
                    rows,
                    cols,
                    place: SessionPlace::NewTab,
                })
            })
            .collect()
    }

    fn session_target_hosts(&self) -> Vec<String> {
        if !self.view.selected.is_empty() {
            return self
                .hosts
                .iter()
                .filter(|host| self.view.selected.contains(&host.name))
                .map(|host| host.name.clone())
                .collect();
        }
        self.active_cursor_name()
            .into_iter()
            .collect()
    }

    fn default_session_size(&self) -> (u16, u16) {
        let area = self.session_content_area();
        let rows = area.height.saturating_sub(3).max(1);
        let cols = area.width.saturating_sub(2).max(1);
        (rows, cols)
    }

    fn sessions_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Vec<UiCommand> {
        let is_prefix = code == self.session_prefix.0
            && modifiers
                .intersection(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
                == self.session_prefix.1;

        if !self.sessions.focused
            && self.sessions.prefix_until.is_none()
            && self.sessions.confirm_close.is_none()
            && !self.sessions.scrollback
        {
            match code {
                KeyCode::Char('q') => return self.update(Action::Quit),
                KeyCode::Char('?') => return self.update(Action::ToggleHelp),
                KeyCode::Char(digit @ '1'..='5') => {
                    let screen = digit.to_digit(10).unwrap_or(0) as u8;
                    return self.update(Action::SwitchScreen(screen));
                }
                KeyCode::Enter if self.sessions.tab_count() > 0 => {
                    self.sessions.focused = true;
                    self.sessions.mark_active_read();
                    self.dirty = true;
                    return Vec::new();
                }
                _ => {}
            }
            if let Some(action) = self.keymap.lookup(code, modifiers) {
                return self.update(action);
            }
            self.dirty = true;
            return Vec::new();
        }

        let modes = self
            .sessions
            .focused_id()
            .and_then(|id| self.sessions.key_modes.get(&id).copied())
            .unwrap_or_default();
        let encode = |code, modifiers| encode_key(code, modifiers, &modes);
        let result = self.sessions.on_key(code, modifiers, is_prefix, encode);
        self.session_key_to_commands(result)
    }

    fn session_key_to_commands(&mut self, result: SessionKey) -> Vec<UiCommand> {
        self.dirty = true;
        match result {
            SessionKey::None => Vec::new(),
            SessionKey::Write(bytes) => {
                let Some(id) = self.sessions.focused_id() else {
                    return Vec::new();
                };
                // Re-encode is not needed; bytes already produced. For app-cursor accuracy,
                // the event loop rewrites using manager.key_modes when Write is applied if we
                // pass the raw key — keep bytes for paste/simple keys.
                vec![UiCommand::Sessions(SessionsCommand::Write { id, bytes })]
            }
            SessionKey::Split { host, dir } => {
                let (rows, cols) = self.default_session_size();
                vec![UiCommand::Sessions(SessionsCommand::Open {
                    host,
                    rows,
                    cols,
                    place: SessionPlace::Split(dir),
                })]
            }
            SessionKey::CloseFocused => {
                if let Some(id) = self.sessions.focused_id() {
                    vec![UiCommand::Sessions(SessionsCommand::Close(id))]
                } else {
                    Vec::new()
                }
            }
            SessionKey::ConfirmClose(_) => Vec::new(),
            SessionKey::Detach => Vec::new(),
            SessionKey::Reconnect { id, host } => {
                let (rows, cols) = self
                    .sessions
                    .last_sizes
                    .get(&id)
                    .copied()
                    .unwrap_or_else(|| self.default_session_size());
                vec![UiCommand::Sessions(SessionsCommand::Reconnect {
                    id,
                    host,
                    rows,
                    cols,
                })]
            }
            SessionKey::Scroll(delta) => {
                let Some(id) = self.sessions.focused_id() else {
                    return Vec::new();
                };
                vec![UiCommand::Sessions(SessionsCommand::Scroll { id, delta })]
            }
        }
    }

    /// Forward a paste into the focused session when appropriate.
    pub fn on_paste(&mut self, text: &str) -> Vec<UiCommand> {
        if self.screen != 4 || !self.sessions.focused || self.sessions.scrollback {
            return Vec::new();
        }
        let Some(id) = self.sessions.focused_id() else {
            return Vec::new();
        };
        if self.sessions.ended.contains_key(&id) {
            return Vec::new();
        }
        let bracketed = self
            .sessions
            .bracketed_paste
            .get(&id)
            .copied()
            .unwrap_or(false);
        let bytes = encode_paste(text, bracketed);
        self.dirty = true;
        vec![UiCommand::Sessions(SessionsCommand::Write { id, bytes })]
    }

    /// Apply a session manager event.
    pub fn apply_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Output(id) => {
                self.sessions.note_output(id);
                self.dirty = true;
            }
            SessionEvent::Exited { id, code } => {
                self.sessions.mark_ended(id, code);
                self.dirty = true;
            }
        }
    }

    /// Refresh key/paste modes from the live parser after output.
    pub fn refresh_session_modes(
        &mut self,
        id: SessionId,
        modes: KeyModes,
        bracketed_paste: bool,
    ) {
        self.sessions.key_modes.insert(id, modes);
        self.sessions.bracketed_paste.insert(id, bracketed_paste);
    }

    /// Record a newly opened session in the UI.
    pub fn session_opened(
        &mut self,
        id: SessionId,
        host: String,
        place: SessionPlace,
    ) {
        match place {
            SessionPlace::NewTab => self.sessions.open_tab(id, host),
            SessionPlace::Split(dir) => {
                if !self.sessions.split_focused(id, host.clone(), dir) {
                    self.sessions.open_tab(id, host);
                }
            }
        }
        self.screen = 4;
        self.dirty = true;
    }

    /// Record a reconnect that replaced `old` with `new`.
    pub fn session_reconnected(&mut self, old: SessionId, new: SessionId, host: String) {
        self.sessions.replace_session(old, new, host);
        self.dirty = true;
    }

    /// Drop UI state after a session is closed by the event loop.
    pub fn session_closed(&mut self, id: SessionId) {
        self.sessions.remove_session(id);
        self.dirty = true;
    }

    fn quit_sessions_key(&mut self, code: KeyCode) -> Vec<UiCommand> {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.modal = None;
                let mut commands = self.finish_quit();
                commands.insert(0, UiCommand::Sessions(SessionsCommand::CloseAll));
                commands
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.modal = None;
                self.dirty = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn force_quit(&mut self) -> Vec<UiCommand> {
        let mut commands = self.finish_quit();
        if !self.sessions.hosts.is_empty() {
            commands.insert(0, UiCommand::Sessions(SessionsCommand::CloseAll));
        }
        commands
    }

    fn finish_quit(&mut self) -> Vec<UiCommand> {
        self.state
            .remember_ui(screen_name(self.screen), &self.view.query);
        self.quit = true;
        let mut commands = vec![UiCommand::Persist];
        if self
            .exec_results
            .as_ref()
            .is_some_and(|results| results.running)
        {
            commands.insert(0, UiCommand::CancelExec);
        }
        commands
    }

    pub fn show_error(&mut self, message: impl Into<String>) {
        self.modal = Some(Modal::Error(message.into()));
        self.dirty = true;
    }

    /// In-memory state the event loop writes on [`UiCommand::Persist`].
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Path of `state.json`.
    pub fn state_path(&self) -> &std::path::Path {
        &self.state_path
    }

    /// Inventory hosts for SFTP argv building.
    pub fn effective_hosts(&self) -> &[EffectiveHost] {
        &self.hosts
    }

    /// Located `ssh` binary, if any.
    pub fn ssh_program(&self) -> Option<&PathBuf> {
        self.ssh.as_ref()
    }

    /// Whether ControlMaster paths are enabled.
    pub fn multiplexing_enabled(&self) -> bool {
        self.multiplexing
    }

    /// Directory for ssh control sockets.
    pub fn control_dir_path(&self) -> &std::path::Path {
        &self.control_dir
    }
}

fn files_home() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn screen_name(screen: u8) -> &'static str {
    match screen {
        1 => "hosts",
        2 => "health",
        3 => "files",
        4 => "sessions",
        _ => "log",
    }
}

fn screen_index(name: &str) -> Option<u8> {
    match name {
        "hosts" => Some(1),
        "health" => Some(2),
        "files" => Some(3),
        "sessions" => Some(4),
        "log" => Some(5),
        _ => None,
    }
}

fn format_prefix((code, modifiers): (KeyCode, KeyModifiers)) -> String {
    let key = match code {
        KeyCode::Char(ch) => ch.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        other => format!("{other:?}"),
    };
    let mut out = String::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        out.push_str("Ctrl+");
    }
    if modifiers.contains(KeyModifiers::ALT) {
        out.push_str("Alt+");
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        out.push_str("Shift+");
    }
    out.push_str(&key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::host::{Defaults, Host, ProblemSite, resolve};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn host(
        name: &str,
        address: &str,
        user: &str,
        tags: &[&str],
        notes: Option<&str>,
        jump: Option<&str>,
    ) -> EffectiveHost {
        resolve(
            &Host {
                name: name.into(),
                address: address.into(),
                user: Some(user.into()),
                tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
                notes: notes.map(str::to_string),
                jump: jump.map(str::to_string),
                ..Host::default()
            },
            &Defaults::default(),
            "ada",
        )
    }

    fn fleet() -> Vec<EffectiveHost> {
        vec![
            host("bastion", "198.51.100.5", "admin", &["infra"], None, None),
            host(
                "web-1",
                "203.0.113.10",
                "deploy",
                &["web", "prod"],
                Some("Main nginx box"),
                Some("bastion"),
            ),
            host(
                "web-2",
                "203.0.113.11",
                "deploy",
                &["web", "prod"],
                Some("Secondary nginx"),
                Some("bastion"),
            ),
            host(
                "db-1",
                "10.0.0.20",
                "db",
                &["db", "prod"],
                None,
                Some("bastion"),
            ),
            host(
                "db-3",
                "10.0.0.23",
                "db",
                &["db", "prod"],
                None,
                Some("bastion"),
            ),
        ]
    }

    fn preview(hosts: Vec<EffectiveHost>) -> App {
        let view = HostsView::new(&hosts);
        App {
            inventory: Inventory::default(),
            hosts,
            problems: Vec::new(),
            hosts_path: PathBuf::from("~/.config/vcon/hosts.toml"),
            theme: Theme::plain(),
            show_detail: true,
            multiplexing: false,
            control_dir: PathBuf::from("/tmp/vcon/cm"),
            screen: 1,
            keymap: Keymap::from_config(&crate::config::KeyConfig::default()).unwrap(),
            view,
            modal: None,
            toast: None,
            quit: false,
            dirty: true,
            terminal_width: 80,
            terminal_height: 24,
            mouse: false,
            frame: Duration::from_millis(33),
            state: AppState::default(),
            state_path: PathBuf::from("/tmp/vcon-state.json"),
            ssh: Some(PathBuf::from("ssh")),
            parallel: 10,
            confirm_threshold: 5,
            protected_tags: Vec::new(),
            exec_prompt: None,
            exec_confirm: None,
            exec_results: None,
            exec_visible: false,
            health: HealthView::new(30),
            health_config: HealthConfig::default(),
            health_history: 60,
            health_refresh: Duration::from_secs(30),
            files: files_ui::FilesView::new(files_home()),
            sessions: SessionsView::new(),
            session_prefix: (KeyCode::Char(']'), KeyModifiers::CONTROL),
            session_scrollback: 5000,
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        app.note_size(width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| app.draw(frame, None))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    #[test]
    fn selection_is_by_name_and_escape_clears_it() {
        let mut app = preview(fleet());
        app.on_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.view.selected.contains("bastion"));
        app.on_key(KeyCode::Char('j'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.view.selected.contains("web-1"));
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('b'), KeyModifiers::NONE);
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.view.selected.contains("bastion"));
        assert!(app.view.selected.contains("web-1"));
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.view.selected.is_empty());
        assert!(!app.view.query.is_empty());
    }

    #[test]
    fn cursor_stays_put_while_the_host_is_still_visible() {
        let mut app = preview(fleet());
        app.view.cursor_name = Some("db-3".into());
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for ch in "10.0.0.2".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.view.cursor_name.as_deref(), Some("db-3"));
        app.on_key(KeyCode::Char('0'), KeyModifiers::NONE);
        assert_eq!(app.view.cursor_name.as_deref(), Some("db-1"));
    }

    #[test]
    fn tag_tree_filters_and_help_toggles() {
        let mut app = preview(fleet());
        app.on_key(KeyCode::Char('h'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(matches!(app.view.scope, hosts::HostScope::Tag(ref tag) if tag == "db"));
        let names: Vec<_> = app
            .view
            .output(&app.hosts)
            .matches
            .iter()
            .map(|matched| app.hosts[matched.index].name.as_str())
            .collect();
        assert_eq!(names, ["db-1", "db-3"]);
        app.on_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(app.modal, Some(Modal::Help));
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.modal, None);
    }

    #[test]
    fn copy_returns_the_resolved_ssh_command() {
        let mut app = preview(fleet());
        app.view.cursor_name = Some("web-1".into());
        let commands = app.on_key(KeyCode::Char('y'), KeyModifiers::NONE);
        match commands.as_slice() {
            [UiCommand::Copy(line)] => {
                assert!(line.contains("deploy@203.0.113.10"), "{line}");
                assert!(line.contains("admin@198.51.100.5"), "{line}");
                assert!(line.starts_with("ssh "), "{line}");
            }
            other => panic!("expected a copy command, got {other:?}"),
        }
    }

    #[test]
    fn validation_modal_lists_problems() {
        let mut app = preview(fleet());
        app.problems.push(Problem {
            site: ProblemSite::Host(0),
            message: "host 'web-1' jump 'ghost' does not exist".into(),
            line: Some(4),
        });
        app.on_key(KeyCode::Char('v'), KeyModifiers::NONE);
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("does not exist"));
        assert!(screen.contains('⚠') || screen.contains('1'));
    }

    #[test]
    fn missing_ssh_opens_an_error_modal_instead_of_connecting() {
        let mut app = preview(fleet());
        app.ssh = None;
        let commands = app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(commands.is_empty());
        assert!(matches!(app.modal, Some(Modal::Error(ref text)) if text.contains("PATH")));
    }

    #[test]
    fn snapshot_full_list() {
        let mut app = preview(fleet());
        app.view.cursor_name = Some("web-1".into());
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("web-1"));
        assert!(screen.contains("bastion"));
        assert!(screen.contains("deploy@203.0.113.10"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn snapshot_filtered_list() {
        let mut app = preview(fleet());
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for ch in ['p', 'r', ' ', 'n', 'g'] {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("web-1"));
        assert!(screen.contains("web-2"));
        assert!(!screen.contains("db-1"));
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn snapshot_empty_state() {
        let mut app = preview(Vec::new());
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("No hosts yet"));
        assert!(screen.contains("~/.config/vcon/hosts.toml"));
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn snapshot_narrow_terminal() {
        let mut app = preview(fleet());
        let screen = render(&mut app, 40, 12);
        assert!(!screen.contains("Tags"));
        assert!(!screen.contains("Detail"));
        assert!(screen.contains("web-1"));
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn enter_requests_ssh_without_moving_the_cursor() {
        let mut app = preview(fleet());
        app.view.cursor_name = Some("web-1".into());
        app.view.query = "web".into();
        app.view.selected.insert("web-1".into());
        let commands = app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.view.cursor_name.as_deref(), Some("web-1"));
        assert_eq!(app.view.query, "web");
        assert!(app.view.selected.contains("web-1"));
        assert_eq!(app.state().hosts["web-1"].connect_count, 1);
        match commands.as_slice() {
            [
                UiCommand::Persist,
                UiCommand::Suspend(Handoff::Connect(handoff)),
            ] => {
                assert!(handoff.banner.contains("via bastion"), "{}", handoff.banner);
                assert!(handoff.args.iter().any(|arg| arg == "deploy@203.0.113.10"));
            }
            other => panic!("expected a connect handoff, got {other:?}"),
        }
        app.view.scope = hosts::HostScope::Recent;
        let names: Vec<_> = app
            .view
            .output(&app.hosts)
            .matches
            .iter()
            .map(|matched| app.hosts[matched.index].name.as_str())
            .collect();
        assert_eq!(names, ["web-1"]);
    }

    #[test]
    fn star_toggles_a_favorite_and_sorts_by_frecency() {
        let mut app = preview(fleet());
        app.view.cursor_name = Some("db-1".into());
        let commands = app.on_key(KeyCode::Char('*'), KeyModifiers::SHIFT);
        assert!(matches!(commands.as_slice(), [UiCommand::Persist]));
        assert!(app.view.favorites.contains("db-1"));
        app.view.scope = hosts::HostScope::Favorites;
        let names: Vec<_> = app
            .view
            .output(&app.hosts)
            .matches
            .iter()
            .map(|matched| app.hosts[matched.index].name.as_str())
            .collect();
        assert_eq!(names, ["db-1"]);

        let now = Utc::now();
        app.state
            .record_connect("bastion", now - chrono::TimeDelta::days(40));
        app.state
            .record_connect("bastion", now - chrono::TimeDelta::days(40));
        app.state.record_connect("db-1", now);
        app.refresh_groups();
        app.view.scope = hosts::HostScope::All;
        app.on_key(KeyCode::Char('S'), KeyModifiers::SHIFT);
        let names: Vec<_> = app
            .view
            .output(&app.hosts)
            .matches
            .iter()
            .map(|matched| app.hosts[matched.index].name.as_str())
            .collect();
        assert_eq!(names[0], "db-1");
    }

    #[test]
    fn last_filter_is_restored_when_start_screen_is_last() {
        let mut app = preview(fleet());
        app.state.remember_ui("hosts", "@db");
        app.apply_start_screen("last");
        assert_eq!(app.view.query, "@db");
        let names: Vec<_> = app
            .view
            .output(&app.hosts)
            .matches
            .iter()
            .map(|matched| app.hosts[matched.index].name.as_str())
            .collect();
        assert_eq!(names, ["db-1", "db-3"]);
    }

    #[test]
    fn ssh_255_opens_an_error_modal() {
        let mut app = preview(fleet());
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 255")
            .status()
            .unwrap();
        app.note_session_exit(&status);
        assert!(matches!(app.modal, Some(Modal::Error(ref text)) if text.contains("255")));
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.modal.is_some());
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.modal.is_none());
    }

    #[test]
    fn editor_handoff_targets_the_inventory_file() {
        let mut app = preview(fleet());
        let commands = app.on_key(KeyCode::Char('E'), KeyModifiers::NONE);
        match commands.as_slice() {
            [UiCommand::Suspend(Handoff::Editor(handoff))] => {
                assert_eq!(
                    handoff.args.last().map(String::as_str),
                    Some("~/.config/vcon/hosts.toml")
                );
            }
            other => panic!("expected an editor handoff, got {other:?}"),
        }
    }

    #[test]
    fn reload_picks_up_a_new_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(
            &path,
            "[[hosts]]\nname = \"added\"\naddress = \"203.0.113.9\"\n",
        )
        .unwrap();
        let mut app = preview(fleet());
        app.hosts_path = path;
        app.reload_inventory();
        assert!(app.hosts.iter().any(|host| host.name == "added"));
        assert!(app.toast.as_ref().unwrap().0.contains("reloaded"));
    }

    fn writable(body: &str) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, body).unwrap();
        let mut app = preview(Vec::new());
        app.hosts_path = path;
        app.reload_inventory();
        app.toast = None;
        (dir, app)
    }

    const PAIR: &str = r#"
[[hosts]]
name = "web-1"
address = "203.0.113.10"
ssh_options = { ForwardAgent = "yes" }

[[hosts]]
name = "db-1"
address = "10.0.0.20"
jump = "web-1"
"#;

    #[test]
    fn add_writes_a_host_and_a_duplicate_name_does_not() {
        let (_dir, mut app) = writable(PAIR);
        app.on_key(KeyCode::Char('a'), KeyModifiers::NONE);
        for ch in "edge".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Down, KeyModifiers::NONE);
        for ch in "10.1.1.1".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let commands = app.on_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::Persist))
        );
        assert!(app.modal.is_none());
        let text = std::fs::read_to_string(&app.hosts_path).unwrap();
        assert!(text.contains("edge"), "{text}");
        assert!(text.contains("10.1.1.1"), "{text}");

        app.on_key(KeyCode::Char('a'), KeyModifiers::NONE);
        for ch in "web-1".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, Some(Modal::HostForm(_))));
        let text = std::fs::read_to_string(&app.hosts_path).unwrap();
        assert_eq!(text.matches("name = \"web-1\"").count(), 1, "{text}");
    }

    #[test]
    fn delete_refuses_a_jump_target_and_removes_a_leaf() {
        let (_dir, mut app) = writable(PAIR);
        app.state.record_connect("db-1", Utc::now());
        app.view.cursor_name = Some("web-1".to_string());
        app.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        match &app.modal {
            Some(Modal::Error(message)) => assert!(message.contains("jump"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            std::fs::read_to_string(&app.hosts_path)
                .unwrap()
                .contains("web-1")
        );

        app.modal = None;
        app.view.cursor_name = Some("db-1".to_string());
        app.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.quit());
        assert!(matches!(app.modal, Some(Modal::Delete { .. })));
        let commands = app.on_key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::Persist))
        );
        let text = std::fs::read_to_string(&app.hosts_path).unwrap();
        assert!(!text.contains("db-1"), "{text}");
        assert!(text.contains("web-1"), "{text}");
        assert!(!app.state.hosts.contains_key("db-1"));
    }

    #[test]
    fn duplicate_writes_a_copy_and_keeps_ssh_options() {
        let (_dir, mut app) = writable(PAIR);
        app.view.cursor_name = Some("web-1".to_string());
        app.on_key(KeyCode::Char('D'), KeyModifiers::NONE);
        let commands = app.on_key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::Persist))
        );
        let loaded = crate::infra::inventory_store::load(&app.hosts_path).unwrap();
        let copy = loaded
            .inventory
            .hosts
            .iter()
            .find(|host| host.name == "web-1-copy")
            .unwrap();
        assert_eq!(copy.address, "203.0.113.10");
        assert_eq!(
            copy.ssh_options.get("ForwardAgent").map(String::as_str),
            Some("yes")
        );
    }

    #[test]
    fn exec_confirms_a_protected_tag_and_q_stays_in_the_prompt() {
        let mut app = preview(fleet());
        app.on_key(KeyCode::Char('x'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.quit());
        assert_eq!(app.exec_prompt.as_ref().unwrap().command, "q");
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.exec_prompt.is_none());

        app.protected_tags = vec!["prod".to_string()];
        app.view.cursor_name = Some("web-1".to_string());
        app.on_key(KeyCode::Char('x'), KeyModifiers::NONE);
        for ch in "uptime".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let commands = app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(commands.is_empty());
        assert!(app.exec_confirm.as_ref().unwrap().reason.contains("prod"));
        let commands = app.on_key(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(commands.iter().any(|command| matches!(command, UiCommand::StartExec(launch) if launch.targets.len() == 1 && launch.targets[0].args.last().map(String::as_str) == Some("uptime"))));
        assert!(app.exec_visible);
        let commands = app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(commands.is_empty());
        assert!(!app.exec_visible);
        assert!(app.exec_results.as_ref().unwrap().running);
        app.on_key(KeyCode::Char('X'), KeyModifiers::NONE);
        assert!(app.exec_visible);
        let commands = app.on_key(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, UiCommand::CancelExec))
        );
    }
}
