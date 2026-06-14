//! `project(action="open")` — synchronously open and activate a project.
//!
//! MCP project open is intentionally lightweight: it canonicalizes the project
//! path, opens the selected store, initializes schema, and activates
//! `ActiveProject`. It never indexes, scans the whole tree, or runs in a
//! background task. Scoped queries trigger focus-driven extraction on demand.

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::Store;

use super::{MAX_FILE_PATH_LENGTH, ToolRouter};

#[derive(serde::Serialize)]
struct OpenProjectResult {
    ok: bool,
    active_project: String,
    db_path: String,
    storage: String,
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

struct PreparedProject {
    project_root: PathBuf,
    store: Arc<Store>,
    result: OpenProjectResult,
}

impl ToolRouter {
    /// Handle `project(action="open")`.
    ///
    /// Parameters:
    ///   project_path (required): absolute path to the project directory.
    ///   storage (optional): "auto" (default) | "memory" | "persistent".
    pub(crate) fn handle_open_project(&mut self, args: &serde_json::Value) -> (String, bool) {
        match prepare_project(args) {
            Ok(prepared) => {
                let result = serde_json::to_string_pretty(&prepared.result)
                    .unwrap_or_else(|e| e.to_string());
                self.activate_project(prepared.project_root, prepared.store);
                (result, false)
            }
            Err(resp) => (
                serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            ),
        }
    }
}

#[allow(clippy::result_large_err)]
fn prepare_project(args: &serde_json::Value) -> Result<PreparedProject, OpenProjectResult> {
    let project_path = args["project_path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| open_error("Missing required parameter: project_path"))?;

    if project_path.len() > MAX_FILE_PATH_LENGTH {
        return Err(open_error(&format!(
            "project_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
        )));
    }

    if let Some(param) = unsupported_open_param(args) {
        return Err(open_error(&format!(
            "Unsupported project open parameter '{param}'. MCP project open only activates a project; use scoped queries for focus extraction and the CLI 'atlas index' command for explicit indexing."
        )));
    }

    let canonical = std::path::Path::new(project_path)
        .canonicalize()
        .map_err(|e| {
            open_error(&format!(
                "Project path not found or not accessible: {project_path} ({e})"
            ))
        })?;

    if !canonical.is_dir() {
        return Err(open_error(&format!(
            "Not a directory: {}",
            canonical.display()
        )));
    }

    let requested_storage = args["storage"].as_str().unwrap_or("auto");
    let persistent_index_mode = reusable_persistent_index_mode(&canonical);
    let storage = match requested_storage {
        "auto" if persistent_index_mode.is_some() => "persistent",
        "auto" => "memory",
        "memory" => "memory",
        "persistent" => "persistent",
        _ => {
            return Err(open_error(&format!(
                "Unknown storage mode '{requested_storage}'. Valid choices: 'auto', 'memory', 'persistent'."
            )));
        }
    };

    let store: Arc<Store> = match storage {
        "persistent" => {
            let atlas_dir = canonical.join(".atlas");
            std::fs::create_dir_all(&atlas_dir).map_err(|e| {
                open_error(&format!(
                    "Failed to create .atlas directory at {}: {}",
                    atlas_dir.display(),
                    e
                ))
            })?;
            let db_path = atlas_dir.join("atlas.db");
            Arc::new(Store::open_db(&db_path).map_err(|e| {
                open_error(&format!(
                    "Failed to open database {}: {e:#}",
                    db_path.display()
                ))
            })?)
        }
        "memory" => Arc::new(
            Store::open_in_memory()
                .map_err(|e| open_error(&format!("Failed to open in-memory store: {e:#}")))?,
        ),
        _ => unreachable!("storage validated above"),
    };

    store
        .init_schema()
        .map_err(|e| open_error(&format!("Schema init failed: {e:#}")))?;

    let note = match (requested_storage, storage, persistent_index_mode.as_deref()) {
        ("auto", "persistent", Some(mode)) => Some(format!(
            "Reused existing persistent .atlas/atlas.db (mode={mode})."
        )),
        ("auto", "memory", _) => Some(
            "No reusable .atlas/atlas.db was found; opened zero-footprint in-memory storage."
                .to_string(),
        ),
        ("memory", "memory", Some(_)) => Some(
            "Opened explicit in-memory storage; existing .atlas/atlas.db, if any, is ignored."
                .to_string(),
        ),
        _ => None,
    };

    Ok(PreparedProject {
        project_root: canonical.clone(),
        store: store.clone(),
        result: OpenProjectResult {
            ok: true,
            active_project: canonical.display().to_string(),
            db_path: store.db_path().display().to_string(),
            storage: storage.to_string(),
            error: None,
            note,
        },
    })
}

fn unsupported_open_param(args: &serde_json::Value) -> Option<&'static str> {
    [
        "index",
        "analysis",
        "include",
        "exclude",
        "background",
        "scan_files",
        "force_memory",
    ]
    .into_iter()
    .find(|key| args.get(*key).is_some())
}

fn reusable_persistent_index_mode(project_root: &std::path::Path) -> Option<String> {
    let db_path = project_root.join(".atlas").join("atlas.db");
    let store = Store::open_db_read_only(&db_path).ok()?;
    let index_mode = super::status::read_index_mode(&store).ok()?;
    if matches!(index_mode.as_str(), "none" | "unknown") {
        None
    } else {
        Some(index_mode)
    }
}

fn open_error(message: &str) -> OpenProjectResult {
    OpenProjectResult {
        ok: false,
        active_project: String::new(),
        db_path: String::new(),
        storage: String::new(),
        error: Some(message.to_string()),
        note: None,
    }
}
