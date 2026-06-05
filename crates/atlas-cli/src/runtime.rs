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
    ExtractionMode, Language, Store, Workspace, guard_against_precision_downgrade,
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
            .with_context(|| format!("Invalid project path: {project}"))?;

        let is_creator = matches!(mode, DbMode::InitOrCreate | DbMode::CreateOrOpenReadWrite);

        if is_creator {
            ws.ensure_atlas_dir()
                .context("Failed to create .atlas directory")?;
        } else {
            let db_exists = ws.db_path().is_file();
            if !db_exists {
                anyhow::bail!(
                    "Not an indexed Atlas project. Run `atlas index --project {project}` first."
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
    /// for `.atlas/`) when `project` is `"."`.  Creator modes fall back to the
    /// current directory when no ancestor index exists.  Otherwise delegates to
    /// standard `Workspace::open()`.
    ///
    /// This preserves the MCP server's ability to start from a non-project
    /// working directory while still finding the nearest Atlas project root.
    ///
    /// Respects `DbMode` identically to `open()` — creator modes will
    /// `ensure_atlas_dir()` + `init_schema()`, consumer modes will bail
    /// if the database does not exist.
    pub fn find_and_open(project: &str, mode: DbMode) -> anyhow::Result<Self> {
        let is_creator = matches!(mode, DbMode::InitOrCreate | DbMode::CreateOrOpenReadWrite);

        let ws = if project == "." {
            match Workspace::find() {
                Some(ws) => ws,
                None if is_creator => Workspace::open(Path::new(project))
                    .with_context(|| format!("Invalid project path: {project}"))?,
                None => {
                    anyhow::bail!("No .atlas directory found. Run `atlas index` first.");
                }
            }
        } else {
            Workspace::open(Path::new(project))
                .with_context(|| format!("Invalid project path: {project}"))?
        };

        if is_creator {
            ws.ensure_atlas_dir()
                .context("Failed to create .atlas directory")?;
        } else {
            let db_exists = ws.db_path().is_file();
            if !db_exists {
                anyhow::bail!("Not an indexed Atlas project. Run `atlas index` first.");
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

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{CapabilityMask, FileInfo, ParseStatus, source_file_id};
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

    #[test]
    fn guard_rejects_manifest_downgrade_of_structural_index() {
        let dir = TempDir::new().unwrap();
        let ctx = CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();
        seed_file_layer(&ctx.store, "src/lib.rs", "structural");

        let err = guard_against_precision_downgrade(
            &ctx.store,
            &ExtractionMode::Manifest,
            false,
            "atlas index",
        )
        .expect_err("manifest should not downgrade a structural index");
        assert!(
            err.to_string().contains("force_reindex=true"),
            "error should point to explicit override: {err:#}"
        );

        guard_against_precision_downgrade(
            &ctx.store,
            &ExtractionMode::Manifest,
            true,
            "atlas index",
        )
        .expect("force_reindex should allow explicit downgrade");
    }

    #[test]
    fn guard_rejects_structural_downgrade_of_full_index() {
        let dir = TempDir::new().unwrap();
        let ctx = CommandContext::open(dir.path().to_str().unwrap(), DbMode::InitOrCreate).unwrap();
        seed_file_layer(&ctx.store, "src/lib.rs", "dataflow");

        let err = guard_against_precision_downgrade(
            &ctx.store,
            &ExtractionMode::Structural,
            false,
            "atlas sync",
        )
        .expect_err("structural should not downgrade a full index");
        assert!(
            err.to_string().contains("analysis='full'"),
            "error should recommend preserving full precision: {err:#}"
        );
    }

    fn seed_file_layer(store: &Store, path: &str, layer: &str) {
        let file_id = source_file_id(Path::new(path)).expect("file id");
        let content_hash = "fresh-hash";
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language: Language::Rust,
                content_hash: content_hash.to_string(),
                status: ParseStatus::Success,
            })
            .expect("insert file");
        let layers: Vec<&str> = match layer {
            "dataflow" => vec!["manifest", "structural", "dataflow"],
            "structural" => vec!["manifest", "structural"],
            other => vec![other],
        };
        for layer in layers {
            store
                .upsert_file_extraction_state(
                    &file_id,
                    layer,
                    content_hash,
                    "complete",
                    CapabilityMask::from_layers(&[layer]),
                )
                .expect("insert extraction state");
        }
    }
}
