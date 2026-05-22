//! Status tools: project overview and file listing.

use crate::types::{Language, LanguageCapabilityProfile};

use super::ToolRouter;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_status(&self) -> (String, bool) {
        let stats = match self.store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {}", e), true),
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

        (
            serde_json::to_string_pretty(&json!({
                "summary": {
                    "files": stats.total_files,
                    "symbols": stats.total_symbols,
                    "references": stats.total_references,
                    "edges": stats.total_edges,
                    "unresolved_references": stats.unresolved_references,
                },
                "database": {
                    "sqlite_version": stats.sqlite_version,
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
