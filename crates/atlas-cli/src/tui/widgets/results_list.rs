//! Scrollable search results list widget.

use atlas_engine::{SearchResult, SymbolKind};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
};

/// One row of the results list (pre-flattened for rendering).
pub struct ResultRow {
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: String,
    pub snippet: Option<String>,
}

impl From<SearchResult> for ResultRow {
    fn from(r: SearchResult) -> Self {
        let file_path = r.file_path.unwrap_or_else(|| String::from("?"));
        Self {
            name: r.symbol.name,
            kind: r.symbol.kind,
            file_path,
            snippet: r.snippet,
        }
    }
}

/// Render a scrollable symbol results list.
///
/// - `rows`: all matching symbols (pre-flattened).
/// - `selected`: index of the currently highlighted row (0-based).
/// - `scroll`: first visible row (adjusted so `selected` is in view).
///
/// The widget automatically computes the visible window so `selected`
/// is never outside the rendered area.
pub fn render(
    frame: &mut ratatui::Frame,
    area: Rect,
    rows: &[ResultRow],
    selected: usize,
    scroll: &mut usize,
) {
    let list_height = area.height.saturating_sub(2) as usize; // minus borders
    if list_height == 0 {
        return;
    }

    // Clamp scroll so the selected row is visible.
    if selected < *scroll {
        *scroll = selected;
    } else if selected >= *scroll + list_height {
        *scroll = selected.saturating_sub(list_height - 1);
    }

    let visible_rows = rows.iter().skip(*scroll).take(list_height);
    let items: Vec<ListItem> = visible_rows
        .enumerate()
        .map(|(i, row)| {
            let global_idx = *scroll + i;
            let is_selected = global_idx == selected;

            let name_style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let kind_str = format!("{:?}", row.kind);
            let kind_style = Style::default().fg(Color::Cyan);
            let path_style = Style::default().fg(Color::DarkGray);

            let mut spans = vec![
                Span::styled(
                    if is_selected { "> " } else { "  " },
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&row.name, name_style),
                Span::raw("  "),
                Span::styled(kind_str, kind_style),
            ];

            // Append file path on a separate line if there's room.
            if let Some(ref snippet) = row.snippet {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(snippet, Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(&row.file_path, path_style));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = format!(" Results ({}) ", rows.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));

    frame.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn only_selected_result_has_marker() {
        let rows = vec![
            ResultRow {
                name: "first".into(),
                kind: SymbolKind::Function,
                file_path: "src/first.rs".into(),
                snippet: None,
            },
            ResultRow {
                name: "second".into(),
                kind: SymbolKind::Function,
                file_path: "src/second.rs".into(),
                snippet: None,
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let mut scroll = 0;
        terminal
            .draw(|frame| render(frame, frame.area(), &rows, 1, &mut scroll))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("> first"));
        assert!(rendered.contains("> second"));
    }
}
