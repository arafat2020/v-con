//! Command prompt and the live per-host results view.
//!
//! The prompt never runs ssh. It returns the command, the sudo flag, and the
//! timeout. The results view renders events that [`crate::services::exec`] emits.

use crate::services::exec::{EndStatus, HostOutcome, JobEvent, OUTPUT_CAP};
use crate::tui::theme::Theme;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::time::{Duration, Instant};

const TIMEOUTS: [u64; 5] = [10, 30, 60, 120, 0];
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// The command the user is typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Host names shown in the title.
    pub targets: Vec<String>,
    /// Text in the command field.
    pub command: String,
    /// Cursor, in characters.
    pub cursor: usize,
    /// Prefix the command with `sudo -n`.
    pub sudo: bool,
    /// Seconds. `0` means no per-host limit.
    pub timeout_secs: u64,
    /// Older commands, oldest first.
    pub history: Vec<String>,
    /// Walk position while ↑/↓ is browsing history. `None` is the live draft.
    history_at: Option<usize>,
    /// Command text saved when history browsing starts.
    draft: String,
}

/// What the prompt wants after one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptEvent {
    /// Stay open.
    Continue,
    /// Close without running.
    Cancel,
    /// The command is ready to confirm or start.
    Submit {
        /// Trimmed command, without the sudo prefix.
        command: String,
        /// Whether to prefix `sudo -n`.
        sudo: bool,
        /// Per-host timeout. Zero waits without a limit.
        timeout: Duration,
    },
}

impl Prompt {
    /// A prompt for `targets`, seeded with `history`.
    pub fn new(targets: Vec<String>, history: Vec<String>) -> Self {
        Self {
            targets,
            command: String::new(),
            cursor: 0,
            sudo: false,
            timeout_secs: 30,
            history,
            history_at: None,
            draft: String::new(),
        }
    }

    /// Title such as `Run on 2 hosts (web-1, web-2)`.
    pub fn title(&self) -> String {
        let noun = if self.targets.len() == 1 {
            "host"
        } else {
            "hosts"
        };
        let shown: Vec<&str> = self.targets.iter().take(3).map(String::as_str).collect();
        let mut names = shown.join(", ");
        if self.targets.len() > 3 {
            names.push_str(", …");
        }
        format!("Run on {} {noun} ({names})", self.targets.len())
    }
}

/// Apply one key. Printable characters, including `q`, stay in the field.
pub fn prompt_on_key(prompt: &mut Prompt, code: KeyCode, modifiers: KeyModifiers) -> PromptEvent {
    if code == KeyCode::Esc {
        return PromptEvent::Cancel;
    }
    if code == KeyCode::Enter {
        let command = prompt.command.trim().to_string();
        if command.is_empty() {
            return PromptEvent::Continue;
        }
        return PromptEvent::Submit {
            command,
            sudo: prompt.sudo,
            timeout: Duration::from_secs(prompt.timeout_secs),
        };
    }
    if code == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
        prompt.sudo = !prompt.sudo;
        return PromptEvent::Continue;
    }
    if code == KeyCode::Char('t') && modifiers.contains(KeyModifiers::CONTROL) {
        let index = TIMEOUTS
            .iter()
            .position(|seconds| *seconds == prompt.timeout_secs)
            .unwrap_or(1);
        prompt.timeout_secs = TIMEOUTS[(index + 1) % TIMEOUTS.len()];
        return PromptEvent::Continue;
    }
    match code {
        KeyCode::Up => history_step(prompt, -1),
        KeyCode::Down => history_step(prompt, 1),
        KeyCode::Left => {
            prompt.cursor = prompt.cursor.saturating_sub(1);
        }
        KeyCode::Right => {
            let count = prompt.command.chars().count();
            prompt.cursor = (prompt.cursor + 1).min(count);
        }
        KeyCode::Backspace => {
            prompt.history_at = None;
            if prompt.cursor > 0 {
                let remove_at = prompt.cursor - 1;
                let byte = prompt
                    .command
                    .char_indices()
                    .nth(remove_at)
                    .map(|(index, _)| index);
                if let Some(byte) = byte {
                    prompt.command.remove(byte);
                    prompt.cursor = remove_at;
                }
            }
        }
        KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
            prompt.history_at = None;
            let byte = prompt
                .command
                .char_indices()
                .nth(prompt.cursor)
                .map(|(index, _)| index)
                .unwrap_or(prompt.command.len());
            prompt.command.insert(byte, ch);
            prompt.cursor += 1;
        }
        _ => {}
    }
    PromptEvent::Continue
}

fn history_step(prompt: &mut Prompt, delta: isize) {
    if prompt.history.is_empty() {
        return;
    }
    let next = match prompt.history_at {
        None if delta < 0 => {
            prompt.draft = prompt.command.clone();
            Some(prompt.history.len() - 1)
        }
        None => None,
        Some(index) => {
            let next = index as isize + delta;
            if next < 0 {
                Some(0)
            } else if next as usize >= prompt.history.len() {
                None
            } else {
                Some(next as usize)
            }
        }
    };
    prompt.history_at = next;
    prompt.command = match next {
        Some(index) => prompt.history[index].clone(),
        None => prompt.draft.clone(),
    };
    prompt.cursor = prompt.command.chars().count();
}

/// Confirmation that names the command and why it is required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    /// Command that will run, without `sudo -n`.
    pub command: String,
    /// Whether the launch prefixes `sudo -n`.
    pub sudo: bool,
    /// Per-host timeout.
    pub timeout: Duration,
    /// Hosts this run will use.
    pub hosts: Vec<crate::domain::host::EffectiveHost>,
    /// [`crate::services::exec::ConfirmReason::message`].
    pub reason: String,
}

/// One line kept for a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    /// Stderr is drawn in the error colour.
    pub stderr: bool,
    /// Text without the newline.
    pub text: String,
}

/// One row in the results list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Inventory name.
    pub name: String,
    /// Current status.
    pub status: EndStatus,
    /// Set once the host has finished.
    pub outcome: Option<HostOutcome>,
    /// When ssh started, so a running row can show elapsed time.
    pub started: Option<Instant>,
    /// Captured lines, trimmed to [`OUTPUT_CAP`].
    pub lines: Vec<OutputLine>,
    /// Bytes currently stored in `lines`.
    bytes: usize,
    /// True once a line was dropped to stay under the cap.
    pub truncated: bool,
    /// First visible output line.
    pub scroll: usize,
    /// Follow new lines.
    stick: bool,
}

/// The exec results view.
#[derive(Debug, Clone)]
pub struct Results {
    /// Command the user typed.
    pub command: String,
    /// Sudo flag, kept so a rerun uses the same command.
    pub sudo: bool,
    /// Timeout, kept so a rerun uses the same limit.
    pub timeout: Duration,
    /// One row per target, in the order the job was started.
    pub rows: Vec<Row>,
    /// Index into `rows`, or into the failed-only subset when that filter is on.
    pub cursor: usize,
    /// Hide hosts that succeeded.
    pub failed_only: bool,
    /// One pane of interleaved output.
    pub combined: bool,
    /// Output uses the whole popup.
    pub expanded: bool,
    /// The job has not emitted [`JobEvent::JobFinished`].
    pub running: bool,
    /// Spinner frame.
    pub spinner: u8,
    /// When the job started.
    pub started: Instant,
    /// Scroll position of the combined pane.
    combined_scroll: usize,
}

impl Results {
    /// Rows start queued. `running` is true until the job finishes.
    pub fn starting(command: String, sudo: bool, timeout: Duration, names: &[String]) -> Self {
        Self {
            command,
            sudo,
            timeout,
            rows: names
                .iter()
                .map(|name| Row {
                    name: name.clone(),
                    status: EndStatus::Skipped,
                    outcome: None,
                    started: None,
                    lines: Vec::new(),
                    bytes: 0,
                    truncated: false,
                    scroll: 0,
                    stick: true,
                })
                .collect(),
            cursor: 0,
            failed_only: false,
            combined: false,
            expanded: false,
            running: true,
            spinner: 0,
            started: Instant::now(),
            combined_scroll: 0,
        }
    }

    /// Fold one service event into the rows.
    pub fn apply(&mut self, event: JobEvent) {
        match event {
            JobEvent::HostStarted { host } => {
                if let Some(row) = self.row_mut(&host) {
                    row.status = EndStatus::Ok;
                    row.started = Some(Instant::now());
                    row.outcome = None;
                }
            }
            JobEvent::Line { host, stderr, text } => {
                if let Some(row) = self.row_mut(&host) {
                    push_line(row, stderr, text);
                }
            }
            JobEvent::HostFinished { host, outcome } => {
                if let Some(row) = self.row_mut(&host) {
                    row.status = outcome.status;
                    row.truncated = row.truncated || outcome.truncated;
                    row.outcome = Some(outcome);
                }
            }
            JobEvent::JobFinished { .. } => self.running = false,
        }
    }

    fn row_mut(&mut self, name: &str) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.name == name)
    }

    /// Hosts that did not succeed. Used by `R`.
    pub fn failed_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| {
                row.outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.status != EndStatus::Ok)
            })
            .map(|row| row.name.clone())
            .collect()
    }

    fn visible(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                !self.failed_only
                    || row
                        .outcome
                        .as_ref()
                        .is_some_and(|outcome| outcome.status != EndStatus::Ok)
                    || row.outcome.is_none()
            })
            .map(|(index, _)| index)
            .collect()
    }
}

fn push_line(row: &mut Row, stderr: bool, text: String) {
    row.bytes += text.len() + 1;
    row.lines.push(OutputLine { stderr, text });
    while row.bytes > OUTPUT_CAP && row.lines.len() > 1 {
        let removed = row.lines.remove(0);
        row.bytes = row.bytes.saturating_sub(removed.text.len() + 1);
        row.truncated = true;
        if row.scroll > 0 {
            row.scroll -= 1;
        }
    }
    if row.stick {
        row.scroll = row.lines.len().saturating_sub(1);
    }
}

/// Keys the results view understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultsEvent {
    /// Stay on the view.
    Continue,
    /// Hide the view. The job keeps running.
    Close,
    /// Cancel the running job.
    Cancel,
    /// Copy the highlighted output.
    Copy(String),
    /// Run the same command on the failed hosts.
    Rerun(Vec<String>),
}

/// Results keys. `Esc` and `q` hide the view and leave the job running.
pub fn results_on_key(results: &mut Results, code: KeyCode) -> ResultsEvent {
    let visible = results.visible();
    match code {
        KeyCode::Esc | KeyCode::Char('q') => ResultsEvent::Close,
        KeyCode::Char('c') => ResultsEvent::Cancel,
        KeyCode::Char('f') => {
            results.failed_only = !results.failed_only;
            results.cursor = 0;
            ResultsEvent::Continue
        }
        KeyCode::Char('o') => {
            results.combined = !results.combined;
            ResultsEvent::Continue
        }
        KeyCode::Enter => {
            results.expanded = !results.expanded;
            ResultsEvent::Continue
        }
        KeyCode::Char('y') => ResultsEvent::Copy(copy_text(results)),
        KeyCode::Char('R') => ResultsEvent::Rerun(results.failed_names()),
        KeyCode::Up | KeyCode::Char('k') => {
            if results.combined || results.expanded {
                scroll(results, -1);
            } else {
                results.cursor = results.cursor.saturating_sub(1);
            }
            ResultsEvent::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if results.combined || results.expanded {
                scroll(results, 1);
            } else if results.cursor + 1 < visible.len() {
                results.cursor += 1;
            }
            ResultsEvent::Continue
        }
        KeyCode::PageUp => {
            scroll(results, -8);
            ResultsEvent::Continue
        }
        KeyCode::PageDown => {
            scroll(results, 8);
            ResultsEvent::Continue
        }
        _ => ResultsEvent::Continue,
    }
}

fn scroll(results: &mut Results, delta: isize) {
    if results.combined {
        let next = results.combined_scroll as isize + delta;
        results.combined_scroll = next.max(0) as usize;
        return;
    }
    let Some(index) = results.visible().get(results.cursor).copied() else {
        return;
    };
    let row = &mut results.rows[index];
    let next = row.scroll as isize + delta;
    row.scroll = next.max(0) as usize;
    row.stick = row.scroll + 1 >= row.lines.len();
}

fn copy_text(results: &Results) -> String {
    if results.combined {
        return combined_lines(results).join("\n");
    }
    let Some(index) = results.visible().get(results.cursor).copied() else {
        return String::new();
    };
    results.rows[index]
        .lines
        .iter()
        .map(|line| {
            if line.stderr {
                format!("! {}", line.text)
            } else {
                line.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn combined_lines(results: &Results) -> Vec<String> {
    let mut lines = Vec::new();
    for row in &results.rows {
        for line in &row.lines {
            if line.stderr {
                lines.push(format!("[{}] ! {}", row.name, line.text));
            } else {
                lines.push(format!("[{}] {}", row.name, line.text));
            }
        }
    }
    lines
}

/// Draw the command prompt in the center of `area`.
pub fn draw_prompt(frame: &mut Frame, area: Rect, prompt: &Prompt, theme: &Theme) {
    let popup = centered(area, 68, 9);
    frame.render_widget(Clear, popup);
    let sudo = if prompt.sudo { "on" } else { "off" };
    let timeout = if prompt.timeout_secs == 0 {
        "none".to_string()
    } else {
        format!("{}s", prompt.timeout_secs)
    };
    let body = format!(
        "{}\n\n> {}\n\nsudo -n: {sudo}    timeout: {timeout}\nEnter runs   ^S sudo   ^T timeout   ↑↓ history   Esc cancels",
        prompt.title(),
        prompt.command
    );
    let paragraph = Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Exec ")
                .border_style(theme.selected()),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

/// Draw the confirmation popup.
pub fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &Confirm, theme: &Theme) {
    let popup = centered(area, 68, 8);
    frame.render_widget(Clear, popup);
    let remote = crate::services::exec::remote_command(&confirm.command, confirm.sudo);
    let body = format!(
        "Run `{remote}`?\n{}\ny confirms   n or Esc cancels",
        confirm.reason
    );
    let paragraph = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Confirm exec ")
            .border_style(theme.warning()),
    );
    frame.render_widget(paragraph, popup);
}

/// Draw the results popup.
pub fn draw_results(frame: &mut Frame, area: Rect, results: &Results, theme: &Theme) {
    let popup = centered(
        area,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    frame.render_widget(Clear, popup);
    let ok = results
        .rows
        .iter()
        .filter(|row| {
            row.outcome
                .as_ref()
                .is_some_and(|outcome| outcome.status == EndStatus::Ok)
        })
        .count();
    let failed = results
        .rows
        .iter()
        .filter(|row| {
            row.outcome
                .as_ref()
                .is_some_and(|outcome| outcome.status != EndStatus::Ok)
        })
        .count();
    let elapsed = format_elapsed(results.started.elapsed());
    let title = format!(
        " Exec: {} ─ {} hosts ─ {ok} ✔ {failed} ✖ ─ {elapsed} ",
        results.command,
        results.rows.len()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(theme.selected());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let chunks = if results.expanded || results.combined {
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner)
    } else {
        let body = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner)[0]);
        let hint_area =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner)[1];
        draw_host_list(frame, body[0], results, theme);
        draw_output(frame, body[1], results, theme);
        frame.render_widget(
            Paragraph::new(
                "↑↓ host  ⏎ full  f failed  o combined  R rerun  y copy  c cancel  Esc close",
            )
            .style(theme.muted()),
            hint_area,
        );
        return;
    };
    draw_output(frame, chunks[0], results, theme);
    frame.render_widget(
        Paragraph::new("↑↓ scroll  ⏎ full  o combined  y copy  c cancel  Esc close")
            .style(theme.muted()),
        chunks[1],
    );
}

fn draw_host_list(frame: &mut Frame, area: Rect, results: &Results, theme: &Theme) {
    let visible = results.visible();
    let mut lines = Vec::new();
    for (shown, index) in visible.iter().copied().enumerate() {
        let row = &results.rows[index];
        let mark = mark(row, results.spinner);
        let duration = row
            .started
            .map(|started| {
                row.outcome
                    .as_ref()
                    .map(|outcome| outcome.duration)
                    .unwrap_or_else(|| started.elapsed())
            })
            .map(format_elapsed)
            .unwrap_or_else(|| "—".to_string());
        let style = if shown == results.cursor {
            theme.selected()
        } else {
            status_style(row, theme)
        };
        lines.push(Line::from(Span::styled(
            format!(" {mark} {:<14} {duration}", row.name),
            style,
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_output(frame: &mut Frame, area: Rect, results: &Results, theme: &Theme) {
    let lines = if results.combined {
        let all = combined_lines(results);
        let start = results.combined_scroll.min(all.len().saturating_sub(1));
        all.into_iter()
            .skip(start)
            .map(Line::from)
            .collect::<Vec<_>>()
    } else {
        let Some(index) = results.visible().get(results.cursor).copied() else {
            frame.render_widget(Paragraph::new("no host"), area);
            return;
        };
        let row = &results.rows[index];
        let mut header = row.name.clone();
        if let Some(outcome) = &row.outcome {
            header.push_str(&format!(
                "  {}  {}",
                outcome.reason,
                format_elapsed(outcome.duration)
            ));
        }
        let mut lines = vec![Line::styled(
            header,
            Style::new().add_modifier(Modifier::BOLD),
        )];
        for line in row.lines.iter().skip(row.scroll) {
            let style = if line.stderr {
                theme.error()
            } else {
                Style::new()
            };
            lines.push(Line::styled(line.text.clone(), style));
        }
        if row.truncated {
            lines.push(Line::styled("[truncated]", theme.warning()));
        }
        lines
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn mark(row: &Row, spinner: u8) -> &'static str {
    match &row.outcome {
        None if row.started.is_some() => SPINNER[spinner as usize % SPINNER.len()],
        None => "·",
        Some(outcome) => match outcome.status {
            EndStatus::Ok => "✔",
            EndStatus::Timeout => "⏱",
            EndStatus::Skipped => "⊘",
            EndStatus::Cancelled => "⊘",
            _ => "✖",
        },
    }
}

fn status_style(row: &Row, theme: &Theme) -> Style {
    match row.outcome.as_ref().map(|outcome| outcome.status) {
        Some(EndStatus::Ok) => theme.ok(),
        Some(EndStatus::Skipped | EndStatus::Cancelled) | None => theme.muted(),
        Some(_) => theme.error(),
    }
}

fn format_elapsed(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 10_000 {
        format!("{:.1}s", millis as f64 / 1000.0)
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width).max(1);
    let height = height.min(area.height).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_stays_in_the_command_and_history_cycles() {
        let mut prompt = Prompt::new(vec!["web-1".to_string()], vec!["uptime".to_string()]);
        assert_eq!(
            prompt_on_key(&mut prompt, KeyCode::Char('q'), KeyModifiers::NONE),
            PromptEvent::Continue
        );
        assert_eq!(prompt.command, "q");
        prompt_on_key(&mut prompt, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(prompt.command, "uptime");
        prompt_on_key(&mut prompt, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(prompt.command, "q");
    }

    #[test]
    fn a_line_arrives_after_the_host_starts() {
        let mut results = Results::starting(
            "uptime".to_string(),
            false,
            Duration::from_secs(30),
            &["web-1".to_string()],
        );
        results.apply(JobEvent::HostStarted {
            host: "web-1".to_string(),
        });
        results.apply(JobEvent::Line {
            host: "web-1".to_string(),
            stderr: false,
            text: " 12:00 up".to_string(),
        });
        assert_eq!(results.rows[0].lines[0].text, " 12:00 up");
        assert!(results.running);
        results.apply(JobEvent::JobFinished {
            summary: crate::services::exec::Summary {
                ok: 1,
                failed: 0,
                skipped: 0,
                duration: Duration::from_secs(1),
            },
        });
        assert!(!results.running);
    }
}
