//! Unified CLI bootstrapping — `CommandContext` replaces per-command
//! `Workspace::open` / `Store::open_db` / `init_schema` boilerplate.
//!
//! ## DbMode
//! - `InitOrCreate`: init command — creates `.atlas/` and DB, initialises schema.
//! - `CreateOrOpenReadWrite`: index command — same creation + schema init.
//! - `ExistingReadOnly`: read-only commands — DB must already exist.
//! - `ExistingReadWrite`: read-write commands on existing DB (sync).
//!
//! ## Factory methods
//! - `CommandContext::open(project, mode)` — standard `Workspace::open` path.
//! - `CommandContext::find_and_open(project, mode)` — for MCP: uses
//!   `Workspace::find()` when `project == "."` (walks up from cwd).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use atlas_engine::{
    ExtractionError, ExtractionMode, FailureCategory, FileFacts, Language, LanguageFrontend,
    ParseWorkerPool, Store, Workspace,
};

/// Controls DB creation and schema-initialisation behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbMode {
    /// Create `.atlas/` directory and DB file if missing; init schema.
    InitOrCreate,
    /// Like `InitOrCreate` — also creates and initialises.  Used by index.
    CreateOrOpenReadWrite,
    /// DB must already exist; no creation side-effects.
    ExistingReadOnly,
    /// DB must already exist; caller intends writes.
    ExistingReadWrite,
}

/// Unified CLI bootstrapping context.
///
/// Holds an open workspace, the canonical project root, and an `Arc<Store>`.
/// Commands should obtain a context via `open()` or `find_and_open()` instead of
/// manually wiring `Workspace::open` + `Store::open_db` + `init_schema`.
pub struct CommandContext {
    pub workspace: Workspace,
    pub root: PathBuf,
    pub store: Arc<Store>,
}

impl CommandContext {
    // ── Standard bootstrap ─────────────────────────────────────────────────

    /// Open a workspace from `project` with the given `DbMode`.
    ///
    /// Creator modes (`InitOrCreate`, `CreateOrOpenReadWrite`) will
    /// `ensure_atlas_dir()` and `init_schema()`.
    ///
    /// Consumer modes (`ExistingReadOnly`, `ExistingReadWrite`) bail if no
    /// database exists.
    pub fn open(project: &str, mode: DbMode) -> anyhow::Result<Self> {
        let ws = Workspace::open(Path::new(project))
            .with_context(|| format!("Invalid project path: {}", project))?;

        let is_creator = matches!(mode, DbMode::InitOrCreate | DbMode::CreateOrOpenReadWrite);

        if is_creator {
            ws.ensure_atlas_dir()
                .context("Failed to create .atlas directory")?;
        } else {
            let db_exists = ws.db_path().is_file();
            if !db_exists {
                anyhow::bail!(
                    "Not an initialized Atlas project. Run `atlas init {}` first.",
                    project
                );
            }
        }

        let store =
            Arc::new(Store::open_db(ws.db_path()).context("Failed to open Atlas database")?);

        if is_creator {
            store
                .init_schema()
                .context("Failed to initialize database schema")?;
        }

        Ok(Self {
            root: ws.root().to_path_buf(),
            workspace: ws,
            store,
        })
    }

    // ── MCP-style bootstrap with ancestor walk ─────────────────────────────

    /// Like `open()` but uses `Workspace::find()` (walks up from cwd looking
    /// for `.atlas/`) when `project` is `"."`.  Otherwise delegates to standard
    /// `Workspace::open()`.
    ///
    /// This preserves the MCP server's ability to start from a non-project
    /// working directory while still finding the nearest Atlas project root.
    ///
    /// Respects `DbMode` identically to `open()` — creator modes will
    /// `ensure_atlas_dir()` + `init_schema()`, consumer modes will bail
    /// if the database does not exist.
    pub fn find_and_open(project: &str, mode: DbMode) -> anyhow::Result<Self> {
        let ws = if project == "." {
            Workspace::find().context("No .atlas directory found. Run `atlas init` first.")?
        } else {
            Workspace::open(Path::new(project))
                .with_context(|| format!("Invalid project path: {}", project))?
        };

        let is_creator = matches!(mode, DbMode::InitOrCreate | DbMode::CreateOrOpenReadWrite);

        if is_creator {
            ws.ensure_atlas_dir()
                .context("Failed to create .atlas directory")?;
        } else {
            let db_exists = ws.db_path().is_file();
            if !db_exists {
                anyhow::bail!("Not an initialized Atlas project. Run `atlas init` first.");
            }
        }

        let store =
            Arc::new(Store::open_db(ws.db_path()).context("Failed to open Atlas database")?);

        if is_creator {
            store
                .init_schema()
                .context("Failed to initialize database schema")?;
        }

        Ok(Self {
            root: ws.root().to_path_buf(),
            workspace: ws,
            store,
        })
    }
}

// ── Shared extraction ──────────────────────────────────────────────────────

/// Read a source file, hash it, and extract facts using the given language frontend.
///
/// Shared by `commands::index` and `tui::auto_index` to avoid duplicating the
/// file-read → hash → `pool.extract_one` pipeline.
pub fn extract_one(
    pool: &ParseWorkerPool,
    path: &Path,
    root: &Path,
    _lang: Language,
    frontend: &LanguageFrontend,
    mode: ExtractionMode,
) -> Result<FileFacts, ExtractionError> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let relative = path.strip_prefix(root).unwrap_or(path);
            let rel_str = relative.to_string_lossy().to_string();
            let msg = format!("Failed to read {}: {}", path.display(), e);
            pool.push_failure(&rel_str, FailureCategory::IoError, msg.clone());
            return Err(ExtractionError {
                file_path: rel_str,
                category: FailureCategory::IoError,
                message: msg,
            });
        }
    };
    let content_hash = blake3::hash(source.as_bytes()).to_hex();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let rel_str = relative.to_string_lossy().to_string();
    let file_id = atlas_engine::source_file_id(relative).map_err(|_| ExtractionError {
        file_path: rel_str.clone(),
        category: FailureCategory::IoError,
        message: format!("invalid source path: {}", relative.display()),
    })?;
    pool.extract_one(frontend, file_id, relative, &source, &content_hash, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A valid temp directory with no `.atlas/` should succeed with creator modes.
    #[test]
    fn open_init_or_create_creates_atlas() {
        let dir = TempDir::new().unwrap();
        let atlas_dir = dir.path().join(".atlas");
        assert!(!atlas_dir.exists(), "precondition: no .atlas/ yet");

        let ctx = CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();

        assert!(atlas_dir.is_dir(), ".atlas/ should exist");
        assert!(
            atlas_dir.join("atlas.db").is_file(),
            "atlas.db should exist"
        );
        assert!(ctx.store.get_stats().is_ok(), "store should be usable");
    }

    /// Consumer modes must reject directories with no `.atlas/`.
    #[test]
    fn open_existing_read_only_rejects_missing_db() {
        let dir = TempDir::new().unwrap();
        let result = CommandContext::open(dir.path().to_str().unwrap(), DbMode::ExistingReadOnly);
        assert!(result.is_err(), "consumer mode should reject missing DB");
    }

    /// Consumer modes should accept an existing DB.
    #[test]
    fn open_existing_read_only_accepts_db() {
        let dir = TempDir::new().unwrap();
        // First create the DB via creator mode
        let _ctx =
            CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();
        // Now re-open as read-only
        let ctx =
            CommandContext::open(dir.path().to_str().unwrap(), DbMode::ExistingReadOnly).unwrap();

        assert!(ctx.store.get_stats().is_ok(), "store should be usable");
    }

    /// `find_and_open` with "." should walk up and fail when no .atlas/ ancestor exists.
    #[test]
    fn find_and_open_rejects_no_ancestor() {
        let dir = TempDir::new().unwrap();
        let result =
            CommandContext::find_and_open(dir.path().to_str().unwrap(), DbMode::ExistingReadOnly);
        assert!(result.is_err(), "find_and_open should reject missing DB");
    }

    /// Using a non-"." project should still work with find_and_open.
    #[test]
    fn find_and_open_with_explicit_project() {
        let dir = TempDir::new().unwrap();
        // First create via creator mode
        let _ctx =
            CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();
        // Open with explicit path via find_and_open
        let ctx =
            CommandContext::find_and_open(dir.path().to_str().unwrap(), DbMode::ExistingReadOnly)
                .unwrap();
        assert!(ctx.store.get_stats().is_ok());
    }

    /// `InitOrCreate` mode should also call init_schema (idempotent).
    #[test]
    fn open_init_or_create_inits_schema() {
        let dir = TempDir::new().unwrap();
        let ctx = CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();
        // After init, schema version should be set
        let stats = ctx.store.get_stats().unwrap();
        // Just verify the store is in a healthy state
        assert!(stats.total_symbols == 0);
    }
}
