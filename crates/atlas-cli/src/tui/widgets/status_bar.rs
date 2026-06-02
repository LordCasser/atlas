//! Bottom status bar widget — displays project stats and graph state.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
};

/// Render the status bar at the given area.
///
/// Reads pre-fetched stats and graph state; performs **no I/O**.
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
    graph_ready: bool,
    additional: &str,
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

    text.push_str(" | Esc back/confirm | Ctrl-C quit");

    let status = Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Gray));
    frame.render_widget(status, area);
}
