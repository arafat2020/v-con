//! Hosts screen: tag tree, searchable table, and detail pane.
//!
//! Filtering lives in [`crate::domain::filter`]. This module owns cursor
//! movement, multi-select (stored by host name), and drawing. The detail pane
//! hides when the terminal is narrower than 70 columns or shorter than 20 rows.
//! The tag tree hides below 60 columns. `recent` and `favorites` list hosts
//! recorded in the state file. `recent` is ordered newest first.

use crate::domain::filter::{Field, FilterOutput, Scope, cursor_position, filter};
use crate::domain::host::EffectiveHost;
use crate::infra::ssh::command::{SshCommandOptions, build_ssh_command, command_line, jump_chain};
use crate::tui::action::Action;
use crate::tui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState, Wrap,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use tui_tree_widget::{Tree, TreeItem, TreeState};

/// Identity of one tag-tree row. Counts are rendered into the label, not stored here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// Every host.
    All,
    /// One inventory tag.
    Tag(String),
    /// Empty until phase 2.
    Recent,
    /// Empty until phase 2.
    Favorites,
}

/// Owned copy of [`crate::domain::filter::Scope`] held by the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostScope {
    /// No tag filter.
    All,
    /// Only hosts with this tag.
    Tag(String),
    /// Empty until phase 2.
    Recent,
    /// Empty until phase 2.
    Favorites,
}

/// Which pane receives `j`/`k`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The tag tree.
    Tree,
    /// The host table.
    Table,
}

/// Cursor, filter, and selection for the Hosts screen.
///
/// `cursor_name` and `selected` use host names so they stay valid when the
/// filter changes the visible rows.
#[derive(Debug)]
pub struct HostsView {
    /// Current search text, without the leading `/`.
    pub query: String,
    /// True while the search box is capturing keystrokes.
    pub editing: bool,
    /// Tag-tree filter applied before the text query.
    pub scope: HostScope,
    /// Selected tree node. Kept in sync with `scope`.
    pub node: NodeId,
    /// Host under the cursor. `None` when the visible list is empty.
    pub cursor_name: Option<String>,
    /// Multi-selected host names. `*` marks them in the table.
    pub selected: BTreeSet<String>,
    /// Tree or table.
    pub focus: Focus,
    /// Widget state for the tag tree. The selected id is [`NodeId`].
    pub tree: TreeState<NodeId>,
    /// First visible table row.
    pub table_offset: usize,
    /// Rows that fit in the table, updated while drawing. Page keys use it.
    pub table_page: usize,
    /// Host names in the recent group, newest first.
    pub recent_order: Vec<String>,
    /// Membership of the recent group.
    pub recent: BTreeSet<String>,
    /// Membership of the favorites group.
    pub favorites: BTreeSet<String>,
    /// Compact last-connected labels keyed by host name.
    pub last_seen: BTreeMap<String, String>,
    /// Frecency scores. Missing names sort as zero.
    pub scores: BTreeMap<String, u64>,
    /// When true, the all and tag tables sort by [`Self::scores`].
    pub sort_frecency: bool,
}

/// Theme, paths, and the flags that decide whether the detail pane is shown.
pub struct ViewOptions<'a> {
    /// Colours for the selection, warnings, and fuzzy highlights.
    pub theme: &'a Theme,
    /// Shown in the empty-state message.
    pub hosts_path: &'a Path,
    /// Config flag. The pane also hides on a small terminal.
    pub show_detail: bool,
    /// Full terminal width, not the table's width.
    pub terminal_width: u16,
    /// Full terminal height.
    pub terminal_height: u16,
    /// When true, the detail command includes ControlMaster options.
    pub multiplexing: bool,
    /// Control socket directory used in that command.
    pub control_dir: &'a Path,
}

impl HostsView {
    /// Start on `all`, with the cursor on the first host.
    pub fn new(hosts: &[EffectiveHost]) -> Self {
        let mut tree = TreeState::default();
        tree.select(vec![NodeId::All]);
        Self {
            query: String::new(),
            editing: false,
            scope: HostScope::All,
            node: NodeId::All,
            cursor_name: hosts.first().map(|host| host.name.clone()),
            selected: BTreeSet::new(),
            focus: Focus::Table,
            tree,
            table_offset: 0,
            table_page: 10,
            recent_order: Vec::new(),
            recent: BTreeSet::new(),
            favorites: BTreeSet::new(),
            last_seen: BTreeMap::new(),
            scores: BTreeMap::new(),
            sort_frecency: false,
        }
    }

    /// Replace the recent, favorite, and frecency data the table sorts with.
    pub fn apply_groups(
        &mut self,
        recent_order: Vec<String>,
        favorites: BTreeSet<String>,
        last_seen: BTreeMap<String, String>,
        scores: BTreeMap<String, u64>,
    ) {
        self.recent = recent_order.iter().cloned().collect();
        self.recent_order = recent_order;
        self.favorites = favorites;
        self.last_seen = last_seen;
        self.scores = scores;
    }

    /// Put the cursor back on a visible row after the inventory changes.
    pub fn refresh(&mut self, hosts: &[EffectiveHost]) {
        self.restabilize(hosts);
    }

    /// Apply a host-list action. Returns false for actions the shell handles
    /// (quit, help, copy, connect).
    pub fn handle(&mut self, action: &Action, hosts: &[EffectiveHost]) -> bool {
        match action {
            Action::FocusTree => self.focus = Focus::Tree,
            Action::FocusTable => self.focus = Focus::Table,
            Action::MoveUp => self.move_focused(hosts, -1),
            Action::MoveDown => self.move_focused(hosts, 1),
            Action::MoveTop => self.move_edge(hosts, true),
            Action::MoveBottom => self.move_edge(hosts, false),
            Action::PageUp => self.move_focused(hosts, -(self.table_page.max(1) as isize)),
            Action::PageDown => self.move_focused(hosts, self.table_page.max(1) as isize),
            Action::OpenSearch => self.editing = true,
            Action::SearchInsert(ch) => {
                self.query.push(*ch);
                self.restabilize(hosts);
            }
            Action::SearchBackspace => {
                self.query.pop();
                self.restabilize(hosts);
            }
            Action::SearchSubmit => self.editing = false,
            Action::SearchClear => {
                self.query.clear();
                self.editing = false;
                self.restabilize(hosts);
            }
            Action::ToggleSelect => {
                if let Some(name) = self.cursor_name.clone()
                    && !self.selected.remove(&name)
                {
                    self.selected.insert(name);
                }
            }
            Action::SelectAll => {
                for matched in self.output(hosts).matches {
                    self.selected.insert(hosts[matched.index].name.clone());
                }
            }
            Action::ClearSelection => self.selected.clear(),
            _ => return false,
        }
        true
    }

    /// The effective host under the cursor.
    pub fn cursor<'a>(&self, hosts: &'a [EffectiveHost]) -> Option<&'a EffectiveHost> {
        self.cursor_name
            .as_ref()
            .and_then(|name| hosts.iter().find(|host| &host.name == name))
    }

    /// Rows visible with the current tag and search query.
    ///
    /// The recent group is newest-first. Other groups stay in inventory order
    /// unless frecency sorting is on.
    pub fn output(&self, hosts: &[EffectiveHost]) -> FilterOutput {
        let scope = match &self.scope {
            HostScope::All => Scope::All,
            HostScope::Tag(tag) => Scope::Tag(tag),
            HostScope::Recent => Scope::Recent(&self.recent),
            HostScope::Favorites => Scope::Favorites(&self.favorites),
        };
        let mut output = filter(hosts, &self.query, scope);
        match &self.scope {
            HostScope::Recent => {
                output.matches.sort_by_key(|matched| {
                    self.recent_order
                        .iter()
                        .position(|name| name == &hosts[matched.index].name)
                        .unwrap_or(usize::MAX)
                });
            }
            _ if self.sort_frecency => {
                output.matches.sort_by(|left, right| {
                    let left_score = self
                        .scores
                        .get(&hosts[left.index].name)
                        .copied()
                        .unwrap_or(0);
                    let right_score = self
                        .scores
                        .get(&hosts[right.index].name)
                        .copied()
                        .unwrap_or(0);
                    right_score
                        .cmp(&left_score)
                        .then(left.index.cmp(&right.index))
                });
            }
            _ => {}
        }
        output
    }

    fn move_focused(&mut self, hosts: &[EffectiveHost], delta: isize) {
        if self.focus == Focus::Tree {
            self.move_tree(hosts, delta);
        } else {
            self.move_table(hosts, delta);
        }
    }

    fn move_edge(&mut self, hosts: &[EffectiveHost], top: bool) {
        if self.focus == Focus::Tree {
            let nodes = self.nodes(hosts);
            if let Some((node, _)) = if top { nodes.first() } else { nodes.last() } {
                self.select_node(node.clone(), hosts);
            }
        } else {
            let output = self.output(hosts);
            if output.matches.is_empty() {
                return;
            }
            let position = if top { 0 } else { output.matches.len() - 1 };
            self.cursor_name = Some(hosts[output.matches[position].index].name.clone());
            self.reveal(position);
        }
    }

    fn move_table(&mut self, hosts: &[EffectiveHost], delta: isize) {
        let output = self.output(hosts);
        if output.matches.is_empty() {
            self.cursor_name = None;
            return;
        }
        let current =
            cursor_position(self.cursor_name.as_deref(), &output.matches, hosts).unwrap_or(0);
        let last = output.matches.len() as isize - 1;
        let next = (current as isize + delta).clamp(0, last) as usize;
        self.cursor_name = Some(hosts[output.matches[next].index].name.clone());
        self.reveal(next);
    }

    fn move_tree(&mut self, hosts: &[EffectiveHost], delta: isize) {
        let nodes = self.nodes(hosts);
        if nodes.is_empty() {
            return;
        }
        let current = nodes
            .iter()
            .position(|(id, _)| id == &self.node)
            .unwrap_or(0);
        let last = nodes.len() as isize - 1;
        let next = (current as isize + delta).clamp(0, last) as usize;
        self.select_node(nodes[next].0.clone(), hosts);
    }

    fn select_node(&mut self, node: NodeId, hosts: &[EffectiveHost]) {
        self.scope = match &node {
            NodeId::All => HostScope::All,
            NodeId::Tag(tag) => HostScope::Tag(tag.clone()),
            NodeId::Recent => HostScope::Recent,
            NodeId::Favorites => HostScope::Favorites,
        };
        self.node = node.clone();
        self.tree.select(vec![node]);
        self.restabilize(hosts);
    }

    fn nodes(&self, hosts: &[EffectiveHost]) -> Vec<(NodeId, String)> {
        node_list(hosts, self.recent.len(), self.favorites.len())
    }

    fn restabilize(&mut self, hosts: &[EffectiveHost]) {
        let output = self.output(hosts);
        match cursor_position(self.cursor_name.as_deref(), &output.matches, hosts) {
            Some(position) => {
                self.cursor_name = Some(hosts[output.matches[position].index].name.clone());
                self.reveal(position);
            }
            None => {
                self.cursor_name = None;
                self.table_offset = 0;
            }
        }
    }

    fn reveal(&mut self, position: usize) {
        let page = self.table_page.max(1);
        if position < self.table_offset {
            self.table_offset = position;
        } else if position >= self.table_offset + page {
            self.table_offset = position + 1 - page;
        }
    }
}

/// Draw the tag tree, the table, and the detail pane into `area`.
///
/// `area` is the space under the tab bar and above the hint line. The tree
/// and detail pane are omitted when the terminal is too small.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    view: &mut HostsView,
    hosts: &[EffectiveHost],
    options: &ViewOptions<'_>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let show_tree = options.terminal_width >= 60;
    let show_detail =
        options.show_detail && options.terminal_width >= 70 && options.terminal_height >= 20;
    if !show_tree {
        view.focus = Focus::Table;
    }

    let (main, detail) = if show_detail {
        let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(8)]).split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };
    let (tree_area, table_area) = if show_tree {
        let chunks = Layout::horizontal([Constraint::Length(24), Constraint::Min(10)]).split(main);
        (Some(chunks[0]), chunks[1])
    } else {
        (None, main)
    };

    if let Some(tree_area) = tree_area {
        draw_tree(frame, tree_area, view, hosts, options);
    }
    draw_table(frame, table_area, view, hosts, options);
    if let Some(detail) = detail {
        draw_detail(frame, detail, view, hosts, options);
    }
}

fn draw_tree(
    frame: &mut Frame,
    area: Rect,
    view: &mut HostsView,
    hosts: &[EffectiveHost],
    options: &ViewOptions<'_>,
) {
    let nodes = node_list(hosts, view.recent.len(), view.favorites.len());
    let _ = view.tree.select(vec![view.node.clone()]);
    let items: Vec<TreeItem<NodeId>> = nodes
        .iter()
        .map(|(id, label)| TreeItem::new_leaf(id.clone(), label.clone()))
        .collect();
    let tree = Tree::new(&items)
        .expect("tag identifiers are unique")
        .block(Block::bordered().title(" Tags "))
        .highlight_style(if view.focus == Focus::Tree {
            options.theme.selected()
        } else {
            Style::new().add_modifier(ratatui::style::Modifier::BOLD)
        })
        .highlight_symbol("");
    frame.render_stateful_widget(tree, area, &mut view.tree);
}

fn draw_table(
    frame: &mut Frame,
    area: Rect,
    view: &mut HostsView,
    hosts: &[EffectiveHost],
    options: &ViewOptions<'_>,
) {
    let output = view.output(hosts);
    let page = area.height.saturating_sub(3).max(1) as usize;
    view.table_page = page;
    if let Some(name) = view.cursor_name.clone()
        && let Some(position) = cursor_position(Some(&name), &output.matches, hosts)
    {
        view.reveal(position);
    }

    let title = table_title(view, output.matches.len());
    let notice = output
        .selector_error
        .clone()
        .or_else(|| output.warnings.first().cloned());
    let mut block = Block::bordered().title(title);
    if let Some(notice) = notice.as_ref() {
        block = block.title_bottom(Line::styled(format!(" {notice} "), options.theme.error()));
    }

    if hosts.is_empty() {
        let message = format!(
            "No hosts yet — press `i` to import from ~/.ssh/config or `a` to add one (phase 3), or edit {}",
            options.hosts_path.display()
        );
        frame.render_widget(
            Paragraph::new(message)
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    if output.matches.is_empty() {
        let message = output
            .selector_error
            .clone()
            .unwrap_or_else(|| "No hosts match this filter.".to_string());
        frame.render_widget(
            Paragraph::new(message)
                .style(options.theme.error())
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let compact = area.width < 56;
    let cursor = view.cursor_name.as_deref();
    let position = cursor_position(cursor, &output.matches, hosts);
    let rows = output.matches.iter().map(|matched| {
        let host = &hosts[matched.index];
        let gutter = gutter(&host.name, cursor, &view.selected);
        if compact {
            Row::new(vec![
                Cell::from(Line::from(highlighted(
                    &format!("{gutter}{}", host.name),
                    &[],
                    options.theme,
                ))),
                Cell::from(field_line(host, matched, Field::Address, options.theme)),
            ])
        } else {
            Row::new(vec![
                Cell::from(Line::from(named(
                    &gutter,
                    host,
                    matched,
                    view.favorites.contains(&host.name),
                    options.theme,
                ))),
                Cell::from(field_line(host, matched, Field::Address, options.theme)),
                Cell::from(host.user.as_str()),
                Cell::from(field_line(host, matched, Field::Tags, options.theme)),
                Cell::from(
                    view.last_seen
                        .get(&host.name)
                        .map(String::as_str)
                        .unwrap_or("—"),
                ),
            ])
        }
    });
    let widths: Vec<Constraint> = if compact {
        vec![Constraint::Min(8), Constraint::Min(8)]
    } else {
        vec![
            Constraint::Length(20),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Min(8),
            Constraint::Length(4),
        ]
    };
    let header = if compact {
        Row::new(vec!["NAME", "ADDRESS"])
    } else {
        Row::new(vec!["NAME", "ADDRESS", "USER", "TAGS", "LAST"])
    };
    let table = Table::new(rows, widths)
        .header(header.style(options.theme.muted()))
        .block(block)
        .row_highlight_style(options.theme.selected());
    let mut state = TableState::new()
        .with_offset(view.table_offset)
        .with_selected(position);
    frame.render_stateful_widget(table, area, &mut state);
    view.table_offset = state.offset();

    if output.matches.len() > page {
        let mut scrollbar = ScrollbarState::new(output.matches.len().saturating_sub(page))
            .position(view.table_offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut scrollbar,
        );
    }
}

fn draw_detail(
    frame: &mut Frame,
    area: Rect,
    view: &HostsView,
    hosts: &[EffectiveHost],
    options: &ViewOptions<'_>,
) {
    let Some(host) = view.cursor(hosts) else {
        frame.render_widget(
            Paragraph::new("No host selected.").block(Block::bordered().title(" Detail ")),
            area,
        );
        return;
    };
    let chain = jump_chain(host, hosts);
    let via = if chain.is_empty() {
        format!("{}:{}", host.destination(), host.port)
    } else {
        format!(
            "{}:{} via {}",
            host.destination(),
            host.port,
            chain.join(" → ")
        )
    };
    let identity = host.identity_file.as_deref().unwrap_or("ssh default");
    let notes = host.notes.as_deref().unwrap_or("—");
    let args = build_ssh_command(
        host,
        hosts,
        &SshCommandOptions {
            batch_mode: false,
            connect_timeout: None,
            multiplexing: options.multiplexing,
            control_path_dir: options
                .multiplexing
                .then(|| options.control_dir.display().to_string()),
        },
    );
    let text = format!(
        "{via}\nidentity: {identity}\ntags: {}\nnotes: {notes}\n{}",
        host.tags.join(", "),
        command_line(&args)
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::bordered().title(" Detail "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn table_title(view: &HostsView, visible: usize) -> String {
    let mut title = format!(" Hosts ─ {visible} ");
    if !view.selected.is_empty() {
        title.push_str(&format!("─ {} selected ", view.selected.len()));
    }
    if view.editing || !view.query.is_empty() {
        title.push_str(&format!("─ /{}", view.query));
    }
    title
}

fn gutter(name: &str, cursor: Option<&str>, selected: &BTreeSet<String>) -> String {
    match (cursor == Some(name), selected.contains(name)) {
        (true, true) => ">* ".to_string(),
        (true, false) => ">  ".to_string(),
        (false, true) => "*  ".to_string(),
        (false, false) => "   ".to_string(),
    }
}

fn named<'a>(
    gutter: &str,
    host: &EffectiveHost,
    matched: &crate::domain::filter::HostMatch,
    favorite: bool,
    theme: &Theme,
) -> Vec<Span<'a>> {
    let mut spans = name_spans(gutter, host, matched, theme);
    if favorite {
        spans.push(Span::raw(" *"));
    }
    spans
}

fn name_spans<'a>(
    gutter: &str,
    host: &EffectiveHost,
    matched: &crate::domain::filter::HostMatch,
    theme: &Theme,
) -> Vec<Span<'a>> {
    let mut spans = vec![Span::raw(gutter.to_string())];
    let indices = field_indices(matched, Field::Name);
    spans.extend(highlighted(&host.name, indices, theme));
    spans
}

fn field_line<'a>(
    host: &EffectiveHost,
    matched: &crate::domain::filter::HostMatch,
    field: Field,
    theme: &Theme,
) -> Line<'a> {
    let text = match field {
        Field::Name => host.name.clone(),
        Field::Address => host.address.clone(),
        Field::Tags => host.tags.join(","),
        Field::Notes => host.notes.clone().unwrap_or_default(),
    };
    Line::from(highlighted(&text, field_indices(matched, field), theme))
}

fn field_indices(matched: &crate::domain::filter::HostMatch, field: Field) -> &[u32] {
    matched
        .spans
        .iter()
        .find(|span| span.field == field)
        .map(|span| span.indices.as_slice())
        .unwrap_or(&[])
}

fn highlighted<'a>(text: &str, indices: &[u32], theme: &Theme) -> Vec<Span<'a>> {
    let hits: HashSet<u32> = indices.iter().copied().collect();
    if hits.is_empty() {
        return vec![Span::raw(text.to_string())];
    }
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut active = false;
    for (index, ch) in text.chars().enumerate() {
        let hit = hits.contains(&(index as u32));
        if hit != active && !buf.is_empty() {
            let style = if active {
                theme.match_style()
            } else {
                Style::new()
            };
            spans.push(Span::styled(std::mem::take(&mut buf), style));
        }
        active = hit;
        buf.push(ch);
    }
    if !buf.is_empty() {
        let style = if active {
            theme.match_style()
        } else {
            Style::new()
        };
        spans.push(Span::styled(buf, style));
    }
    spans
}

/// Tag-tree rows: `all`, then each tag alphabetically with a count, then
/// `recent` and `favorites`.
pub fn node_list(
    hosts: &[EffectiveHost],
    recent: usize,
    favorites: usize,
) -> Vec<(NodeId, String)> {
    let mut counts = BTreeMap::<String, usize>::new();
    for host in hosts {
        for tag in &host.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut nodes = vec![(NodeId::All, format!("all ({})", hosts.len()))];
    for (tag, count) in counts {
        nodes.push((NodeId::Tag(tag.clone()), format!("{tag} ({count})")));
    }
    nodes.push((NodeId::Recent, format!("recent ({recent})")));
    nodes.push((NodeId::Favorites, format!("favorites ({favorites})")));
    nodes
}
