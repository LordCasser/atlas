//! Caller-path trace view — renders a `CallerChain` from root to target.
//!
//! Pure rendering — reads pre-computed `CallerChain` and scroll/selection state,
//! never accesses Store or GraphEngine.

use atlas_engine::CallerChain;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Render the caller-path trace view in the given area.
///
/// - `chain`: the caller chain to display (root → ... → target).
/// - `selected`: which step is highlighted (0-based index into chain.steps).
/// - `scroll`: vertical scroll offset (pre-clamped so selected is visible).
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    chain: &CallerChain,
    selected: usize,
    scroll: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Caller-Path Trace ")
        .style(Style::default());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let list_height = inner.height as usize;
    if list_height == 0 {
        return;
    }

    // Build all display lines, then skip to scroll position.
    let mut lines: Vec<Line> = Vec::new();

    // ── Root ─────────────────────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::styled("  Root: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &chain.root.name,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ({})", chain.root.qualified_name),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::default());

    // ── Steps ────────────────────────────────────────────────────────────
    for step in &chain.steps {
        let step_idx = step.index as usize;

        let marker = if step_idx == selected { "> " } else { "  " };
        let marker_style = if step_idx == selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let name_style = if step_idx == selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        // Main step line: "> description  [EdgeKind]"
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), marker_style),
            Span::styled(&step.description, name_style),
            Span::styled(
                format!("  [{:?}]", step.edge_kind),
                Style::default().fg(Color::Cyan),
            ),
        ]));

        // Callsite line: "  callsite: file_id_hex:line"
        let callsite_line = if let Some(ref range) = step.range {
            format!(
                "  callsite: {}:{}",
                step.file_id.short_hex(),
                range.start_line + 1
            )
        } else {
            format!("  callsite: {}:?", step.file_id.short_hex())
        };
        lines.push(Line::from(Span::styled(
            callsite_line,
            Style::default().fg(Color::DarkGray),
        )));

        // Caller snippet.
        if let Some(ref snippet) = step.caller_snippet {
            for snip_line in snippet.lines() {
                lines.push(Line::from(Span::styled(
                    format!("    {snip_line}"),
                    Style::default().fg(Color::Gray),
                )));
            }
        }

        // Callee snippet.
        if let Some(ref snippet) = step.callee_snippet {
            for snip_line in snippet.lines() {
                lines.push(Line::from(Span::styled(
                    format!("    ▶ {snip_line}"),
                    Style::default().fg(Color::Green),
                )));
            }
        }

        // Blank separator between steps.
        lines.push(Line::default());
    }

    // ── Target ───────────────────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::styled("  Target: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &chain.target.name,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ({})", chain.target.qualified_name),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    lines.push(Line::default());

    // ── Stats footer ─────────────────────────────────────────────────────
    let truncated = if chain.truncated { "yes" } else { "no" };
    let stats = format!(
        "  Nodes visited: {}  |  Max depth: {}  |  Truncated: {}",
        chain.nodes_visited, chain.max_depth_reached, truncated
    );
    lines.push(Line::from(Span::styled(
        stats,
        Style::default().fg(Color::DarkGray),
    )));

    // Render visible slice.
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(list_height).collect();

    let paragraph = Paragraph::new(visible);
    frame.render_widget(paragraph, inner);
}
