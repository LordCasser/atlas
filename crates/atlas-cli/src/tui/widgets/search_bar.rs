//! Single-line search input widget.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Renders a search input bar at the given area.
///
/// `input` is the current query text; `cursor` is the byte index of the
/// insertion cursor (clamped to `input.len()`).  When the bar is focused,
/// a blinking block cursor is drawn at that position.
pub fn render(frame: &mut ratatui::Frame, area: Rect, input: &str, cursor: usize, focused: bool) {
    let (border_style, title) = if focused {
        (
            Style::default().fg(Color::Yellow),
            Line::from(" Search (type to filter, Enter to search, Esc to clear) "),
        )
    } else {
        (Style::default().fg(Color::DarkGray), Line::from(" Search "))
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build display text with a cursor indicator.
    let display: Line = if input.is_empty() {
        Line::from(Span::styled(
            "Search symbols...",
            Style::default().fg(Color::DarkGray),
        ))
    } else if focused {
        // Show cursor: split text at cursor position, insert a highlighted block.
        let cursor = cursor.min(input.len());
        let (before, at_char, after) = split_at_cursor(input, cursor);
        let highlight = if at_char.is_empty() {
            Span::styled(
                " ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                at_char,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        };

        Line::from(vec![Span::raw(before), highlight, Span::raw(after)])
    } else {
        Line::from(Span::raw(input))
    };

    let p = Paragraph::new(display);
    frame.render_widget(p, inner);
}

/// Handle text-editing key events for the search input.
///
/// Consumes the character and mutates `input`/`cursor` accordingly.
/// Returns `true` if the key was consumed, `false` if it should be
/// ignored and propagated to the parent handler.
pub fn handle_key(input: &mut String, cursor: &mut usize, c: char) -> bool {
    match c {
        '\u{08}' | '\u{7f}' => {
            // Backspace.
            if *cursor > 0 {
                let byte_pos = byte_index_at_char(input, *cursor - 1);
                input.remove(byte_pos);
                *cursor -= 1;
            }
            true
        }
        c if c.is_ascii_control() => {
            // Ignore other control characters (Enter, Esc, arrows, etc.
            // are handled by the parent via KeyCode).
            false
        }
        c => {
            let byte_pos = byte_index_at_char(input, *cursor);
            input.insert(byte_pos, c);
            *cursor += 1;
            true
        }
    }
}

/// Map a character index to the byte position in `s`.
fn byte_index_at_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Split `s` into (before_cursor, char_at_cursor, after_cursor) as owned strings.
/// `cursor` is a character index (0 = before first char).
fn split_at_cursor(s: &str, cursor: usize) -> (String, String, String) {
    let byte_pos = byte_index_at_char(s, cursor);
    let before = s[..byte_pos].to_string();
    let mut chars = s[byte_pos..].chars();
    let at = chars.next().unwrap_or(' ').to_string();
    let after = chars.as_str().to_string();
    (before, at, after)
}
