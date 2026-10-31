//! Fleet health table and per-host charts.

use crate::domain::filter::{self, Scope};
use crate::domain::health::{HealthReport, HealthStatus, HistoryPoint, format_rate};
use crate::domain::host::EffectiveHost;
use crate::services::health::{HostHealth, status_counts};
use crate::tui::theme::Theme;
use crate::tui::widgets::gauge::{bar, sparkline};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table, TableState, Wrap,
};
use std::collections::{BTreeMap, VecDeque};

/// Column used for sorting the health table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Inventory order / name.
    Host,
    /// Overall status.
    Status,
    /// CPU percent.
    Cpu,
    /// Load average.
    Load,
    /// Memory percent.
    Mem,
    /// Disk percent.
    Disk,
    /// Receive rate.
    Net,
    /// Uptime seconds.
    Up,
}

impl SortKey {
    /// Cycle to the next column.
    pub fn next(self) -> Self {
        match self {
            Self::Host => Self::Status,
            Self::Status => Self::Cpu,
            Self::Cpu => Self::Load,
            Self::Load => Self::Mem,
            Self::Mem => Self::Disk,
            Self::Disk => Self::Net,
            Self::Net => Self::Up,
            Self::Up => Self::Host,
        }
    }

    /// Short label for the header.
    pub fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Status => "status",
            Self::Cpu => "cpu",
            Self::Load => "load",
            Self::Mem => "mem",
            Self::Disk => "disk",
            Self::Net => "net",
            Self::Up => "up",
        }
    }
}

/// What a key on the health screen means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthEvent {
    /// Nothing to do.
    None,
    /// Close the detail pane.
    CloseDetail,
    /// Open detail for the cursor host.
    OpenDetail,
    /// Connect to the cursor host.
    Connect,
    /// Open exec for the cursor / selection.
    Exec,
    /// Toggle pause.
    TogglePause,
    /// Refresh the cursor host.
    RefreshOne,
    /// Refresh every host.
    RefreshAll,
    /// Start editing the filter.
    OpenSearch,
    /// Leave search editing and keep the filter.
    SearchSubmit,
    /// Clear the filter.
    SearchClear,
    /// Insert a character into the filter.
    SearchInsert(char),
    /// Delete the last filter character.
    SearchBackspace,
}

/// Fleet table state.
#[derive(Debug)]
pub struct HealthView {
    /// Host under the cursor.
    pub cursor_name: Option<String>,
    /// Filter text without a leading `/`.
    pub query: String,
    /// True while `/` search is capturing keys.
    pub editing: bool,
    /// Sort column.
    pub sort: SortKey,
    /// Reverse the sort when true.
    pub sort_desc: bool,
    /// Per-host reports and history.
    pub hosts: BTreeMap<String, HostHealth>,
    /// When true the scheduler should not start new polls.
    pub paused: bool,
    /// Configured refresh interval, for the stale marker and title.
    pub refresh_secs: u64,
    /// Open detail host, when Enter was pressed.
    pub detail: Option<String>,
    /// First visible table row.
    pub offset: usize,
    /// Rows that fit, updated while drawing.
    pub page: usize,
}

impl HealthView {
    /// Empty view with the configured refresh interval.
    pub fn new(refresh_secs: u64) -> Self {
        Self {
            cursor_name: None,
            query: String::new(),
            editing: false,
            sort: SortKey::Host,
            sort_desc: false,
            hosts: BTreeMap::new(),
            paused: false,
            refresh_secs,
            detail: None,
            offset: 0,
            page: 1,
        }
    }

    /// Seed from cached state-file reports so the first frame is not empty.
    pub fn load_cache(&mut self, cache: BTreeMap<String, HostHealth>, hosts: &[EffectiveHost]) {
        self.hosts = cache;
        if self.cursor_name.is_none() {
            self.cursor_name = hosts.first().map(|host| host.name.clone());
        }
    }

    /// Apply one scheduler update.
    pub fn apply(
        &mut self,
        host: String,
        report: crate::domain::health::HealthReport,
        point: Option<HistoryPoint>,
        history_limit: usize,
    ) {
        let entry = self.hosts.entry(host).or_default();
        if let Some(point) = point {
            entry.push_history(point, history_limit);
        }
        entry.report = Some(report);
    }

    /// Visible host names in sort order.
    pub fn visible(&self, hosts: &[EffectiveHost]) -> Vec<String> {
        let output = filter::filter(hosts, &self.query, Scope::All);
        let mut names: Vec<String> = output
            .matches
            .iter()
            .map(|matched| hosts[matched.index].name.clone())
            .collect();
        names.sort_by(|left, right| {
            let order = compare_hosts(left, right, &self.hosts, self.sort);
            if self.sort_desc {
                order.reverse()
            } else {
                order
            }
        });
        names
    }

    /// Move the cursor by `delta` rows among the visible list.
    pub fn move_by(&mut self, hosts: &[EffectiveHost], delta: isize) {
        let names = self.visible(hosts);
        if names.is_empty() {
            self.cursor_name = None;
            return;
        }
        let current = self
            .cursor_name
            .as_ref()
            .and_then(|name| names.iter().position(|row| row == name))
            .unwrap_or(0);
        let next = if delta < 0 {
            current.saturating_sub((-delta) as usize)
        } else {
            (current + delta as usize).min(names.len() - 1)
        };
        self.cursor_name = names.get(next).cloned();
        self.reveal(next);
    }

    /// Keep the selected row inside the viewport.
    pub fn reveal(&mut self, index: usize) {
        if index < self.offset {
            self.offset = index;
        } else if index >= self.offset + self.page {
            self.offset = index + 1 - self.page;
        }
    }

    /// Summary counts for the tab bar.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        status_counts(self.hosts.values().filter_map(|host| host.report.as_ref()))
    }
}

/// Handle a key while the health screen (or its search box) has focus.
pub fn on_key(view: &mut HealthView, code: KeyCode, hosts: &[EffectiveHost]) -> HealthEvent {
    if view.detail.is_some() {
        return match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                view.detail = None;
                HealthEvent::CloseDetail
            }
            _ => HealthEvent::None,
        };
    }
    if view.editing {
        return match code {
            KeyCode::Esc => {
                view.editing = false;
                view.query.clear();
                HealthEvent::SearchClear
            }
            KeyCode::Enter => {
                view.editing = false;
                HealthEvent::SearchSubmit
            }
            KeyCode::Backspace => {
                view.query.pop();
                HealthEvent::SearchBackspace
            }
            KeyCode::Char(ch) => {
                view.query.push(ch);
                HealthEvent::SearchInsert(ch)
            }
            _ => HealthEvent::None,
        };
    }
    match code {
        KeyCode::Char('/') => {
            view.editing = true;
            HealthEvent::OpenSearch
        }
        KeyCode::Char('s') => {
            view.sort = view.sort.next();
            HealthEvent::None
        }
        KeyCode::Char('S') => {
            view.sort_desc = !view.sort_desc;
            HealthEvent::None
        }
        KeyCode::Char('p') => {
            view.paused = !view.paused;
            HealthEvent::TogglePause
        }
        KeyCode::Char('r') => HealthEvent::RefreshOne,
        KeyCode::Char('R') => HealthEvent::RefreshAll,
        KeyCode::Char('c') => HealthEvent::Connect,
        KeyCode::Char('x') => HealthEvent::Exec,
        KeyCode::Enter => {
            if let Some(name) = view.cursor_name.clone() {
                view.detail = Some(name);
                HealthEvent::OpenDetail
            } else {
                HealthEvent::None
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            view.move_by(hosts, -1);
            HealthEvent::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            view.move_by(hosts, 1);
            HealthEvent::None
        }
        KeyCode::PageUp => {
            let page = view.page.max(1) as isize;
            view.move_by(hosts, -page);
            HealthEvent::None
        }
        KeyCode::PageDown => {
            let page = view.page.max(1) as isize;
            view.move_by(hosts, page);
            HealthEvent::None
        }
        KeyCode::Home => {
            view.move_by(hosts, isize::MIN / 4);
            HealthEvent::None
        }
        KeyCode::End => {
            view.move_by(hosts, isize::MAX / 4);
            HealthEvent::None
        }
        _ => HealthEvent::None,
    }
}

/// Draw the table or the detail pane.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &mut HealthView,
    hosts: &[EffectiveHost],
    theme: &Theme,
) {
    if let Some(name) = view.detail.clone() {
        draw_detail(frame, area, view, &name, theme);
        return;
    }
    let names = view.visible(hosts);
    if view
        .cursor_name
        .as_ref()
        .is_none_or(|name| !names.contains(name))
    {
        view.cursor_name = names.first().cloned();
    }
    let page = area.height.saturating_sub(3).max(1) as usize;
    view.page = page;
    if let Some(name) = view.cursor_name.clone()
        && let Some(index) = names.iter().position(|row| row == &name)
    {
        view.reveal(index);
    }

    let (ok, warn, crit, down) = view.counts();
    let total = hosts.len();
    let pause = if view.paused { " paused" } else { "" };
    let title = format!(
        " Health ─ {total} hosts ─ {ok} ✔ {warn} ⚠ {crit}✖ {down} ✖ ─ refresh {}s{pause} ─ sort: {} ",
        view.refresh_secs,
        view.sort.label()
    );
    let filter_note = if view.editing {
        format!(" /{}_ ", view.query)
    } else if !view.query.is_empty() {
        format!(" /{} ", view.query)
    } else {
        String::new()
    };

    let rows = names.iter().map(|name| {
        let report = view.hosts.get(name).and_then(|host| host.report.as_ref());
        let history = view
            .hosts
            .get(name)
            .map(|host| host.history.as_slices())
            .map(|(a, b)| {
                let mut values = Vec::with_capacity(a.len() + b.len());
                values.extend(a.iter().filter_map(|point| point.cpu_pct));
                values.extend(b.iter().filter_map(|point| point.cpu_pct));
                values
            })
            .unwrap_or_default();
        row_for(name, report, &history, theme)
    });
    let widths = [
        Constraint::Length(12),
        Constraint::Length(3),
        Constraint::Length(16),
        Constraint::Length(6),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(5),
    ];
    let header = Row::new(vec![
        "HOST",
        "ST",
        "CPU",
        "LOAD",
        "MEM",
        "DISK",
        "NET ↓/↑",
        "UP",
    ])
    .style(theme.header_row());
    let selected = view
        .cursor_name
        .as_ref()
        .and_then(|name| names.iter().position(|row| row == name));
    let table = Table::new(rows, widths)
        .header(header)
        .style(theme.text())
        .block(
            Block::bordered()
                .title(Span::styled(title, theme.title()))
                .title_bottom(Line::styled(filter_note, theme.muted()))
                .border_style(theme.border()),
        )
        .row_highlight_style(theme.selected());
    let mut state = TableState::new()
        .with_offset(view.offset)
        .with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
    view.offset = state.offset();
}

/// How to scale a history chart's Y axis.
#[derive(Debug, Clone, Copy)]
enum ChartScale {
    /// Always 0–100 so low usage still reads as “almost idle”.
    Percent,
    /// Fit the observed peak (network rates, etc.).
    AutoRate,
}

fn draw_detail(frame: &mut Frame, area: Rect, view: &HealthView, name: &str, theme: &Theme) {
    let host = view.hosts.get(name);
    let report = host.and_then(|host| host.report.as_ref());
    let history = host.map(|host| &host.history);
    let chunks = Layout::vertical([
        Constraint::Length(9),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
        Constraint::Percentage(33),
    ])
    .split(area);

    let summary = detail_summary(name, report, theme);
    frame.render_widget(
        Paragraph::new(summary)
            .block(
                Block::bordered()
                    .title(Span::styled(format!(" {name} "), theme.title()))
                    .border_style(theme.border()),
            )
            .wrap(Wrap { trim: false }),
        chunks[0],
    );

    draw_history_chart(
        frame,
        chunks[1],
        "CPU",
        history,
        |point| point.cpu_pct,
        theme.ok(),
        theme,
        ChartScale::Percent,
    );
    draw_history_chart(
        frame,
        chunks[2],
        "MEM",
        history,
        |point| point.mem_pct,
        theme.warning(),
        theme,
        ChartScale::Percent,
    );
    draw_history_chart(
        frame,
        chunks[3],
        "NET rx",
        history,
        |point| point.net_rx_bps,
        theme.title(),
        theme,
        ChartScale::AutoRate,
    );
}

fn detail_summary(name: &str, report: Option<&HealthReport>, theme: &Theme) -> Vec<Line<'static>> {
    match report {
        Some(report) if report.reachable => {
            let mut lines = vec![Line::from(vec![
                Span::styled(name.to_string(), theme.title()),
                Span::raw("  "),
                Span::styled(
                    report.status.symbol().to_string(),
                    status_style(report.status, theme),
                ),
                Span::styled(
                    format!("  {}", report.os.as_deref().unwrap_or("—")),
                    theme.muted(),
                ),
            ])];
            lines.push(usage_bar_line("cpu", report.cpu_pct, theme));
            lines.push(usage_bar_line("mem", report.mem_pct, theme));
            lines.push(usage_bar_line("disk", report.disk_pct, theme));
            lines.push(Line::from(vec![
                Span::styled("load ", theme.muted()),
                Span::styled(load(report.load1), theme.text()),
                Span::styled("   up ", theme.muted()),
                Span::styled(
                    report.uptime.clone().unwrap_or_else(|| "—".into()),
                    theme.text(),
                ),
                if let Some(failed) = report.failed_units {
                    Span::styled(format!("   failed units: {failed}"), theme.muted())
                } else {
                    Span::raw("")
                },
            ]));
            if report.reboot_required == Some(true) {
                lines.push(Line::styled("reboot required", theme.warning()));
            }
            for disk in &report.disks {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<10} ", disk.mount), theme.muted()),
                    Span::styled(
                        format!("{:<16}", bar(disk.used_pct, 16)),
                        usage_style(disk.used_pct, theme),
                    ),
                    Span::styled(
                        format!(" {:>3.0}%", disk.used_pct),
                        usage_style(disk.used_pct, theme),
                    ),
                ]));
            }
            lines
        }
        Some(report) => vec![Line::from(vec![
            Span::styled(name.to_string(), theme.title()),
            Span::styled("  ✖  ", theme.error()),
            Span::styled(
                report.error.clone().unwrap_or_else(|| "unreachable".into()),
                theme.error(),
            ),
        ])],
        None => vec![Line::from(vec![
            Span::styled(name.to_string(), theme.title()),
            Span::styled("  —  waiting for first sample", theme.muted()),
        ])],
    }
}

fn usage_bar_line(label: &str, value: Option<f64>, theme: &Theme) -> Line<'static> {
    let pct_val = value.unwrap_or(0.0);
    let style = usage_style(pct_val, theme);
    Line::from(vec![
        Span::styled(format!("{label:<4} "), theme.muted()),
        Span::styled(format!("{:<24}", bar(pct_val, 24)), style),
        Span::styled(format!(" {:>4}", pct(value)), style),
    ])
}

fn usage_style(pct: f64, theme: &Theme) -> Style {
    if pct >= 90.0 {
        theme.error()
    } else if pct >= 75.0 {
        theme.warning()
    } else {
        theme.ok()
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_history_chart(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    history: Option<&VecDeque<HistoryPoint>>,
    pick: impl Fn(&HistoryPoint) -> Option<f64>,
    style: Style,
    theme: &Theme,
    scale: ChartScale,
) {
    let values: Vec<f64> = history
        .map(|points| points.iter().filter_map(pick).collect())
        .unwrap_or_default();
    if values.is_empty() {
        frame.render_widget(
            Paragraph::new("no samples yet").style(theme.muted()).block(
                Block::bordered()
                    .title(Span::styled(format!(" {label} "), theme.title()))
                    .border_style(theme.border()),
            ),
            area,
        );
        return;
    }
    let current = values.last().copied().unwrap_or(0.0);
    let peak = values.iter().cloned().fold(0.0_f64, f64::max);
    let (y_max, y_labels, headline): (f64, Vec<Line<'static>>, String) = match scale {
        ChartScale::Percent => (
            100.0,
            vec![Line::from("0%"), Line::from("50%"), Line::from("100%")],
            format!("now {current:.0}%  peak {peak:.0}%"),
        ),
        ChartScale::AutoRate => {
            let max = peak.max(1.0) * 1.05;
            (
                max,
                vec![
                    Line::from("0"),
                    Line::from(format_rate(max / 2.0)),
                    Line::from(format_rate(max)),
                ],
                format!("now {}  peak {}", format_rate(current), format_rate(peak)),
            )
        }
    };
    let title = format!(" {label}  ·  {headline} ");
    let data: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64, *value))
        .collect();
    let dataset = Dataset::default()
        .marker(ratatui::symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(style)
        .data(&data);
    let chart = Chart::new(vec![dataset])
        .block(
            Block::bordered()
                .title(Span::styled(title, theme.title()))
                .border_style(theme.border()),
        )
        .x_axis(Axis::default().bounds([0.0, (data.len().saturating_sub(1).max(1)) as f64]))
        .y_axis(
            Axis::default()
                .bounds([0.0, y_max])
                .labels(y_labels)
                .style(theme.muted()),
        );
    frame.render_widget(chart, area);
}

fn row_for(
    name: &str,
    report: Option<&HealthReport>,
    cpu_history: &[f64],
    theme: &Theme,
) -> Row<'static> {
    let Some(report) = report else {
        return Row::new(vec![
            Cell::from(name.to_string()),
            Cell::from("·"),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
            Cell::from("—"),
        ]);
    };
    if !report.reachable {
        let age = stale_label(report);
        let message = format!(
            "{} ({age})",
            report.error.as_deref().unwrap_or("unreachable")
        );
        return Row::new(vec![
            Cell::from(name.to_string()),
            Cell::from(Span::styled("✖", theme.error())),
            Cell::from(message),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]);
    }
    let status_style = status_style(report.status, theme);
    let spark = sparkline(cpu_history, 8);
    let cpu = format!("{:>3} {spark}", pct(report.cpu_pct));
    let mem = format!(
        "{} {:>3}",
        bar(report.mem_pct.unwrap_or(0.0), 9),
        pct(report.mem_pct)
    );
    let disk = format!(
        "{} {:>3}",
        bar(report.disk_pct.unwrap_or(0.0), 9),
        pct(report.disk_pct)
    );
    let net = match (report.net_rx_bps, report.net_tx_bps) {
        (Some(rx), Some(tx)) => format!("{}/{}", format_rate(rx), format_rate(tx)),
        _ => "—".to_string(),
    };
    let up = report.uptime.clone().unwrap_or_else(|| "—".to_string());
    let host_label = if report.stale {
        format!("{name}*")
    } else {
        name.to_string()
    };
    Row::new(vec![
        Cell::from(host_label),
        Cell::from(Span::styled(
            report.status.symbol().to_string(),
            status_style,
        )),
        Cell::from(cpu),
        Cell::from(load(report.load1)),
        Cell::from(mem),
        Cell::from(disk),
        Cell::from(net),
        Cell::from(up),
    ])
    .style(if report.stale {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    })
}

fn compare_hosts(
    left: &str,
    right: &str,
    hosts: &BTreeMap<String, HostHealth>,
    sort: SortKey,
) -> std::cmp::Ordering {
    let left_report = hosts.get(left).and_then(|host| host.report.as_ref());
    let right_report = hosts.get(right).and_then(|host| host.report.as_ref());
    let order = match sort {
        SortKey::Host => left.cmp(right),
        SortKey::Status => status_rank(left_report).cmp(&status_rank(right_report)),
        SortKey::Cpu => cmp_opt(
            left_report.and_then(|r| r.cpu_pct),
            right_report.and_then(|r| r.cpu_pct),
        ),
        SortKey::Load => cmp_opt(
            left_report.and_then(|r| r.load1),
            right_report.and_then(|r| r.load1),
        ),
        SortKey::Mem => cmp_opt(
            left_report.and_then(|r| r.mem_pct),
            right_report.and_then(|r| r.mem_pct),
        ),
        SortKey::Disk => cmp_opt(
            left_report.and_then(|r| r.disk_pct),
            right_report.and_then(|r| r.disk_pct),
        ),
        SortKey::Net => cmp_opt(
            left_report.and_then(|r| r.net_rx_bps),
            right_report.and_then(|r| r.net_rx_bps),
        ),
        SortKey::Up => cmp_opt(
            left_report.and_then(|r| r.uptime_secs),
            right_report.and_then(|r| r.uptime_secs),
        ),
    };
    order.then_with(|| left.cmp(right))
}

fn status_rank(report: Option<&HealthReport>) -> u8 {
    match report.map(|report| report.status) {
        Some(HealthStatus::Crit) => 0,
        Some(HealthStatus::Unreachable) => 1,
        Some(HealthStatus::Warn) => 2,
        Some(HealthStatus::Ok) => 3,
        None => 4,
    }
}

fn cmp_opt(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn status_style(status: HealthStatus, theme: &Theme) -> Style {
    match status {
        HealthStatus::Ok => theme.ok(),
        HealthStatus::Warn => theme.warning(),
        HealthStatus::Crit | HealthStatus::Unreachable => theme.error(),
    }
}

fn pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.0}%", value))
        .unwrap_or_else(|| "—".to_string())
}

fn load(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "—".to_string())
}

fn stale_label(report: &HealthReport) -> String {
    let age = crate::domain::health::now_secs() - report.checked_at;
    if age < 60.0 {
        format!("{:.0}s ago", age.max(0.0))
    } else if age < 3600.0 {
        format!("{:.0}m ago", age / 60.0)
    } else {
        format!("{:.0}h ago", age / 3600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::health::{HealthReport, HealthStatus};

    fn host(name: &str) -> EffectiveHost {
        EffectiveHost {
            name: name.to_string(),
            address: "203.0.113.10".to_string(),
            user: "deploy".to_string(),
            port: 22,
            identity_file: None,
            jump: None,
            tags: vec!["web".to_string()],
            notes: None,
            ssh_options: BTreeMap::new(),
            disabled: false,
            connect_timeout: None,
            tunnels: Vec::new(),
        }
    }

    #[test]
    fn sort_and_filter_and_q_stays_out_of_search() {
        let hosts = vec![host("web-1"), host("db-1")];
        let mut view = HealthView::new(30);
        view.hosts.insert(
            "web-1".to_string(),
            HostHealth {
                report: Some(HealthReport {
                    checked_at: 1.0,
                    reachable: true,
                    error: None,
                    status: HealthStatus::Ok,
                    stale: false,
                    hostname: None,
                    kernel: None,
                    os: None,
                    uptime: Some("1d".into()),
                    uptime_secs: Some(86400.0),
                    load1: Some(0.1),
                    load_per_cpu: Some(0.1),
                    cpus: Some(1),
                    cpu_pct: Some(10.0),
                    mem_pct: Some(20.0),
                    swap_pct: None,
                    disk_pct: Some(30.0),
                    disks: Vec::new(),
                    net_rx_bps: None,
                    net_tx_bps: None,
                    failed_units: Some(0),
                    reboot_required: Some(false),
                }),
                ..HostHealth::default()
            },
        );
        view.cursor_name = Some("web-1".to_string());
        assert_eq!(
            on_key(&mut view, KeyCode::Char('s'), &hosts),
            HealthEvent::None
        );
        assert_eq!(view.sort, SortKey::Status);
        assert_eq!(
            on_key(&mut view, KeyCode::Char('p'), &hosts),
            HealthEvent::TogglePause
        );
        assert!(view.paused);
        assert_eq!(
            on_key(&mut view, KeyCode::Char('/'), &hosts),
            HealthEvent::OpenSearch
        );
        assert_eq!(
            on_key(&mut view, KeyCode::Char('q'), &hosts),
            HealthEvent::SearchInsert('q')
        );
        assert_eq!(view.query, "q");
    }
}
