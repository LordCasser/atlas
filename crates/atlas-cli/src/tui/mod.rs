//! Terminal UI modules.
//!
//! - `app` — TUI application state machine and event loop
//! - `event` — crossterm event reader and high-level event types
//! - `progress` — terminal progress lifecycle (init, draw loop, summary)
//! - `fallback` — plain-text progress for non-TTY environments

pub mod app;
pub mod event;
pub mod fallback;
pub mod jobs;
pub mod progress;
pub mod search_session;
pub mod session;
pub mod widgets;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use atlas_engine::Store;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::ExecutableCommand;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub use fallback::TextFallback;
pub use progress::TuiProgress;

/// Map a character index to the byte position in `s`.
pub(crate) fn byte_index_at_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Launch the interactive TUI session.
///
/// Opens the Atlas database at `project_root/.atlas/atlas.db`, initialises a
/// ratatui terminal, and runs the main event loop.  The terminal is restored
/// on exit regardless of success or failure.
pub fn run_tui(project_root: PathBuf) -> anyhow::Result<()> {
    // ── Open database store ──────────────────────────────────────────────
    let db_path = project_root.join(".atlas").join("atlas.db");
    let mut store = open_tui_store(&db_path)?;

    if !has_basic_or_better_index(&store)? {
        drop(store);
        run_default_index_before_tui(&project_root)?;
        store = open_tui_store(&db_path)?;
    }

    // ── Set up ratatui terminal ──────────────────────────────────────────
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode")?;
    stdout
        .execute(EnterAlternateScreen)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    // ── Run app (with panic guard to restore terminal on unwind) ────────
    let mut app = app::App::new(store, project_root);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.run(&mut terminal)));

    // ── Restore terminal (always, even after panic) ─────────────────────
    disable_raw_mode().ok();
    terminal.backend_mut().execute(LeaveAlternateScreen).ok();

    match result {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}

fn open_tui_store(db_path: &Path) -> anyhow::Result<Arc<Store>> {
    // Ensure .atlas directory exists (first-run: rusqlite creates the file
    // but does not create parent directories).
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory at {}", parent.display()))?;
    }

    match open_initialized_store(db_path) {
        Ok(store) => Ok(store),
        Err(first_err) => {
            preserve_unusable_db(db_path).with_context(|| {
                format!(
                    "Failed to preserve unusable database at {} after open/init error: {first_err:#}",
                    db_path.display()
                )
            })?;
            open_initialized_store(db_path).with_context(|| {
                format!(
                    "Failed to recreate database at {} after preserving unusable DB: {first_err:#}",
                    db_path.display()
                )
            })
        }
    }
}

fn open_initialized_store(db_path: &Path) -> anyhow::Result<Arc<Store>> {
    let store = Arc::new(
        Store::open_db(db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?,
    );
    store
        .init_schema()
        .with_context(|| format!("Failed to initialize schema at {}", db_path.display()))?;
    Ok(store)
}

fn preserve_unusable_db(db_path: &Path) -> anyhow::Result<()> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for path in [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.display())),
        PathBuf::from(format!("{}-shm", db_path.display())),
    ] {
        if path.exists() {
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("atlas.db");
            let backup = path.with_file_name(format!("{file_name}.corrupt.{suffix}"));
            fs::rename(&path, &backup).with_context(|| {
                format!(
                    "Failed to move unusable database file {} to {}",
                    path.display(),
                    backup.display()
                )
            })?;
        }
    }
    Ok(())
}

fn has_basic_or_better_index(store: &Store) -> anyhow::Result<bool> {
    Ok(!matches!(
        store.read_index_mode()?.as_str(),
        "none" | "unknown"
    ))
}

fn run_default_index_before_tui(project_root: &Path) -> anyhow::Result<()> {
    let project = project_root.to_str().with_context(|| {
        format!(
            "Project root is not valid UTF-8; cannot run default index before TUI: {}",
            project_root.display()
        )
    })?;
    crate::commands::index::run(project, &[], &[], &[], "structural")
        .context("Failed to run default structural index before launching TUI")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tui_store_creates_fresh_database() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(".atlas").join("atlas.db");

        let store = open_tui_store(&db_path).unwrap();

        assert!(db_path.is_file());
        assert!(store.get_stats().is_ok());
    }

    #[test]
    fn open_tui_store_preserves_corrupt_database_and_recreates() {
        let dir = tempfile::TempDir::new().unwrap();
        let atlas_dir = dir.path().join(".atlas");
        fs::create_dir_all(&atlas_dir).unwrap();
        let db_path = atlas_dir.join("atlas.db");
        fs::write(&db_path, b"not sqlite").unwrap();

        let store = open_tui_store(&db_path).unwrap();

        assert!(db_path.is_file());
        assert!(store.get_stats().is_ok());
        let backups: Vec<_> = fs::read_dir(&atlas_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("atlas.db.corrupt.")
            })
            .collect();
        assert_eq!(backups.len(), 1);
    }
}
