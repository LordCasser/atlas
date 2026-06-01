//! Terminal UI modules.
//!
//! - `app` — TUI application state machine and event loop
//! - `auto_index` — manifest-mode background indexing on empty DB
//! - `event` — crossterm event reader and high-level event types
//! - `progress` — terminal progress lifecycle (init, draw loop, summary)
//! - `fallback` — plain-text progress for non-TTY environments

pub mod app;
pub mod auto_index;
pub mod event;
pub mod fallback;
pub mod progress;
pub mod session;
pub mod widgets;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use atlas_engine::Store;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::ExecutableCommand;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen,
                                   LeaveAlternateScreen};

pub use fallback::TextFallback;
pub use progress::TuiProgress;

/// Launch the interactive TUI session.
///
/// Opens the Atlas database at `project_root/.atlas/atlas.db`, initialises a
/// ratatui terminal, and runs the main event loop.  The terminal is restored
/// on exit regardless of success or failure.
pub fn run_tui(project_root: PathBuf) -> anyhow::Result<()> {
    // ── Open database store ──────────────────────────────────────────────
    let db_path = project_root.join(".atlas").join("atlas.db");
    let store = Arc::new(
        Store::open_db(&db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?,
    );

    // ── Set up ratatui terminal ──────────────────────────────────────────
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode")?;
    stdout
        .execute(EnterAlternateScreen)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    // ── Run app ─────────────────────────────────────────────────────────
    let mut app = app::App::new(store, project_root);
    let result = app.run(&mut terminal);

    // ── Restore terminal (always) ───────────────────────────────────────
    disable_raw_mode().ok();
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .ok();

    result
}
