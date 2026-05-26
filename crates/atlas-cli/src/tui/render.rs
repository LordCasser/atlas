// TUI renderer — draws the compact progress display using ratatui.
//
// Layout (Plan B — compact with Gauge progress bar):
// ```
// ┌─ Atlas Index ──────────────────────────────────────────
// │
// │  ◆ Scanning files · Computing hashes · Cleaning stale data
// │  Parsing code  ████████░░░░░░░░░░░░  45%
// │  · Storing data · Resolving refs · Building edges · Finalizing
// │  Total: 8,234/18,432 | 1,240/s | elapsed 5.2s
// └────────────────────────────────────────────────────────
// ```
//
// Completed phases are merged into one row, pending phases into another.
// The currently running phase uses a ratatui `Gauge` widget when the
// total is known; otherwise it falls back to a text line with a count.

use std::sync::{Arc, Mutex};

use atlas_engine::progress::ProgressSnapshot;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};

// Colour palette
const DONE_COLOR: Color = Color::Rgb(100, 200, 100);
const ACTIVE_COLOR: Color = Color::Rgb(251, 191, 36);
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

    // Group phases by state
    let completed: Vec<&atlas_engine::progress::PhaseEntry> =
        snap.phases.iter().filter(|e| e.state.is_completed()).collect();
    let running: Option<&atlas_engine::progress::PhaseEntry> =
        snap.phases.iter().find(|e| e.state.is_running());
    let pending: Vec<&atlas_engine::progress::PhaseEntry> =
        snap.phases.iter().filter(|e| e.state.is_pending()).collect();

    // 4 rows: completed | gauge | pending | footer
    let rows = Layout::vertical([
        Constraint::Length(1), // completed
        Constraint::Length(1), // gauge / running
        Constraint::Length(1), // pending
        Constraint::Length(1), // footer
    ])
    .split(inner);

    render_completed_row(frame, &completed, rows[0]);
    render_gauge_row(frame, running, &snap, rows[1]);
    render_pending_row(frame, &pending, rows[2]);
    render_footer(frame, &snap, rows[3]);
}

// ── Completed row ──────────────────────────────────────────────────────

fn render_completed_row(
    frame: &mut Frame,
    completed: &[&atlas_engine::progress::PhaseEntry],
    area: Rect,
) {
    if completed.is_empty() {
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" ◆ ", Style::new().fg(DONE_COLOR)));

    for (i, entry) in completed.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(DIM)));
        }
        spans.push(Span::styled(
            entry.phase.display_name(),
            Style::new().fg(DONE_COLOR),
        ));
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

// ── Gauge / running row ────────────────────────────────────────────────

fn render_gauge_row(
    frame: &mut Frame,
    running: Option<&atlas_engine::progress::PhaseEntry>,
    snap: &ProgressSnapshot,
    area: Rect,
) {
    let Some(entry) = running else {
        return; // no running phase, nothing to render
    };
    let name = entry.phase.display_name();

    // When the current phase has a known total, render a Gauge bar.
    // Otherwise (Discovery, LanguageInit, Finalizing) fall back to text.
    if let Some(pct) = snap.percent() {
        let label = format!("{}  {:.0}%", name, pct);
        let gauge = Gauge::default()
            .gauge_style(Style::new().fg(ACTIVE_COLOR))
            .percent(pct as u16)
            .label(Span::styled(label, Style::new()));
        frame.render_widget(gauge, area);
    } else {
        // No total available — render a text line with a spinner and count
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!(" ◌ {}", name),
            Style::new().fg(ACTIVE_COLOR),
        ));
        if snap.current > 0 {
            spans.push(Span::styled(
                format!("  {} items", snap.current),
                Style::new().fg(DIM),
            ));
        }
        let line = Line::from(spans);
        frame.render_widget(Paragraph::new(line), area);
    }
}

// ── Pending row ────────────────────────────────────────────────────────

fn render_pending_row(
    frame: &mut Frame,
    pending: &[&atlas_engine::progress::PhaseEntry],
    area: Rect,
) {
    if pending.is_empty() {
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" · ", Style::new().fg(PENDING_COLOR)));

    for (i, entry) in pending.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(PENDING_COLOR)));
        }
        spans.push(Span::styled(
            entry.phase.display_name(),
            Style::new().fg(PENDING_COLOR),
        ));
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

// ── Footer ─────────────────────────────────────────────────────────────

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
