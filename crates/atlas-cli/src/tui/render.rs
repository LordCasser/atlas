// TUI renderer — draws the multi-panel phase list using ratatui.
//
// Layout:
// ```
// ┌─ Atlas Index ──────────────────────────────────────────
// │
// │  ◆ Discovery — 161 files                      0.1s
// │  ◆ Hash check — 150 dirty / 11 reused         0.2s
// │  ◌ Parsing code   8,234 files . 1,240/s
// │  . Storing data                          (pending)
// │  . Resolving refs                        (pending)
// │
// │  Total: 8,234/18,432 | 1,240/s | elapsed 5.2s
// └────────────────────────────────────────────────────────
// ```

use std::sync::{Arc, Mutex};

use atlas_engine::progress::{PhaseState, ProgressSnapshot};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

// Colour palette
const DONE_COLOR: Color = Color::Rgb(100, 200, 100);
const ACTIVE_COLOR: Color = Color::Rgb(251, 191, 36);
const BAR_FILLED: Color = Color::Rgb(100, 200, 100);
const BAR_EMPTY: Color = Color::Rgb(60, 60, 60);
const PENDING_COLOR: Color = Color::Rgb(120, 120, 120);
const DIM: Color = Color::Rgb(120, 120, 120);

/// Render one frame of the TUI. Called from the main thread's draw loop.
pub fn render(
    frame: &mut Frame,
    state: Arc<Mutex<atlas_engine::progress::ProgressState>>,
    _tick: u64,
) {
    // Use try_lock to never block if the worker holds the mutex.
    // On contention, the frame is simply skipped — the next frame
    // (200ms later) will pick up the latest state.
    let guard = match state.try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let snap = guard.read_snapshot();

    let area = frame.area();
    let block = Block::default()
        .title(" Atlas Index ")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Phase rows
    let mut constraints = Vec::new();
    for _entry in &snap.phases {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // footer
    let rows = Layout::vertical(constraints).split(inner);

    for (i, entry) in snap.phases.iter().enumerate() {
        if i >= rows.len().saturating_sub(1) {
            break;
        }
        render_phase_row(frame, entry, &snap, rows[i]);
    }

    // Footer
    if let Some(footer_area) = rows.last() {
        render_footer(frame, &snap, *footer_area);
    }
}

fn render_phase_row(
    frame: &mut Frame,
    entry: &atlas_engine::progress::PhaseEntry,
    snap: &ProgressSnapshot,
    area: Rect,
) {
    let name = entry.phase.display_name();
    let mut spans: Vec<Span> = Vec::new();

    match &entry.state {
        PhaseState::Completed { elapsed, note, .. } => {
            spans.push(Span::styled(
                format!(" ◆ {}", name),
                Style::new().fg(DONE_COLOR),
            ));
            if let Some(n) = note {
                spans.push(Span::styled(
                    format!(" — {}", n),
                    Style::new().fg(DIM),
                ));
            }
            let s = elapsed.as_secs_f64();
            if s >= 0.1 {
                spans.push(Span::styled(
                    format!("  {:.1}s", s),
                    Style::new().fg(DIM),
                ));
            }
        }

        PhaseState::Running { .. } => {
            if snap.phase2_active && snap.total.is_some() {
                let pct = snap.percent().unwrap_or(0.0);
                let current = snap.current;
                let total = snap.total.unwrap_or(0);
                spans.push(Span::styled(
                    format!(" ◌ {}  {:.0}%  {}/{}", name, pct, current, total),
                    Style::new().fg(ACTIVE_COLOR),
                ));
            } else {
                spans.push(Span::styled(
                    format!(" ◌ {}", name),
                    Style::new().fg(ACTIVE_COLOR),
                ));
                if snap.current > 0 {
                    spans.push(Span::styled(
                        format!("  {} matched", snap.current),
                        Style::new().fg(DIM),
                    ));
                }
            }
            if let Some(rate) = snap.rate {
                spans.push(Span::styled(
                    format!("  {:.0}/s", rate),
                    Style::new().fg(DIM),
                ));
            }
            if let Some(ref msg) = snap.message {
                spans.push(Span::styled(
                    format!("  {}", msg),
                    Style::new().fg(DIM),
                ));
            }
        }

        PhaseState::Pending => {
            spans.push(Span::styled(
                format!(" · {}  (pending)", name),
                Style::new().fg(PENDING_COLOR),
            ));
        }
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_footer(frame: &mut Frame, snap: &ProgressSnapshot, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    if let Some(total) = snap.total {
        spans.push(Span::styled(
            format!("{}/{}", snap.current, total),
            Style::new(),
        ));
    } else if snap.current > 0 {
        spans.push(Span::styled(
            format!("{} items", snap.current),
            Style::new(),
        ));
    }

    if let Some(rate) = snap.rate {
        spans.push(Span::styled(" | ", Style::new().fg(DIM)));
        spans.push(Span::styled(format!("{:.0}/s", rate), Style::new().fg(DIM)));
    }

    spans.push(Span::styled(" | elapsed ", Style::new().fg(DIM)));
    spans.push(Span::styled(
        format!("{:.1}s", snap.elapsed.as_secs_f64()),
        Style::new().fg(DIM),
    ));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}
