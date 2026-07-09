//! Status tools: project overview and file listing.

use atlas_engine::{Language, LanguageCapabilityProfile};

use super::ToolRouter;

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_status(&self) -> (String, bool) {
        let stats = match self.project().store.get_stats() {
            Ok(s) => s,
            Err(e) => return (format!("Error getting stats: {e}"), true),
        };
        let layer_counts = self
            .project()
            .store
            .count_fresh_file_extraction_state()
            .unwrap_or_default();
        let active_jobs = self
            .project()
            .store
            .list_active_extraction_jobs()
            .unwrap_or_default();

        let catalog_tier = self
            .project()
            .store
            .read_catalog_tier()
            .unwrap_or_else(|_| "unknown".to_string());

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
            let df_stats = self.project().store.get_lazy_dataflow_stats().ok();
            let (files_with_dataflow, _structural, _manifest, files_with_cfg) = self
                .project()
                .store
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
            df.as_object_mut()
                .unwrap()
                .insert("files_with_cfg".to_string(), json!(files_with_cfg));
            df
        };

        let db_path = self.project().store.db_path().to_string_lossy().to_string();

        // ── SQLite cache diagnostics ──────────────────────────────────────
        let cache_stats = self.project().store.get_cache_stats().ok();
        let diagnostics = {
            let mut diag = json!({
                "storage_hierarchy": {
                    "model": "single persistent SQLite DB with transparent page cache",
                    "layers": {
                        "l1_page_cache": "SQLite in-process page cache — transparent, 64 MB default, evicts least-recently-used pages",
                        "l2_durable_db": "project/.atlas/atlas.db — WAL journal, 256 MB mmap, durable across restarts",
                        "l3_focus_extraction": "on-demand structural extraction from source files when symbols are not yet indexed"
                    }
                }
            });
            if let Some(ref cs) = cache_stats {
                let total_db_kib = cs.page_count.saturating_mul(cs.page_size) / 1024;
                let used_pages = cs.page_count.saturating_sub(cs.freelist_count);
                let used_kib = used_pages.saturating_mul(cs.page_size) / 1024;
                let file_kib = (cs.db_file_size_bytes / 1024) as i64;
                diag.as_object_mut().unwrap().insert(
                    "sqlite_cache".to_string(),
                    json!({
                        "page_count": cs.page_count,
                        "page_size_bytes": cs.page_size,
                        "freelist_count": cs.freelist_count,
                        "cache_size_kib": cs.cache_size_kib,
                        "db_file_size_bytes": cs.db_file_size_bytes,
                        "derived": {
                            "total_db_kib": total_db_kib,
                            "used_db_kib": used_kib,
                            "file_on_disk_kib": file_kib,
                            "fragmentation_ratio": if cs.page_count > 0 {
                                (cs.freelist_count as f64 / cs.page_count as f64 * 1000.0).round() / 1000.0
                            } else { 0.0 },
                            "cache_coverage_ratio": if total_db_kib > 0 {
                                (cs.cache_size_kib as f64 / total_db_kib as f64 * 1000.0).round() / 1000.0
                            } else { 0.0 },
                        }
                    }),
                );
            }
            diag
        };

        (
            serde_json::to_string_pretty(&json!({
                "project": {
                    "active_project": self.project().root.to_string_lossy(),
                    "db_path": db_path,
                },
                "summary": {
                    "files": stats.total_files,
                    "symbols": stats.total_symbols,
                    "references": stats.total_references,
                    "edges": stats.total_edges,
                    "unresolved_references": stats.unresolved_references,
                },
                "index": {
                    "catalog_tier": catalog_tier,
                    "fresh_layers": layer_counts.iter().map(|(layer, status, count)| json!({
                        "layer": layer,
                        "status": status,
                        "files": count,
                    })).collect::<Vec<_>>(),
                    "lazy_dataflow": lazy_dataflow,
                    "active_extraction_jobs": active_jobs.len(),
                },
                "database": {
                    "sqlite_version": stats.sqlite_version,
                },
                "diagnostics": diagnostics,
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
        match self.project().store.list_active_extraction_jobs() {
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
        match self.project().store.list_files() {
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
    use std::path::PathBuf;
    use std::sync::Arc;

    use atlas_engine::Store;
    use serde_json::Value;

    use crate::tools::ToolRouter;

    #[test]
    fn status_lazy_dataflow_includes_has_dataflow() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();

        let stats = store.get_lazy_dataflow_stats().unwrap();
        assert!(
            !stats.has_dataflow,
            "empty DB should have has_dataflow=false"
        );

        let (df, st, mn, cfg) = store.get_capability_counts().unwrap();
        assert_eq!(df, 0);
        assert_eq!(st, 0);
        assert_eq!(mn, 0);
        assert_eq!(cfg, 0);
    }

    #[test]
    fn status_response_omits_legacy_guidance_fields() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));

        let (body, is_error) = router.handle_status();

        assert!(!is_error, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert!(value.get("next_action").is_none(), "{value}");
        assert!(value["index"].get("next_action").is_none(), "{value}");
        assert!(value.get("hint").is_none(), "{value}");
        assert!(value["index"].get("hint").is_none(), "{value}");
    }
}
