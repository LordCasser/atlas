//! `open_project` MCP tool — open, optionally index, and activate a project.
//!
//! Unlike the MCP server's initial project (which is always persistent),
//! `open_project` defaults to `storage: "memory"` and `index: false` for
//! zero-footprint, instant-start temporary sessions. Indexing must be
//! explicitly requested with `index: true`.
//!
//! Long-running project opens can use `background: true`. The tool then returns
//! a `task_id` immediately; `task_status` or `wait_for_task` activates the
//! prepared project once the background task completes.

use std::path::PathBuf;
use std::sync::Arc;

use atlas_engine::{ExtractionMode, Store};
use serde_json::json;

use super::{PendingProjectActivation, ProgressSender, ToolRouter};

/// Result of an open_project invocation.
#[derive(serde::Serialize)]
struct OpenProjectResult {
    ok: bool,
    active_project: String,
    db_path: String,
    storage: String,
    /// Approximate file count, present only when `scan_files=true` or indexing ran.
    file_count: Option<usize>,
    /// Present only when `index: true` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<super::index::IndexResult>,
    /// Suggestion for large projects (e.g. "use analysis='manifest'").
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestion: Option<String>,
    error: Option<String>,
}

struct PreparedProject {
    project_root: PathBuf,
    store: Arc<Store>,
    result: OpenProjectResult,
}

/// Threshold above which we suggest using manifest mode.
const LARGE_PROJECT_FILE_COUNT: usize = 5_000;

impl ToolRouter {
    /// Handle `open_project` tool call.
    ///
    /// Parameters:
    ///   project_path (required): absolute path to the project directory.
    ///   storage (optional): "memory" (default) | "persistent".
    ///   index (optional): whether to run the index pipeline (default: false).
    ///   analysis (optional): "manifest" (default) | "structural" | "full".
    ///   include/exclude (optional): glob patterns for indexing/discovery.
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

        match prepare_project(args, self.progress_sender.clone()) {
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
                index: None,
                suggestion: None,
                error: Some("Missing required parameter: project_path".into()),
            };
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        let task_id = self
            .task_manager
            .create_task("open_project", "open_project");
        let tid = task_id.clone();
        let task_manager = self.task_manager.clone();
        let pending = self.pending_project_activations.clone();
        let owned_args = args.clone();

        std::thread::spawn(move || {
            task_manager.update_progress(&tid, 1.0, "Preparing project...");
            match prepare_project(&owned_args, None) {
                Ok(prepared) => {
                    task_manager.update_progress(
                        &tid,
                        95.0,
                        "Project prepared; activation pending...",
                    );
                    let project_root = prepared.project_root.clone();
                    let result = serde_json::to_value(&prepared.result)
                        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
                    pending.lock().unwrap().insert(
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
                Err(resp) => {
                    let msg = resp
                        .error
                        .clone()
                        .unwrap_or_else(|| "open_project failed".to_string());
                    task_manager.fail_task(&tid, &msg);
                }
            }
        });

        (
            serde_json::to_string_pretty(&json!({
                "background": true,
                "task_id": task_id,
                "tool_name": "open_project",
                "method": "open_project",
                "status": "running",
                "progress": null,
                "note": "open_project is running in background. Use task_status or wait_for_task; completion will activate the prepared project."
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}

fn prepare_project(
    args: &serde_json::Value,
    progress_sender: Option<ProgressSender>,
) -> Result<PreparedProject, OpenProjectResult> {
    let start = std::time::Instant::now();

    let project_path = args["project_path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            index: None,
            suggestion: None,
            error: Some("Missing required parameter: project_path".into()),
        })?;

    let canonical = std::path::Path::new(project_path)
        .canonicalize()
        .map_err(|e| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            index: None,
            suggestion: None,
            error: Some(format!(
                "Project path not found or not accessible: {} ({})",
                project_path, e
            )),
        })?;

    if !canonical.is_dir() {
        return Err(OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: String::new(),
            file_count: None,
            index: None,
            suggestion: None,
            error: Some(format!("Not a directory: {}", canonical.display())),
        });
    }

    let storage = args["storage"].as_str().unwrap_or("memory").to_string();
    if storage != "memory" && storage != "persistent" {
        return Err(OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: storage.clone(),
            file_count: None,
            index: None,
            suggestion: None,
            error: Some(format!(
                "Unknown storage mode '{}'. Valid choices: 'memory', 'persistent'.",
                storage
            )),
        });
    }

    let do_index = args["index"].as_bool().unwrap_or(false);
    let scan_files = args["scan_files"].as_bool().unwrap_or(false);
    let analysis = args["analysis"].as_str().unwrap_or("manifest");
    let mode = match analysis {
        "structural" => ExtractionMode::Structural,
        "full" => ExtractionMode::Full,
        _ => ExtractionMode::Manifest,
    };
    let exclude_patterns = string_array(args, "exclude");
    let include_patterns = string_array(args, "include");

    let store: Arc<Store> = match storage.as_str() {
        "persistent" => {
            let atlas_dir = canonical.join(".atlas");
            std::fs::create_dir_all(&atlas_dir).map_err(|e| OpenProjectResult {
                ok: false,
                active_project: String::new(),
                db_path: String::new(),
                storage: "persistent".to_string(),
                file_count: None,
                index: None,
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
                index: None,
                suggestion: None,
                error: Some(format!("Failed to open database: {:#}", e)),
            })?)
        }
        "memory" => Arc::new(Store::open_in_memory().map_err(|e| OpenProjectResult {
            ok: false,
            active_project: String::new(),
            db_path: String::new(),
            storage: "memory".to_string(),
            file_count: None,
            index: None,
            suggestion: None,
            error: Some(format!("Failed to open in-memory store: {:#}", e)),
        })?),
        _ => unreachable!("storage validated above"),
    };

    store.init_schema().map_err(|e| OpenProjectResult {
        ok: false,
        active_project: canonical.display().to_string(),
        db_path: store.db_path().display().to_string(),
        storage: storage.clone(),
        file_count: None,
        index: None,
        suggestion: None,
        error: Some(format!("Schema init failed: {:#}", e)),
    })?;

    let mut file_count: Option<usize> = None;
    let mut suggestion: Option<String> = None;

    // For responsiveness, plain `open_project(index=false)` does not walk large
    // trees. Callers that need the estimate can opt in with `scan_files=true`;
    // `index=true` discovers files as part of the index pipeline below.
    if scan_files && !do_index {
        let mut config = atlas_engine::discovery::DiscoveryConfig::default();
        if !include_patterns.is_empty() {
            config.include_patterns = include_patterns.clone();
        }
        if !exclude_patterns.is_empty() {
            config.exclude_patterns = exclude_patterns.clone();
        }
        if let Ok(discovered) = atlas_engine::discovery::discover_files(&canonical, &config) {
            file_count = Some(discovered.len());
            if discovered.len() > LARGE_PROJECT_FILE_COUNT
                && !matches!(mode, ExtractionMode::Manifest)
            {
                let mut msg = format!(
                    "Large project detected ({} files). You requested analysis=\"{}\" but manifest mode would be much faster. ",
                    discovered.len(), analysis
                );
                msg.push_str(
                    "Use the default analysis=\"manifest\" for a fast initial index; lazy structural upgrades data on-demand. For deep indexing, use background=true + wait_for_task to avoid blocking.",
                );
                suggestion = Some(msg);
            }
        }
    }

    let mut index_result: Option<super::index::IndexResult> = None;
    if do_index {
        let _lock_guard = if storage == "persistent" {
            Some(
                atlas_engine::FileLock::acquire(&store).map_err(|e| OpenProjectResult {
                    ok: false,
                    active_project: canonical.display().to_string(),
                    db_path: store.db_path().display().to_string(),
                    storage: storage.clone(),
                    file_count,
                    index: None,
                    suggestion: None,
                    error: Some(format!(
                        "Cannot acquire exclusive lock for indexing: {:#}",
                        e
                    )),
                })?,
            )
        } else {
            None
        };

        match super::index::run_index(
            &store,
            &canonical,
            mode,
            &include_patterns,
            &exclude_patterns,
            progress_sender,
        ) {
            Ok(stats) => {
                file_count = Some(stats.discovered);
                index_result = Some(super::index::IndexResult {
                    ok: true,
                    files_discovered: stats.discovered,
                    files_indexed: stats.indexed,
                    files_failed: stats.failed,
                    symbols_found: stats.symbols,
                    references_resolved: stats.resolved,
                    errors: Vec::new(),
                    duration_ms: start.elapsed().as_millis() as u64,
                    warning: None,
                });
            }
            Err(e) => {
                return Err(OpenProjectResult {
                    ok: false,
                    active_project: canonical.display().to_string(),
                    db_path: store.db_path().display().to_string(),
                    storage: storage.clone(),
                    file_count,
                    index: Some(super::index::IndexResult {
                        ok: false,
                        files_discovered: 0,
                        files_indexed: 0,
                        files_failed: 0,
                        symbols_found: 0,
                        references_resolved: 0,
                        errors: vec![format!("{:#}", e)],
                        duration_ms: start.elapsed().as_millis() as u64,
                        warning: None,
                    }),
                    suggestion: None,
                    error: Some("Index failed".into()),
                });
            }
        }
        drop(_lock_guard);
    }

    let db_path = store.db_path().display().to_string();
    let active_project = canonical.display().to_string();
    let duration_ms = start.elapsed().as_millis() as u64;
    if let Some(ref mut ir) = index_result {
        ir.duration_ms = duration_ms;
    }

    // Post-index: suggest background mode for large projects
    if suggestion.is_none() && do_index {
        if let Some(n) = file_count {
            if n > LARGE_PROJECT_FILE_COUNT && duration_ms > 5_000 {
                suggestion = Some(
                    "Large project indexed synchronously. Next time use background=true with wait_for_task to avoid blocking the MCP connection during indexing."
                        .into(),
                );
            }
        }
    }

    Ok(PreparedProject {
        project_root: canonical,
        store,
        result: OpenProjectResult {
            ok: true,
            active_project,
            db_path,
            storage,
            file_count,
            index: index_result,
            suggestion,
            error: None,
        },
    })
}

fn string_array(args: &serde_json::Value, key: &str) -> Vec<String> {
    args[key]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
