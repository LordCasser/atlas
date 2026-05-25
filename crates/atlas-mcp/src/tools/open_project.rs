//! `open_project` MCP tool — open, index, and activate an arbitrary project.
//!
//! Unlike the MCP server's initial project (which is always persistent),
//! `open_project` defaults to `storage: "memory".to_string()` for zero-footprint
//! temporary sessions.  Switching back to the original project requires
//! another `open_project` call with the original path and
//! `storage: "persistent".to_string()`.

use std::sync::Arc;

use atlas_engine::{
    ExtractionMode, Store,
};

use super::ToolRouter;

/// Result of an open_project invocation.
#[derive(serde::Serialize)]
struct OpenProjectResult {
    ok: bool,
    active_project: String,
    db_path: String,
    storage: String,
    /// Present only when `index: true` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<super::index::IndexResult>,
    error: Option<String>,
}

impl ToolRouter {
    /// Handle `open_project` tool call.
    ///
    /// Parameters:
    ///   project_path (required): absolute path to the project directory.
    ///   storage (optional): "memory" (default) | "persistent".
    ///   index (optional): whether to run the index pipeline (default: true).
    ///   analysis (optional): "structural" (default) | "full".
    ///   exclude (optional): list of glob patterns to skip.
    ///
    /// This handler always switches the active project on success.
    /// To re-index the current project without switching, use the `index` tool.
    pub(crate) fn handle_open_project(
        &mut self,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let start = std::time::Instant::now();

        // ── project_path (required) ──────────────────────────────────────
        let project_path = match args["project_path"].as_str() {
            Some(p) if !p.is_empty() => p,
            _ => {
                let resp = OpenProjectResult {
                    ok: false,
                    active_project: String::new(),
                    db_path: String::new(),
                    storage: String::new(),
                    index: None,
                    error: Some("Missing required parameter: project_path".into()),
                };
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // ── Canonicalize + validate ──────────────────────────────────────
        let canonical = match std::path::Path::new(project_path).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                let resp = OpenProjectResult {
                    ok: false,
                    active_project: String::new(),
                    db_path: String::new(),
                    storage: String::new(),
                    index: None,
                    error: Some(format!(
                        "Project path not found or not accessible: {} ({})",
                        project_path, e
                    )),
                };
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        if !canonical.is_dir() {
            let resp = OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: String::new(),
                index: None,
                error: Some(format!("Not a directory: {}", canonical.display())),
            };
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        // ── Storage mode ─────────────────────────────────────────────────
        let storage = args["storage"].as_str().unwrap_or("memory").to_string();
        // Reject unknown storage values to prevent silent misbehavior.
        if storage != "memory" && storage != "persistent" {
            let resp = OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: storage.clone(),
                index: None,
                error: Some(format!(
                    "Unknown storage mode '{}'. Valid choices: 'memory', 'persistent'.",
                    storage
                )),
            };
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        let do_index = args["index"].as_bool().unwrap_or(true);
        let analysis = args["analysis"].as_str().unwrap_or("structural");
        let mode = match analysis {
            "full" => ExtractionMode::Full,
            _ => ExtractionMode::Structural,
        };
        let exclude_patterns: Vec<String> = args["exclude"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // ── Open/create store ────────────────────────────────────────────
        let store: Arc<Store> = match storage.as_str() {
            "persistent" => {
                let atlas_dir = canonical.join(".atlas");
                if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
                    let resp = OpenProjectResult {
                        ok: false,
                        active_project: String::new(),
                        db_path: String::new(),
                        storage: "persistent".to_string(),
                        index: None,
                        error: Some(format!(
                            "Failed to create .atlas directory at {}: {}",
                            atlas_dir.display(), e
                        )),
                    };
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
                let db_path = atlas_dir.join("atlas.db");
                match Store::open_db(&db_path) {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        let resp = OpenProjectResult {
                            ok: false,
                            active_project: String::new(),
                            db_path: db_path.display().to_string(),
                            storage: "persistent".to_string(),
                            index: None,
                            error: Some(format!("Failed to open database: {:#}", e)),
                        };
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                }
            }
            "memory" => {
                // In-memory store
                match Store::open_in_memory() {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        let resp = OpenProjectResult {
                            ok: false,
                            active_project: String::new(),
                            db_path: String::new(),
                            storage: "memory".to_string(),
                            index: None,
                            error: Some(format!("Failed to open in-memory store: {:#}", e)),
                        };
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                }
            }
            _ => unreachable!("storage validated above"),
        };

        // ── Init schema ──────────────────────────────────────────────────
        if let Err(e) = store.init_schema() {
            let resp = OpenProjectResult {
                ok: false,
                active_project: canonical.display().to_string(),
                db_path: store.db_path().display().to_string(),
                storage: storage.clone(),
                index: None,
                error: Some(format!("Schema init failed: {:#}", e)),
            };
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        // ── Index (optional) ─────────────────────────────────────────────
        let mut index_result: Option<super::index::IndexResult> = None;
        if do_index {
            // For persistent mode, try to acquire FileLock to avoid
            // concurrent writes with CLI. Memory mode needs no lock.
            let _lock_guard = if storage == "persistent" {
                match atlas_engine::FileLock::acquire(&store) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        let resp = OpenProjectResult {
                            ok: false,
                            active_project: canonical.display().to_string(),
                            db_path: store.db_path().display().to_string(),
                            storage: storage.clone(),
                            index: None,
                            error: Some(format!(
                                "Cannot acquire exclusive lock for indexing: {:#}",
                                e
                            )),
                        };
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                }
            } else {
                None
            };

            match super::index::run_index(&store, &canonical, mode, &exclude_patterns) {
                Ok(stats) => {
                    index_result = Some(super::index::IndexResult {
                        ok: true,
                        files_discovered: stats.discovered,
                        files_indexed: stats.indexed,
                        files_failed: stats.failed,
                        symbols_found: stats.symbols,
                        references_resolved: stats.resolved,
                        errors: Vec::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }
                Err(e) => {
                    let resp = OpenProjectResult {
                        ok: false,
                        active_project: canonical.display().to_string(),
                        db_path: store.db_path().display().to_string(),
                        storage: storage.clone(),
                        index: Some(super::index::IndexResult {
                            ok: false,
                            files_discovered: 0,
                            files_indexed: 0,
                            files_failed: 0,
                            symbols_found: 0,
                            references_resolved: 0,
                            errors: vec![format!("{:#}", e)],
                            duration_ms: start.elapsed().as_millis() as u64,
                        }),
                        error: Some("Index failed".into()),
                    };
                    return (
                        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                        true,
                    );
                }
            }
            // FileLock guard is dropped here (if it exists), releasing the lock.
            drop(_lock_guard);
        }

        // ── Activate ─────────────────────────────────────────────────────
        let db_path = store.db_path().display().to_string();
        let active_project = canonical.display().to_string();
        self.activate_project(canonical, store);

        // ── Response ─────────────────────────────────────────────────────
        let duration_ms = start.elapsed().as_millis() as u64;
        if let Some(ref mut ir) = index_result {
            ir.duration_ms = duration_ms;
        }

        let resp = OpenProjectResult {
            ok: true,
            active_project,
            db_path,
            storage,
            index: index_result,
            error: None,
        };

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
