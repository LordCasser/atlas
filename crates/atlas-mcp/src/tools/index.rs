//! `atlas_index` MCP tool — trigger project indexing from MCP clients.
//!
//! MCP indexing defaults to fast manifest extraction against the project root.
//! Clients that need complete file dependencies, graph impact, or unscoped
//! search can opt into structural/full analysis explicitly.

use std::sync::Arc;

use atlas_engine::{
    ExtractionMode, FileLock, IndexPipeline, IndexPipelineOptions, IndexPipelineStats,
    ProgressEvent, ProgressSink, guard_against_precision_downgrade,
};

use super::ToolRouter;
use serde_json::json;

/// Progress sink that translates pipeline events into MCP progress
/// notifications over the active progress sender and/or background task manager.
struct McpProgressSink {
    progress_sender: Option<super::ProgressSender>,
    task_manager: Option<Arc<crate::task_manager::TaskManager>>,
    task_id: Option<String>,
}

impl ProgressSink for McpProgressSink {
    fn emit(&self, event: ProgressEvent) {
        match event {
            ProgressEvent::PhaseStarted { phase, .. } => {
                self.send_progress(0.0, Some(format!("{phase}...")));
            }
            ProgressEvent::ItemProgress { phase, completed } => {
                self.send_progress(0.5, Some(format!("{phase}: {completed} items")));
            }
            ProgressEvent::PhaseFinished {
                phase,
                succeeded,
                detail,
                ..
            } => {
                self.send_progress(1.0, detail.or(Some(format!("{phase} done: {succeeded}"))));
            }
            ProgressEvent::Warning { phase, message } => {
                tracing::warn!("[{phase}] {message}");
            }
            ProgressEvent::Cancelled { last_phase } => {
                tracing::info!("Index cancelled at {last_phase}");
            }
        }
    }
}

impl McpProgressSink {
    fn send_progress(&self, fraction: f64, message: Option<String>) {
        if let Some(ref sender) = self.progress_sender {
            let _ = sender.send((fraction, None, message.clone()));
        }
        if let Some(ref tm) = self.task_manager {
            if let Some(ref tid) = self.task_id {
                let pct = (fraction * 100.0).clamp(0.0, 100.0);
                tm.update_progress(tid, pct, &message.unwrap_or_default());
            }
        }
    }
}

/// Maximum number of include/exclude glob patterns accepted per index request.
const MAX_INDEX_PATTERNS: usize = 100;

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
    ///   analysis: "manifest" (default), "structural", or "full"
    ///
    /// If [`ToolCallContext::progress_sender`] is set, progress notifications are sent at each
    /// pipeline phase (discovery, extraction, resolution, graph build).
    ///
    /// Returns a JSON IndexResult with indexing statistics.
    pub(crate) fn handle_index(
        &self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let mode = match parse_analysis_mode(args) {
            Ok(mode) => mode,
            Err(err) => return (index_error_result(err), true),
        };
        let force_reindex = args
            .get("force_reindex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if background {
            if let Err(err) =
                guard_against_precision_downgrade(&self.store, &mode, force_reindex, "MCP index")
                    .map_err(|e| e.to_string())
            {
                return (index_error_result(err), true);
            }
            self.invalidate_manual_full_index_cache();
            return self.handle_index_background(args, mode, force_reindex);
        }

        let start = std::time::Instant::now();

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

        if exclude_patterns.len() > MAX_INDEX_PATTERNS {
            return (
                format!(
                    "exclude patterns ({}) exceed maximum of {}",
                    exclude_patterns.len(),
                    MAX_INDEX_PATTERNS,
                ),
                true,
            );
        }
        if include_patterns.len() > MAX_INDEX_PATTERNS {
            return (
                format!(
                    "include patterns ({}) exceed maximum of {}",
                    include_patterns.len(),
                    MAX_INDEX_PATTERNS,
                ),
                true,
            );
        }

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
                        "Cannot acquire exclusive lock (another atlas process may be indexing): {e:#}"
                    ));
                    let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
                    return (json, true);
                }
            }
        } else {
            None
        };
        if let Err(err) =
            guard_against_precision_downgrade(&self.store, &mode, force_reindex, "MCP index")
                .map_err(|e| e.to_string())
        {
            result.errors.push(err);
            let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
            return (json, true);
        }

        // Run the index pipeline
        let sink = McpProgressSink {
            progress_sender: ctx.progress_sender.clone(),
            task_manager: None,
            task_id: None,
        };
        match run_mcp_index(
            &self.store,
            &self.project_root,
            mode,
            include_patterns,
            exclude_patterns,
            &sink,
        ) {
            Ok(stats) => {
                result.ok = true;
                result.files_discovered = stats.discovered;
                result.files_indexed = stats.indexed;
                result.files_failed = stats.failed;
                result.symbols_found = stats.symbols;
                result.references_resolved = stats.resolved;
                // Re-check layer distribution after any explicit MCP index.
                self.invalidate_manual_full_index_cache();
            }
            Err(e) => {
                result.errors.push(format!("Index failed: {e:#}"));
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

    fn handle_index_background(
        &self,
        args: &serde_json::Value,
        mode: ExtractionMode,
        force_reindex: bool,
    ) -> (String, bool) {
        let task_id = self.task_manager.create_task("index", "index");
        let auto_background = args
            .get("_auto_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tid = task_id.clone();
        let task_manager = self.task_manager.clone();
        let store = self.store.clone();
        let project_root = self.project_root.clone();

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

        if exclude_patterns.len() > MAX_INDEX_PATTERNS {
            return (
                format!(
                    "exclude patterns ({}) exceed maximum of {}",
                    exclude_patterns.len(),
                    MAX_INDEX_PATTERNS,
                ),
                true,
            );
        }
        if include_patterns.len() > MAX_INDEX_PATTERNS {
            return (
                format!(
                    "include patterns ({}) exceed maximum of {}",
                    include_patterns.len(),
                    MAX_INDEX_PATTERNS,
                ),
                true,
            );
        }

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
                                "Cannot acquire exclusive lock (another atlas process may be indexing): {e:#}"
                            ),
                        );
                        return;
                    }
                }
            } else {
                None
            };
            if let Err(err) =
                guard_against_precision_downgrade(&store, &mode, force_reindex, "MCP index")
                    .map_err(|e| e.to_string())
            {
                task_manager.fail_task(&tid, &err);
                return;
            }

            let sink = McpProgressSink {
                progress_sender: None,
                task_manager: Some(task_manager.clone()),
                task_id: Some(tid.clone()),
            };

            match run_mcp_index(
                &store,
                &project_root,
                mode,
                include_patterns,
                exclude_patterns,
                &sink,
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
                    task_manager.fail_task(&tid, &format!("Index failed: {e:#}"));
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
    sink: &dyn ProgressSink,
) -> anyhow::Result<IndexPipelineStats> {
    let options = IndexPipelineOptions::new(mode)
        .with_include_patterns(include_patterns)
        .with_exclude_patterns(exclude_patterns);

    let pipeline = IndexPipeline::new(Arc::clone(store), project_root.to_path_buf(), options);

    pipeline.run(sink, &mut || false)
}

fn parse_analysis_mode(args: &serde_json::Value) -> Result<ExtractionMode, String> {
    let analysis = args
        .get("analysis")
        .and_then(|v| v.as_str())
        .unwrap_or("manifest");
    atlas_engine::parse_analysis_mode(analysis).map_err(|e| e.to_string())
}

fn index_error_result(error: String) -> String {
    serde_json::to_string(&IndexResult {
        ok: false,
        files_discovered: 0,
        files_indexed: 0,
        files_failed: 0,
        symbols_found: 0,
        references_resolved: 0,
        errors: vec![error],
        duration_ms: 0,
        warning: None,
    })
    .unwrap_or_else(|e| e.to_string())
}
