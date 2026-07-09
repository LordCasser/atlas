//! Bottom status bar widget — displays project stats and graph state.
//!

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
};

/// Render the status bar at the given area.
///
/// Reads pre-fetched stats and graph state; performs **no I/O**.
///
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    counts: (i64, i64, i64),
    graph_ready: bool,
    catalog_tier: &str,
    activity: &str,
) {
    let (file_count, symbol_count, edge_count) = counts;
    let graph = if graph_ready { "graph" } else { "loading" };
    let text = if area.width < 80 {
        let mode = match catalog_tier {
            "partial_structural" => "partial",
            "structural+lazy" => "struct+lazy",
            other => other,
        };
        let state = if graph_ready { activity } else { "load" };
        format!(" {file_count}f {symbol_count}s {edge_count}e | {mode} | {state} | : ?")
    } else {
        format!(
            " {file_count}f {symbol_count}s {edge_count}e | {catalog_tier} | {graph}/{activity} | :cmd ?help"
        )
    };

    let status = Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Gray));
    frame.render_widget(status, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn command_and_help_hints_fit_at_eighty_columns() {
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    (341, 6014, 7002),
                    false,
                    "partial_structural",
                    "ready",
                )
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains(":cmd"));
        assert!(rendered.contains("?help"));
    }

    #[test]
    fn compact_status_fits_at_sixty_columns() {
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    (341, 6014, 7002),
                    false,
                    "partial_structural",
                    "ready",
                )
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("| : ?"));
    }
}
