//! Bottom status bar widget — displays project stats and graph state.
//!
//! Extended for focus/MCP parity: accepts an analysis HUD summary string
//! (precision, work, gaps) in addition to the classic "Index: ..." info.
//! The HUD is rendered compactly to fit terminal real-estate while making
//! partial/focus state visible (per recommended hybrid design).

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
};

/// Render the status bar at the given area.
///
/// Reads pre-fetched stats and graph state; performs **no I/O**.
///
/// `analysis_hud`: compact summary for focus-aware state, e.g.
/// "P:local(0.65) work:refine+3 gaps:1". Empty = no extra info.
/// This is the visual counterpart to MCP AnalysisEnvelope/precision/work/gaps.
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
    graph_ready: bool,
    additional: &str,   // legacy, e.g. "Index: structural"
    analysis_hud: &str, // new focus HUD (precision + work + gaps)
) {
    let graph_status = if graph_ready {
        "Graph: Ready"
    } else {
        "Graph: Initializing..."
    };

    let mut text = format!(
        " {file_count} files | {symbol_count} symbols | {edge_count} edges | {graph_status}"
    );

    if !additional.is_empty() {
        text.push_str(" | ");
        text.push_str(additional);
    }

    if !analysis_hud.is_empty() {
        text.push_str(" | ");
        text.push_str(analysis_hud);
    }

    text.push_str(" | Esc back/confirm | Ctrl-C quit | / search");

    let status = Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Gray));
    frame.render_widget(status, area);
}
