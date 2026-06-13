//! Status tools: project overview and file listing.

use atlas_engine::{Language, LanguageCapabilityProfile, Store};

use super::ToolRouter;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_status(&self) -> (String, bool) {
        let stats = match self.active.store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {e}"), true),
        };
        let layer_counts = self
            .active.store
            .count_fresh_file_extraction_state()
            .unwrap_or_default();
        let active_jobs = self.active.store.list_active_extraction_jobs().unwrap_or_default();

        let index_mode = self
            .active.store
            .read_index_mode()
            .unwrap_or_else(|_| "unknown".to_string());

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
        let lazy_dataflow = {
            let df_stats = self.active.store.get_lazy_dataflow_stats().ok();
            let (files_with_dataflow, _structural, _manifest, files_with_cfg) = self
                .active.store
                .get_capability_counts()
                .unwrap_or((0, 0, 0, 0));

            let mut df = if let Some(ref s) = df_stats {
                json!({
                    "enabled": true,
                    "unit_states": s.total_unit_states,
                    "partial_unit_states": s.partial_unit_states,
                    "has_dataflow": s.has_dataflow,
                })
            } else {
                json!({
                    "enabled": true,
                    "unit_states": 0,
                    "partial_unit_states": 0,
                    "has_dataflow": false,
                })
            };
            df.as_object_mut().unwrap().insert(
                "files_with_dataflow".to_string(),
                json!(files_with_dataflow),
            );
            df.as_object_mut().unwrap().insert(
                "files_with_cfg".to_string(),
                json!(files_with_cfg),
            );
            df
        };

        // Determine storage mode from db_path
        let db_path = self.active.store.db_path().to_string_lossy().to_string();
        let storage = if db_path == ":memory:" {
            "memory"
        } else {
            "persistent"
        };

        (
            serde_json::to_string_pretty(&json!({
                "project": {
                    "active_project": self.active.root.to_string_lossy(),
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
        match self.active.store.list_active_extraction_jobs() {
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

    pub(crate) fn handle_files(&self, args: &serde_json::Value) -> (String, bool) {
        let limit = super::get_u64(args, "limit").map(|v| v as usize);
        let language = super::get_str(args, "language");
        let path_prefix = super::get_str(args, "path_prefix");
        match self.active.store.list_files() {
            Ok(files) => {
                let mut filtered: Vec<_> = files
                    .iter()
                    .filter(|f| {
                        if !path_prefix.is_empty() && !f.path.starts_with(path_prefix) {
                            return false;
                        }
                        if !language.is_empty() && f.language.as_str() != language {
                            return false;
                        }
                        true
                    })
                    .collect();
                if let Some(n) = limit {
                    filtered.truncate(n);
                }
                (
                    serde_json::to_string_pretty(&json!({
                        "files": filtered.iter().map(|f| json!({
                            "path": f.path,
                            "language": f.language.as_str(),
                            "status": f.status.as_str(),
                        })).collect::<Vec<_>>(),
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Error listing files: {e}"), true),
        }
    }
}

/// Read the same index mode that `project(action="status")` reports.
///
/// This is used by `project(action="open", storage="auto")` to decide whether
/// a persistent candidate DB contains a reusable index, instead of guessing
/// from filesystem presence alone.
pub(crate) fn read_index_mode(store: &Store) -> anyhow::Result<String> {
    store.read_index_mode()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lazy_dataflow_includes_has_dataflow() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let stats = store.get_lazy_dataflow_stats().unwrap();
        assert!(!stats.has_dataflow, "empty DB should have has_dataflow=false");

        let (df, st, mn, cfg) = store.get_capability_counts().unwrap();
        assert_eq!(df, 0);
        assert_eq!(st, 0);
        assert_eq!(mn, 0);
        assert_eq!(cfg, 0);
    }
}
