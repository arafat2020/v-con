//! Application state for the TUI shell.
//!
//! [`App::update`] changes state and returns [`UiCommand`]s. It does not draw
//! and it does not touch the clipboard. The Hosts screen handles its own keys;
//! anything it does not recognise (quit, help, copy) is handled here.
//! Other tabs are placeholders until their phases.

use crate::config::Config;
use crate::domain::host::{EffectiveHost, Problem};
use crate::error::Error;
use crate::infra::ssh::command::{SshCommandOptions, build_ssh_command, command_line};
use crate::paths::Paths;
use crate::services::inventory::InventoryBundle;
use crate::tui::action::{Action, UiCommand};
use crate::tui::keymap::Keymap;
use crate::tui::screens::hosts::{self, HostsView, ViewOptions};
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::path::PathBuf;
use std::time::Duration;

const TABS: [&str; 5] = ["Hosts", "Health", "Files", "Sessions", "Log"];

/// Which popup is on top of the Hosts screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    /// Key list. Also includes validation problems when there are any.
    Help,
    /// The validation problems from the last load.
    Problems,
}

/// Shell state: inventory, keymap, the Hosts screen, and the open modal.
pub struct App {
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
        let frame = Duration::from_millis(1000 / u64::from(config.tui.fps.max(1)));
        Ok(Self {
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
        })
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
            return self.update(Action::Quit);
        }
        if self.modal.is_some() && code == KeyCode::Esc {
            return self.update(Action::CloseModal);
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
            Action::Quit => self.quit = true,
            Action::Tick => {
                if let Some((_, left)) = &mut self.toast {
                    if *left == 0 {
                        self.toast = None;
                    } else {
                        *left -= 1;
                    }
                } else {
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
                }
            }
            Action::CopySshCommand => return self.copy_command(),
            Action::Connect => self.toast("connect arrives in phase 2"),
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

    /// Show `message` on the hint line for about two seconds.
    pub fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), 8));
        self.dirty = true;
    }

    /// Draw the tab bar, the active screen, the hint line, and a modal if one is open.
    pub fn draw(&mut self, frame: &mut Frame) {
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
            let options = ViewOptions {
                theme: &theme,
                hosts_path: &hosts_path,
                show_detail,
                terminal_width,
                terminal_height,
                multiplexing,
                control_dir: &control_dir,
            };
            hosts::draw(frame, chunks[1], &mut self.view, &self.hosts, &options);
        } else {
            let message = match self.screen {
                2 => "Health arrives in a later phase.",
                3 => "Files arrives in a later phase.",
                4 => "Sessions arrives in a later phase.",
                _ => "Log arrives in a later phase.",
            };
            frame.render_widget(Paragraph::new(message), chunks[1]);
        }
        self.draw_hints(frame, chunks[2]);
        if let Some(modal) = self.modal {
            self.draw_modal(frame, area, modal);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![Span::raw(" vcon ")];
        for (index, name) in TABS.iter().enumerate() {
            let label = format!(" {} {} ", index + 1, name);
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
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_hints(&self, frame: &mut Frame, area: Rect) {
        let text = if let Some((message, _)) = &self.toast {
            message.clone()
        } else {
            let mut hint =
                " j/k move   h/l focus   / search   space select   y copy   ? help   q quit"
                    .to_string();
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
        let Some(host) = self.view.cursor(&self.hosts) else {
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
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        app.note_size(width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
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
}
