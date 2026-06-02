//! Status tools: project overview and file listing.

use std::collections::HashMap;

use atlas_engine::{Language, LanguageCapabilityProfile};

use super::ToolRouter;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_status(&self) -> (String, bool) {
        let stats = match self.store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {e}"), true),
        };
        let lazy_stats = self.store.get_lazy_dataflow_stats().ok();
        let layer_counts = self
            .store
            .count_fresh_file_extraction_state()
            .unwrap_or_default();
        let active_jobs = self.store.list_active_extraction_jobs().unwrap_or_default();

        let mut fresh_layers: HashMap<(String, String), i64> = HashMap::new();
        for (layer, status, count) in &layer_counts {
            fresh_layers.insert((layer.clone(), status.clone()), *count);
        }
        let complete = |layer: &str| -> i64 {
            fresh_layers
                .get(&(layer.to_string(), "complete".to_string()))
                .copied()
                .unwrap_or(0)
        };
        let manifest_complete = complete("manifest");
        let structural_complete = complete("structural");
        let dataflow_file_complete = complete("dataflow");

        // Determine index mode:
        //   "none"           — no files indexed
        //   "manifest"       — files/basic top-level symbols only
        //   "partial_structural" — some fresh files have structural facts
        //   "structural+lazy"— unit-level lazy dataflow state exists
        //   "full"           — dataflow was explicitly built via index --analysis full
        //                       (data_nodes exist but NO lazy unit state)
        //   "structural"     — files indexed, no dataflow, no lazy unit state
        let index_mode = if stats.total_files == 0 {
            "none"
        } else if structural_complete == 0 {
            if manifest_complete > 0 {
                "manifest"
            } else {
                "unknown"
            }
        } else if structural_complete < stats.total_files as i64 {
            if lazy_stats.as_ref().is_some_and(|l| l.total_unit_states > 0) {
                "partial_structural+lazy"
            } else {
                "partial_structural"
            }
        } else if lazy_stats.as_ref().is_some_and(|l| l.total_unit_states > 0) {
            // Lazy unit state exists — the index was structural, dataflow came from lazy.
            "structural+lazy"
        } else if dataflow_file_complete >= stats.total_files as i64
            || lazy_stats.as_ref().is_some_and(|l| l.has_dataflow)
        {
            // Dataflow exists but no lazy unit state — explicit full index.
            "full"
        } else {
            "structural"
        };

        let index_hint = if stats.total_files == 0 {
            Some(
                "The project has not been indexed yet. For large projects, run the 'index' tool with background=true to build the fast manifest layer; scoped search/context/trace will perform deeper lazy parsing on demand.",
            )
        } else {
            None
        };

        let next_action = if stats.total_files == 0 {
            Some(json!({
                "tool": "index",
                "args": { "background": true },
                "reason": "Build the fast manifest layer without blocking MCP startup. This does not perform full structural parsing."
            }))
        } else if index_mode == "manifest" {
            Some(json!({
                "tool": "search",
                "args": { "scope": "project-relative directory or file", "background": false },
                "reason": "The project is in lazy mode. Use scoped queries; small scopes are structurally parsed on demand, large scopes remain manifest-level and ask you to narrow."
            }))
        } else {
            None
        };

        // Build per-language capability summary for languages present in the project.
        let mut lang_caps = Vec::new();
        let mut sorted_langs: Vec<&(String, i64)> = stats.files_by_language.iter().collect();
        sorted_langs.sort_by(|a, b| a.0.cmp(&b.0));
        for (lang_name, _count) in &sorted_langs {
            if let Some(lang) = Language::from_str(lang_name) {
                let profile = LanguageCapabilityProfile::for_language(lang);
                lang_caps.push(json!({
                    "language": lang_name,
                    "capability_level": profile.capability_level.as_str(),
                    "confidence_floor": profile.confidence_floor,
                }));
            }
        }

        // Build lazy_dataflow block
        let lazy_dataflow = lazy_stats
            .as_ref()
            .map(|l| {
                json!({
                    "enabled": true,
                    "unit_states": l.total_unit_states,
                    "partial_unit_states": l.partial_unit_states,
                })
            })
            .unwrap_or(json!({
                "enabled": true,
                "unit_states": 0,
                "partial_unit_states": 0,
            }));

        // Determine storage mode from db_path
        let db_path = self.store.db_path().to_string_lossy().to_string();
        let storage = if db_path == ":memory:" {
            "memory"
        } else {
            "persistent"
        };

        (
            serde_json::to_string_pretty(&json!({
                "project": {
                    "active_project": self.project_root.to_string_lossy(),
                    "db_path": db_path,
                    "storage": storage,
                },
                "summary": {
                    "files": stats.total_files,
                    "symbols": stats.total_symbols,
                    "references": stats.total_references,
                    "edges": stats.total_edges,
                    "unresolved_references": stats.unresolved_references,
                },
                "index": {
                    "mode": index_mode,
                    "fresh_layers": layer_counts.iter().map(|(layer, status, count)| json!({
                        "layer": layer,
                        "status": status,
                        "files": count,
                    })).collect::<Vec<_>>(),
                    "lazy_dataflow": lazy_dataflow,
                    "active_extraction_jobs": active_jobs.len(),
                    "hint": index_hint,
                    "next_action": next_action,
                },
                "database": {
                    "sqlite_version": stats.sqlite_version,
                },
                "server": {
                    "atlas_version": env!("CARGO_PKG_VERSION"),
                    "tool_contract_version": 1,
                    "compiled_features": compiled_features(),
                },
                "language_capabilities": lang_caps,
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    pub(crate) fn handle_jobs(&self) -> (String, bool) {
        match self.store.list_active_extraction_jobs() {
            Ok(jobs) => (
                serde_json::to_string_pretty(&json!({
                    "active_jobs": jobs.iter().map(|job| json!({
                        "job_id": job.job_id,
                        "file_id": job.file_id.to_hex(),
                        "unit_id": job.unit_id.map(unit_id_hex),
                        "layer": job.layer,
                        "status": job.status,
                        "trigger_query": job.trigger_query,
                        "started_at": job.started_at,
                        "budget_ms": job.budget_ms,
                    })).collect::<Vec<_>>(),
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            Err(e) => (format!("Error listing active extraction jobs: {e}"), true),
        }
    }

    pub(crate) fn handle_files(&self) -> (String, bool) {
        match self.store.list_files() {
            Ok(files) => (
                serde_json::to_string_pretty(&json!({
                    "files": files.iter().map(|f| json!({
                        "path": f.path,
                        "language": f.language.as_str(),
                        "status": f.status.as_str(),
                    })).collect::<Vec<_>>(),
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            Err(e) => (format!("Error listing files: {e}"), true),
        }
    }
}

fn unit_id_hex(unit_id: [u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in unit_id {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn compiled_features() -> Vec<&'static str> {
    LanguageCapabilityProfile::all_compiled()
        .into_iter()
        .map(|p| match p.language.as_str() {
            "typescript" => "typescript",
            "javascript" => "javascript",
            "python" => "python",
            "java" => "java",
            "c" => "c",
            "cpp" => "cpp",
            "arkts" => "arkts",
            "go" => "go",
            "csharp" => "csharp",
            "rust" => "rust",
            "php" => "php",
            "ruby" => "ruby",
            "kotlin" => "kotlin",
            "cangjie" => "cangjie",
            _ => "unknown",
        })
        .collect()
}
