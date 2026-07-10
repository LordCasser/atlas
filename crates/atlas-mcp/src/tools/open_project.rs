//! `project(action="open")` — synchronously open and activate a project.
//!
//! MCP project open is intentionally lightweight: it canonicalizes the project
//! path, opens the project's persistent SQLite store, initializes schema, and
//! activates `ActiveProject`. It never indexes, scans the whole tree, or runs
//! in a background task. Scoped queries trigger focus-driven extraction on
//! demand.

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::Store;

use super::{MAX_FILE_PATH_LENGTH, ToolRouter, get_str};

use serde_json::{Value, json};

#[derive(serde::Serialize)]
struct OpenProjectResult {
    ok: bool,
    active_project: String,
    db_path: String,
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
    pub(crate) fn handle_open_project(&self, args: &serde_json::Value) -> (String, bool) {
        match prepare_project(args) {
            Ok(prepared) => {
                let starts_new_session = self
                    .project
                    .get()
                    .map(|active| active.root != prepared.project_root)
                    .unwrap_or(true);
                if starts_new_session && let Err(e) = prepared.store.reset_focus_session_state() {
                    let error =
                        open_error(&format!("Failed to reset stale focus session state: {e:#}"));
                    return (
                        serde_json::to_string_pretty(&error)
                            .unwrap_or_else(|serialize_error| serialize_error.to_string()),
                        true,
                    );
                }
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
            "Unsupported project open parameter '{param}'. MCP project open always activates project/.atlas/atlas.db without storage selection; use scoped queries for focus extraction and the CLI 'atlas index' command for explicit indexing."
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

    let atlas_dir = canonical.join(".atlas");
    std::fs::create_dir_all(&atlas_dir).map_err(|e| {
        open_error(&format!(
            "Failed to create .atlas directory at {}: {}",
            atlas_dir.display(),
            e
        ))
    })?;
    let db_path = atlas_dir.join("atlas.db");
    let store: Arc<Store> = Arc::new(Store::open_db(&db_path).map_err(|e| {
        open_error(&format!(
            "Failed to open database {}: {e:#}",
            db_path.display()
        ))
    })?);

    store.init_schema().map_err(|e| {
        let msg = format!("{e:#}");
        if msg.contains("schema version is") && msg.contains("expected v") {
            open_error(&format!(
                "{msg}\n\nFor MCP: remove the project .atlas/atlas.db (or the whole .atlas/ directory), then re-call project(action=\"open\") — the MCP open will create a fresh database automatically. No CLI indexing is needed."
            ))
        } else {
            open_error(&format!("Schema init failed: {msg}"))
        }
    })?;

    Ok(PreparedProject {
        project_root: canonical.clone(),
        store: store.clone(),
        result: OpenProjectResult {
            ok: true,
            active_project: canonical.display().to_string(),
            db_path: store.db_path().display().to_string(),
            error: None,
            note: Some(
                "Opened persistent project/.atlas/atlas.db; scoped queries populate it on demand."
                    .to_string(),
            ),
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
        "storage",
    ]
    .into_iter()
    .find(|key| args.get(*key).is_some())
}

fn open_error(message: &str) -> OpenProjectResult {
    OpenProjectResult {
        ok: false,
        active_project: String::new(),
        db_path: String::new(),
        error: Some(message.to_string()),
        note: None,
    }
}

impl ToolRouter {
    /// Handle `project` tool - dispatch by `action`.
    pub(crate) fn handle_project(&self, args: &Value) -> (String, bool) {
        let action = get_str(args, "action");
        match action {
            "open" => self.handle_open_project(args),
            "status" if self.project.is_active() => self.handle_status(),
            "status" => (
                serde_json::to_string_pretty(&json!({
                    "state": "not_open",
                    "active_project": null,
                    "open_required": true,
                    "message": "Open a project before using code-analysis tools."
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            "files" if self.project.is_active() => self.handle_files(args),
            "files" => (
                serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "error": "No active project. Call project(action=\"open\") first."
                }))
                .unwrap_or_else(|e| e.to_string()),
                true,
            ),
            "" => (
                "Missing required 'action' parameter. Must be one of: open, status, files"
                    .to_string(),
                true,
            ),
            other => (
                format!("Unknown action: '{other}'. Must be one of: open, status, files"),
                true,
            ),
        }
    }
}
