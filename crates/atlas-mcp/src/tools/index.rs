//! `atlas_index` MCP tool — trigger project indexing from MCP clients.
//!
//! MCP indexing always performs fast manifest extraction against the project
//! root. Structural/full parsing is intentionally handled by scoped query tools
//! on demand so MCP clients do not block on large repositories.

use std::sync::Arc;

use atlas_engine::{
    ExtractionMode, FileLock, IndexPipelineOptions, IndexPipelineStats, IndexProgress,
    IndexProgressCallback, run_index_pipeline,
};

use super::ToolRouter;
use serde_json::json;

/// Result of an atlas_index invocation.
#[derive(serde::Serialize, Clone)]
pub(crate) struct IndexResult {
    pub(crate) ok: bool,
    pub(crate) files_discovered: usize,
    pub(crate) files_indexed: usize,
    pub(crate) files_failed: usize,
    pub(crate) symbols_found: usize,
    pub(crate) references_resolved: usize,
    pub(crate) errors: Vec<String>,
    pub(crate) duration_ms: u64,
    /// Warning for large projects that may cause MCP timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
}

impl ToolRouter {
    /// Handle `atlas_index` tool call.
    ///
    /// Parameters:
    ///   include: list of glob patterns to restrict indexing to
    ///   exclude: list of glob patterns to skip (e.g. ["**/test/**", "**/*.test.ts"])
    ///
    /// If [`Self::progress_sender`] is set, progress notifications are sent at each
    /// pipeline phase (discovery, extraction, resolution, graph build).
    ///
    /// Returns a JSON IndexResult with indexing statistics.
    pub(crate) fn handle_index(&self, args: &serde_json::Value) -> (String, bool) {
        if args.get("analysis").is_some() {
            return (reject_analysis_result(), true);
        }

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if background {
            return self.handle_index_background(args);
        }

        let start = std::time::Instant::now();
        let mode = ExtractionMode::Manifest;

        let exclude_patterns: Vec<String> = args["exclude"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let include_patterns: Vec<String> = args["include"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let mut result = IndexResult {
            ok: false,
            files_discovered: 0,
            files_indexed: 0,
            files_failed: 0,
            symbols_found: 0,
            references_resolved: 0,
            errors: Vec::new(),
            duration_ms: 0,
            warning: None,
        };

        // Acquire FileLock for persistent stores to prevent races with CLI
        // or other MCP processes writing the same .atlas/atlas.db.
        let is_persistent = self.store.db_path() != std::path::Path::new(":memory:");
        let _lock_guard = if is_persistent {
            match FileLock::acquire(&self.store) {
                Ok(g) => Some(g),
                Err(e) => {
                    result.errors.push(format!(
                        "Cannot acquire exclusive lock (another atlas process may be indexing): {:#}",
                        e
                    ));
                    let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
                    return (json, true);
                }
            }
        } else {
            None
        };

        // Run the index pipeline
        let progress_sender = self.progress_sender.clone();
        match run_mcp_index(
            &self.store,
            &self.project_root,
            mode,
            include_patterns,
            exclude_patterns,
            progress_sender.map(progress_callback_from_sender),
        ) {
            Ok(stats) => {
                result.ok = true;
                result.files_discovered = stats.discovered;
                result.files_indexed = stats.indexed;
                result.files_failed = stats.failed;
                result.symbols_found = stats.symbols;
                result.references_resolved = stats.resolved;
                // MCP index always produces manifest-only; invalidate any
                // cached "manual full index" flag so the next search/trace
                // re-detects the actual layer distribution.
                self.invalidate_manual_full_index_cache();
            }
            Err(e) => {
                result.errors.push(format!("Index failed: {:#}", e));
            }
        }

        result.duration_ms = start.elapsed().as_millis() as u64;

        // ── Large project / background guidance ────────────────────────────
        if result.duration_ms > 30_000 {
            let mut msg = "Indexing took over 30 seconds. ".to_string();
            msg.push_str(
                "For large projects, use background=true to run indexing asynchronously, then call wait_for_task with the returned task_id to block until completion.",
            );
            append_warning(&mut result.warning, msg);
        } else if result.files_discovered > 5_000 {
            append_warning(
                &mut result.warning,
                "Large project detected. Use background=true and wait_for_task if the client timeout budget is short."
                    .into(),
            );
        }

        let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
        (json, !result.ok)
    }

    fn handle_index_background(&self, args: &serde_json::Value) -> (String, bool) {
        let task_id = self.task_manager.create_task("index", "index");
        let auto_background = args
            .get("_auto_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tid = task_id.clone();
        let task_manager = self.task_manager.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();

        let mode = ExtractionMode::Manifest;
        let exclude_patterns: Vec<String> = args["exclude"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let include_patterns: Vec<String> = args["include"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            task_manager.update_progress(&tid, 1.0, "Starting index...");

            let is_persistent = store.db_path() != std::path::Path::new(":memory:");
            let _lock_guard = if is_persistent {
                match FileLock::acquire(&store) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        task_manager.fail_task(
                            &tid,
                            &format!(
                                "Cannot acquire exclusive lock (another atlas process may be indexing): {:#}",
                                e
                            ),
                        );
                        return;
                    }
                }
            } else {
                None
            };

            task_manager.update_progress(&tid, 5.0, "Discovering and indexing files...");
            let progress = {
                let task_manager = task_manager.clone();
                let tid = tid.clone();
                Arc::new(move |progress: IndexProgress| {
                    task_manager.update_progress(
                        &tid,
                        (progress.fraction * 100.0).clamp(0.0, 100.0),
                        progress.message.as_deref().unwrap_or("Indexing..."),
                    );
                }) as IndexProgressCallback
            };

            match run_mcp_index(
                &store,
                &project_root,
                mode,
                include_patterns,
                exclude_patterns,
                Some(progress),
            ) {
                Ok(stats) => {
                    let mut result = IndexResult {
                        ok: true,
                        files_discovered: stats.discovered,
                        files_indexed: stats.indexed,
                        files_failed: stats.failed,
                        symbols_found: stats.symbols,
                        references_resolved: stats.resolved,
                        errors: Vec::new(),
                        duration_ms: start.elapsed().as_millis() as u64,
                        warning: None,
                    };
                    if result.duration_ms > 30_000 {
                        append_warning(
                            &mut result.warning,
                            "For very large projects, consider running 'atlas index' locally before connecting via MCP to avoid timeout issues."
                                .into(),
                        );
                    }
                    task_manager.complete_task(
                        &tid,
                        serde_json::to_value(&result)
                            .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() })),
                    );
                }
                Err(e) => {
                    task_manager.fail_task(&tid, &format!("Index failed: {:#}", e));
                }
            }
        });

        (
            serde_json::to_string_pretty(&json!({
                "background": true,
                "task_id": task_id,
                "tool_name": "index",
                "method": "index",
                "status": "running",
                "progress": 0.0,
                "progress_message": "queued",
                "auto_background": auto_background,
                "note": "Index is running in background. Poll task_status for progress percentages; use wait_for_task only when the client can safely block."
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}

fn append_warning(slot: &mut Option<String>, message: String) {
    match slot {
        Some(existing) if !existing.is_empty() => {
            existing.push(' ');
            existing.push_str(&message);
        }
        _ => *slot = Some(message),
    }
}

fn run_mcp_index(
    store: &Arc<atlas_engine::Store>,
    project_root: &std::path::Path,
    mode: ExtractionMode,
    include_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
    progress: Option<IndexProgressCallback>,
) -> anyhow::Result<IndexPipelineStats> {
    let mut options = IndexPipelineOptions::new(mode)
        .with_include_patterns(include_patterns)
        .with_exclude_patterns(exclude_patterns);
    if let Some(progress) = progress {
        options = options.with_progress(progress);
    }
    run_index_pipeline(store, project_root, options)
}

fn progress_callback_from_sender(sender: super::ProgressSender) -> IndexProgressCallback {
    Arc::new(move |progress: IndexProgress| {
        let _ = sender.send((progress.fraction, progress.total, progress.message));
    })
}

fn reject_analysis_result() -> String {
    serde_json::to_string(&IndexResult {
        ok: false,
        files_discovered: 0,
        files_indexed: 0,
        files_failed: 0,
        symbols_found: 0,
        references_resolved: 0,
        errors: vec![
            "Unsupported index parameter 'analysis'. The MCP index tool always builds the manifest layer for the active project. Use scoped search/trace for deeper on-demand parsing."
                .into(),
        ],
        duration_ms: 0,
        warning: None,
    })
    .unwrap_or_else(|e| e.to_string())
}
