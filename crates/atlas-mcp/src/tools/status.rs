//! Status tools: project overview and file listing.

use atlas_engine::{Language, LanguageCapabilityProfile};

use super::ToolRouter;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_status(&self) -> (String, bool) {
        let stats = match self.store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {}", e), true),
        };
        let lazy_stats = self.store.get_lazy_stats().ok();

        // Determine index mode:
        //   "none"           — no files indexed
        //   "structural+lazy"— analysis_artifacts exist (lazy query was triggered)
        //   "full"           — dataflow was explicitly built via index --analysis full
        //                       (data_nodes exist but NO lazy artifacts)
        //   "structural"     — files indexed, no dataflow, no lazy artifacts
        let index_mode = if stats.total_files == 0 {
            "none"
        } else if lazy_stats.as_ref().map_or(false, |l| l.total_artifacts > 0) {
            // Lazy artifacts exist — the index was structural, dataflow came from lazy.
            "structural+lazy"
        } else if lazy_stats.as_ref().map_or(false, |l| l.has_dataflow) {
            // Dataflow exists but no lazy artifacts — explicit full index.
            "full"
        } else {
            "structural"
        };

        // Build per-language capability summary for languages present in the project.
        let mut lang_caps = Vec::new();
        for (lang_name, _count) in &stats.files_by_language {
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
        let lazy_dataflow = lazy_stats.as_ref().map(|l| json!({
            "enabled": true,
            "artifacts": l.total_artifacts,
            "partial_artifacts": l.partial_artifacts,
        })).unwrap_or(json!({
            "enabled": true,
            "artifacts": 0,
            "partial_artifacts": 0,
        }));

        // Determine storage mode from db_path
        let db_path = self.store.db_path().to_string_lossy().to_string();
        let storage = if db_path == ":memory:" { "memory" } else { "persistent" };

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
                    "lazy_dataflow": lazy_dataflow,
                },
                "database": {
                    "sqlite_version": stats.sqlite_version,
                    "schema_version": self.store.schema_version().unwrap_or(0),
                    "app_schema_version": atlas_engine::CURRENT_SCHEMA_VERSION,
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

    pub(crate) fn handle_files(&self) -> (String, bool) {
        match self.store.list_files() {
            Ok(files) => (
                serde_json::to_string_pretty(&json!({
                    "count": files.len(),
                    "files": files.iter().map(|f| json!({
                        "path": f.path,
                        "language": f.language.as_str(),
                        "status": f.status.as_str(),
                    })).collect::<Vec<_>>(),
                }))
                .unwrap_or_else(|e| e.to_string()),
                false,
            ),
            Err(e) => (format!("Error listing files: {}", e), true),
        }
    }
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
            "bash" => "bash",
            "cangjie" => "cangjie",
            _ => "unknown",
        })
        .collect()
}
