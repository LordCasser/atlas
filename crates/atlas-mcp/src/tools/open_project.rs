//! `open_project` MCP tool — open and activate a project.
//!
//! Unlike the MCP server's initial project (which is always persistent),
//! `open_project` defaults to automatic storage selection: read the candidate
//! persistent project status and reuse `.atlas/atlas.db` when it reports a
//! reusable index, otherwise use `storage: "memory"` for zero-footprint,
//! instant-start temporary sessions. Explicit `storage: "memory"` is refused
//! when a reusable persistent index exists unless `force_memory=true` is set.
//! Indexing is handled exclusively by the `index` tool after project activation.
//!
//! Long-running project opens can use `background: true`. The tool then returns
//! a `task_id` immediately; `task_status` or `wait_for_task` activates the
//! prepared project once the background task completes.

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::Store;
use serde_json::json;

use super::{MAX_FILE_PATH_LENGTH, PendingProjectActivation, ToolRouter};

/// Result of an open_project invocation.
#[derive(serde::Serialize)]
struct OpenProjectResult {
    ok: bool,
    active_project: String,
    db_path: String,
    storage: String,
    /// Approximate file count, present only when `scan_files=true`.
    file_count: Option<usize>,
    /// Suggestion for large projects.
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
    error: Option<String>,
}

struct PreparedProject {
    project_root: PathBuf,
    store: Arc<Store>,
    result: OpenProjectResult,
}

/// Threshold above which we suggest background indexing after activation.
const LARGE_PROJECT_FILE_COUNT: usize = 5_000;

impl ToolRouter {
    /// Handle `open_project` tool call.
    ///
    /// Parameters:
    ///   project_path (required): absolute path to the project directory.
    ///   storage (optional): "auto" (default) | "memory" | "persistent".
    ///   force_memory (optional): allow memory even when a persistent index exists.
    ///   scan_files (optional): run pre-index discovery for file_count (default: false).
    ///   background (optional): prepare in a background task and activate on wait/status.
    pub(crate) fn handle_open_project(&mut self, args: &serde_json::Value) -> (String, bool) {
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if background {
            return self.handle_open_project_background(args);
        }

        match prepare_project(args) {
            Ok(prepared) => {
                let result = serde_json::to_string_pretty(&prepared.result)
                    .unwrap_or_else(|e| e.to_string());
                self.activate_project(prepared.project_root, prepared.store);
                (result, false)
            }
            Err(resp) => (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            ),
        }
    }

    fn handle_open_project_background(&mut self, args: &serde_json::Value) -> (String, bool) {
        let auto_background = args
            .get("_auto_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if args["project_path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .is_none()
        {
            let resp = OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: String::new(),
                file_count: None,
                suggestion: None,
                error: Some("Missing required parameter: project_path".into()),
            };
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        let task_id = self.active_mut().job_runtime.task_manager.create_task("project", "project");
        let tid = task_id.clone();
        let task_manager = self.active_mut().job_runtime.task_manager.clone();
        let pending = self.active_mut().job_runtime.pending_project_activations.clone();
        let owned_args = args.clone();

        std::thread::spawn(move || {
            task_manager.update_progress(&tid, 1.0, "Preparing project...");
            match prepare_project(&owned_args) {
                Ok(prepared) => {
                    task_manager.update_progress(
                        &tid,
                        95.0,
                        "Project prepared; activation pending...",
                    );
                    let project_root = prepared.project_root.clone();
                    let result = serde_json::to_value(&prepared.result)
                        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
                    match pending.lock() {
                        Ok(mut guard) => {
                            guard.insert(
                                tid.clone(),
                                PendingProjectActivation {
                                    project_root,
                                    store: prepared.store,
                                },
                            );
                            task_manager.complete_task(
                                &tid,
                                json!({
                                    "open_project": result,
                                    "activation": "pending",
                                    "next_action": "Call task_status or wait_for_task with this task_id; the completed task will activate the prepared project."
                                }),
                            );
                        }
                        Err(_poison) => {
                            tracing::error!(
                                "Mutex poisoned on pending project activations; task '{}' activation failed",
                                tid
                            );
                            task_manager.fail_task(
                                &tid,
                                "Internal server state corrupted (mutex poisoned)",
                            );
                        }
                    }
                }
                Err(resp) => {
                    let msg = resp
                        .error
                        .clone()
                        .unwrap_or_else(|| "project open failed".to_string());
                    task_manager.fail_task(&tid, &msg);
                }
            }
        });

        (
            serde_json::to_string_pretty(&json!({
                "background": true,
                "task_id": task_id,
                "tool_name": "project",
                "method": "project",
                "status": "running",
                "progress": 0.0,
                "progress_message": "queued",
                "auto_background": auto_background,
                "note": "project open is running in background. Poll task_status for progress percentages; completion activates when task_status or wait_for_task observes the completed task."
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}

#[allow(clippy::result_large_err)]
fn prepare_project(args: &serde_json::Value) -> Result<PreparedProject, OpenProjectResult> {
    let project_path = args["project_path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            suggestion: None,
            error: Some("Missing required parameter: project_path".into()),
        })?;

    if project_path.len() > MAX_FILE_PATH_LENGTH {
        return Err(OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            suggestion: None,
            error: Some(format!(
                "project_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
            )),
        });
    }

    if let Some(param) = unsupported_index_param(args) {
        return Err(OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            suggestion: Some(
                "Use open_project only to activate a project, then call index on the active project."
                    .into(),
            ),
            error: Some(format!(
                "Unsupported open_project parameter '{param}'. Project indexing is handled only by the index tool."
            )),
        });
    }

    let canonical = std::path::Path::new(project_path)
        .canonicalize()
        .map_err(|e| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            suggestion: None,
            error: Some(format!(
                "Project path not found or not accessible: {project_path} ({e})"
            )),
        })?;

    if !canonical.is_dir() {
        return Err(OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            suggestion: None,
            error: Some(format!("Not a directory: {}", canonical.display())),
        });
    }

    let requested_storage = args["storage"].as_str().unwrap_or("auto");
    let force_memory = args["force_memory"].as_bool().unwrap_or(false);
    let persistent_index_mode = reusable_persistent_index_mode(&canonical);
    let storage = match requested_storage {
        "auto" if persistent_index_mode.is_some() => "persistent".to_string(),
        "auto" => "memory".to_string(),
        "memory" if persistent_index_mode.is_some() && !force_memory => {
            let index_mode = persistent_index_mode
                .as_deref()
                .unwrap_or("reusable")
                .to_string();
            return Err(OpenProjectResult {
                ok: false,
                active_project: canonical.display().to_string(),
                db_path: canonical.join(".atlas").join("atlas.db").display().to_string(),
                storage: "memory".to_string(),
                file_count: None,
                suggestion: Some(format!(
                    "Existing persistent index detected (mode={index_mode}). Use storage=\"auto\" or storage=\"persistent\" to reuse it. Pass force_memory=true only when you intentionally want an empty temporary in-memory index."
                )),
                error: Some(
                    "Refusing storage=\"memory\" because it would ignore an existing persistent Atlas index."
                        .into(),
                ),
            });
        }
        "memory" | "persistent" => requested_storage.to_string(),
        _ => {
            return Err(OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: requested_storage.to_string(),
                file_count: None,
                suggestion: None,
                error: Some(format!(
                    "Unknown storage mode '{requested_storage}'. Valid choices: 'auto', 'memory', 'persistent'."
                )),
            });
        }
    };
    let scan_files = args["scan_files"].as_bool().unwrap_or(false);

    let store: Arc<Store> = match storage.as_str() {
        "persistent" => {
            let atlas_dir = canonical.join(".atlas");
            std::fs::create_dir_all(&atlas_dir).map_err(|e| OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: "persistent".to_string(),
                file_count: None,
                suggestion: None,
                error: Some(format!(
                    "Failed to create .atlas directory at {}: {}",
                    atlas_dir.display(),
                    e
                )),
            })?;
            let db_path = atlas_dir.join("atlas.db");
            Arc::new(Store::open_db(&db_path).map_err(|e| OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: db_path.display().to_string(),
                storage: "persistent".to_string(),
                file_count: None,
                suggestion: None,
                error: Some(format!("Failed to open database: {e:#}")),
            })?)
        }
        "memory" => Arc::new(Store::open_in_memory().map_err(|e| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: "memory".to_string(),
            file_count: None,
            suggestion: None,
            error: Some(format!("Failed to open in-memory store: {e:#}")),
        })?),
        _ => unreachable!("storage validated above"),
    };

    store.init_schema().map_err(|e| OpenProjectResult {
        ok: false,
        active_project: canonical.display().to_string(),
        db_path: store.db_path().display().to_string(),
        storage: storage.clone(),
        file_count: None,
        suggestion: None,
        error: Some(format!("Schema init failed: {e:#}")),
    })?;

    let mut file_count: Option<usize> = None;
    let mut suggestion: Option<String> = if storage == "persistent" {
        persistent_index_mode.as_ref().map(|index_mode| {
            format!(
                "Reusable persistent index detected via project status (mode={index_mode}); opened .atlas/atlas.db."
            )
        })
    } else if force_memory {
        persistent_index_mode.as_ref().map(|index_mode| {
            format!(
                "Forced in-memory storage despite an existing persistent index (mode={index_mode}); this session starts empty and will not use .atlas/atlas.db."
            )
        })
    } else {
        None
    };

    // For responsiveness, plain `open_project` does not walk large trees.
    // Callers that need the estimate can opt in with `scan_files=true`.
    if scan_files {
        let config = atlas_engine::discovery::DiscoveryConfig::default();
        if let Ok(discovered) = atlas_engine::discovery::discover_files(&canonical, &config) {
            file_count = Some(discovered.len());
            if discovered.len() > LARGE_PROJECT_FILE_COUNT {
                append_suggestion(
                    &mut suggestion,
                    format!(
                        "Large project detected ({} files). After open_project activates it, call index with background=true if the client timeout budget is short.",
                        discovered.len()
                    ),
                );
            }
        }
    }

    let db_path = store.db_path().display().to_string();
    let active_project = canonical.display().to_string();

    Ok(PreparedProject {
        project_root: canonical,
        store,
        result: OpenProjectResult {
            ok: true,
            active_project,
            db_path,
            storage,
            file_count,
            suggestion,
            error: None,
        },
    })
}

fn unsupported_index_param(args: &serde_json::Value) -> Option<&'static str> {
    ["index", "analysis", "include", "exclude"]
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

fn append_suggestion(existing: &mut Option<String>, message: String) {
    match existing {
        Some(current) => {
            current.push(' ');
            current.push_str(&message);
        }
        None => *existing = Some(message),
    }
}
