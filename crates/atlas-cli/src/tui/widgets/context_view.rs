//! Symbol detail view with tabbed panels: Overview, Callers, Callees, Source.
//!
//! Pure rendering — reads pre-computed `ContextView` and tab/scroll state,
//! never accesses Store or GraphEngine.

use atlas_engine::{CalleeDetail, CallerDetail, ContextView, SymbolDef};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};

/// Which tab is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Callers,
    Callees,
    Peers,
    Source,
}

impl DetailTab {
    pub fn next(self) -> Self {
        match self {
            DetailTab::Overview => DetailTab::Callers,
            DetailTab::Callers => DetailTab::Callees,
            DetailTab::Callees => DetailTab::Peers,
            DetailTab::Peers => DetailTab::Source,
            DetailTab::Source => DetailTab::Overview,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DetailTab::Overview => "Overview",
            DetailTab::Callers => "Callers",
            DetailTab::Callees => "Callees",
            DetailTab::Peers => "Peers",
            DetailTab::Source => "Source",
        }
    }
}

/// Render the symbol detail view in the given area.
///
/// - `context`: the ContextView for the current symbol.
/// - `tab`: which tab is active.
/// - `selected`: which caller/callee item is highlighted (for Callers/Callees tabs).
/// - `scroll`: vertical scroll offset (pre-clamped by caller so selected is visible).
/// - `focus_note`: short focus/partial or tool state for embedding (e.g. "focus:usable_partial"), shown in overview.
///   Keeps hybrid state visible even inside detail without relying solely on bottom tool bar or global status.
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    context: &ContextView,
    tab: DetailTab,
    selected: usize,
    scroll: usize,
    focus_note: &str,
) {
    // Vertical: tab bar (1) + tab content.
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    // ── Tab bar ─────────────────────────────────────────────────────────
    let titles: Vec<Line> = [
        DetailTab::Overview,
        DetailTab::Callers,
        DetailTab::Callees,
        DetailTab::Peers,
        DetailTab::Source,
    ]
    .iter()
    .map(|t| {
        let label = format!(" {} ", t.as_str());
        let style = if *t == tab {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        Line::from(Span::styled(label, style))
    })
    .collect();

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::BOTTOM))
        .style(Style::default().fg(Color::White));
    frame.render_widget(tabs, v_chunks[0]);

    // ── Tab content ─────────────────────────────────────────────────────
    match tab {
        DetailTab::Overview => render_overview(frame, v_chunks[1], context, focus_note),
        DetailTab::Callers => render_caller_list(
            frame,
            v_chunks[1],
            &context.caller_details,
            selected,
            scroll,
        ),
        DetailTab::Callees => render_callee_list(
            frame,
            v_chunks[1],
            &context.callee_details,
            selected,
            scroll,
        ),
        DetailTab::Peers => {
            render_peer_list(frame, v_chunks[1], &context.file_peers, selected, scroll)
        }
        DetailTab::Source => render_source(frame, v_chunks[1], context),
    }
}

fn render_overview(frame: &mut ratatui::Frame, area: Rect, ctx: &ContextView, focus_note: &str) {
    let subject = &ctx.subject;
    let file_path = ctx.subject_file_path.as_deref().unwrap_or("(unknown)");

    let kind_str = format!("{:?}", subject.kind);
    let sig = subject.signature.as_deref().unwrap_or("(no signature)");
    let vis = subject
        .visibility
        .as_ref()
        .map(|v| format!("{v:?}"))
        .unwrap_or_else(|| "default".to_string());

    let mut lines = vec![
        Line::from(Span::styled(
            format!("  {}", subject.qualified_name),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  Kind:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(kind_str, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  File:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(file_path, Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("  Line:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", subject.range.start_line),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Signature: ", Style::default().fg(Color::DarkGray)),
            Span::styled(sig, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Visibility:", Style::default().fg(Color::DarkGray)),
            Span::styled(vis, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Exported:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if subject.exported { "yes" } else { "no" },
                Style::default().fg(Color::White),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!(
                "  {} callers  |  {} callees  |  {} peers",
                ctx.callers.len(),
                ctx.callees.len(),
                ctx.file_peers.len()
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    if !focus_note.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("  [focus: {}]", focus_note),
            Style::default().fg(Color::Yellow),
        )));
    }

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn render_caller_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    callers: &[CallerDetail],
    selected: usize,
    scroll: usize,
) {
    let list_height = area.height.saturating_sub(2) as usize;
    if list_height == 0 {
        return;
    }

    let items: Vec<ListItem> = callers
        .iter()
        .skip(scroll)
        .take(list_height)
        .enumerate()
        .map(|(i, c)| {
            let global_idx = scroll + i;
            let is_sel = global_idx == selected;

            let name_style = if is_sel {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(vec![Line::from(vec![
                Span::styled(
                    if is_sel { "> " } else { "  " },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&c.symbol.name, name_style),
                Span::styled(
                    format!("  ({}:{}", c.symbol.qualified_name, c.callsite_line),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {:?})", c.edge_kind),
                    Style::default().fg(Color::Cyan),
                ),
            ])])
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Callers ({}) ", callers.len())),
    );
    frame.render_widget(list, area);
}

fn render_callee_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    callees: &[CalleeDetail],
    selected: usize,
    scroll: usize,
) {
    let list_height = area.height.saturating_sub(2) as usize;
    if list_height == 0 {
        return;
    }

    let items: Vec<ListItem> = callees
        .iter()
        .skip(scroll)
        .take(list_height)
        .enumerate()
        .map(|(i, c)| {
            let global_idx = scroll + i;
            let is_sel = global_idx == selected;

            let name_style = if is_sel {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(vec![Line::from(vec![
                Span::styled(
                    if is_sel { "> " } else { "  " },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&c.symbol.name, name_style),
                Span::styled(
                    format!("  → {}:{}", c.symbol.qualified_name, c.callsite_line),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {:?}", c.edge_kind),
                    Style::default().fg(Color::Magenta),
                ),
            ])])
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Callees ({}) ", callees.len())),
    );
    frame.render_widget(list, area);
}

fn render_peer_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    peers: &[SymbolDef],
    selected: usize,
    scroll: usize,
) {
    let list_height = area.height.saturating_sub(2) as usize;
    if list_height == 0 {
        return;
    }

    let items: Vec<ListItem> = peers
        .iter()
        .skip(scroll)
        .take(list_height)
        .enumerate()
        .map(|(i, p)| {
            let global_idx = scroll + i;
            let is_sel = global_idx == selected;

            let name_style = if is_sel {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(vec![Line::from(vec![
                Span::styled(
                    if is_sel { "> " } else { "  " },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&p.name, name_style),
                Span::styled(
                    format!("  {:?}", p.kind),
                    Style::default().fg(Color::DarkGray),
                ),
            ])])
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Peers ({}) ", peers.len())),
    );
    frame.render_widget(list, area);
}

fn render_source(frame: &mut ratatui::Frame, area: Rect, ctx: &ContextView) {
    let source = match &ctx.subject_source {
        Some(s) => s,
        None => {
            let message = match ctx.subject_file_path.as_deref() {
                Some(path) => format!(
                    "Source not available\n\n{path}\n\nThe file may have moved or the index may be stale. Run `atlas sync` to refresh the project index."
                ),
                None => "Source not available\n\nNo source file path is recorded for this symbol."
                    .to_string(),
            };
            let p = Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: false });
            frame.render_widget(p, area);
            return;
        }
    };

    let lines: Vec<Line> = source
        .lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let line_no = source.start_line as usize + i;
            Line::from(vec![
                Span::styled(
                    format!("{:>4} ", line_no + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(text, Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Source "))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}
