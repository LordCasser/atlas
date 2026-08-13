//! Status tools: project overview and file listing.

use std::collections::HashSet;

use atlas_engine::{Language, LanguageCapabilityProfile, seed_file_inventory_from_scope};

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
        const MAX_ACTIVE_JOBS: usize = 100;
        match self.project().store.list_active_extraction_jobs() {
            Ok(jobs) => {
                let active_jobs = jobs
                    .iter()
                    .take(MAX_ACTIVE_JOBS)
                    .map(|job| {
                        json!({
                            "job_id": job.job_id,
                            "file_id": job.file_id.to_hex(),
                            "unit_id": job.unit_id.map(unit_id_hex),
                            "layer": job.layer,
                            "status": job.status,
                            "trigger_query": job.trigger_query,
                            "started_at": job.started_at,
                            "budget_ms": job.budget_ms,
                        })
                    })
                    .collect::<Vec<_>>();
                let returned = active_jobs.len();
                (
                    serde_json::to_string_pretty(&json!({
                        "active_jobs": active_jobs,
                        "total": jobs.len(),
                        "returned": returned,
                        "truncated": jobs.len() > returned,
                    }))
                    .unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Error listing active extraction jobs: {e}"), true),
        }
    }

    pub(crate) fn handle_files(&self, args: &serde_json::Value) -> (String, bool) {
        const DEFAULT_FILE_LIMIT: usize = 500;
        const MAX_FILE_LIMIT: usize = 1000;
        let limit = super::bounded_usize_arg(args, "limit", DEFAULT_FILE_LIMIT, MAX_FILE_LIMIT);
        let language = super::get_str(args, "language");
        let path_prefix = super::get_str(args, "path_prefix");
        let scope = path_prefix
            .trim()
            .trim_start_matches("./")
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string();
        let indexed_count = self
            .project()
            .store
            .count_files_in_scope(&scope)
            .unwrap_or_default();
        let inventory_count = self
            .project()
            .store
            .count_file_inventory_in_scope(&scope)
            .unwrap_or_default();
        let has_manifest_repo_cache = self
            .project()
            .query_runtime
            .has_repo_cache_for(&self.project().store, atlas_engine::QueryNeed::Manifest);
        if !has_manifest_repo_cache {
            self.project().query_runtime.ensure_focus_started();
        }
        let mut scoped_discovery_complete = None;
        if indexed_count == 0 && inventory_count == 0 {
            match seed_file_inventory_from_scope(
                &self.project().store,
                &self.project().root,
                &scope,
            ) {
                Ok(complete) => scoped_discovery_complete = Some(complete),
                Err(err) => return (format!("Error discovering project files: {err:#}"), true),
            }
        }

        // Fetch one extra row so truncation remains explicit without ever
        // slicing the serialized JSON response at the router boundary.
        let row_limit = limit.saturating_add(1);
        let language_filter = (!language.is_empty()).then_some(language);
        match self
            .project()
            .store
            .list_files_in_scope(&scope, language_filter, row_limit)
        {
            Ok(files) => {
                let mut seen_paths = HashSet::new();
                let indexed_rows_present = !files.is_empty();
                let mut rows: Vec<_> = files
                    .iter()
                    .map(|f| {
                        seen_paths.insert(f.path.clone());
                        json!({
                            "path": f.path,
                            "language": f.language.as_str(),
                            "status": f.status.as_str(),
                        })
                    })
                    .collect();

                let inventory_rows = self
                    .project()
                    .store
                    .list_file_inventory_rows_in_scope(&scope, language_filter, row_limit)
                    .unwrap_or_default();
                let mut inventory_rows_present = false;
                for row in inventory_rows {
                    if seen_paths.contains(&row.path) {
                        continue;
                    }
                    inventory_rows_present = true;
                    rows.push(json!({
                        "path": row.path,
                        "language": row.language,
                        "status": "inventory",
                    }));
                }
                rows.sort_by(|left, right| {
                    left["path"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(right["path"].as_str().unwrap_or_default())
                });
                let truncated = rows.len() > limit;
                rows.truncate(limit);
                let source = match (indexed_rows_present, inventory_rows_present) {
                    (true, true) => "mixed",
                    (true, false) => "indexed",
                    (false, true) => "inventory",
                    (false, false) if indexed_count > 0 => "indexed",
                    (false, false) => "inventory",
                };
                let inventory_count = self
                    .project()
                    .store
                    .count_file_inventory_in_scope(&scope)
                    .unwrap_or_default();
                let inventory_complete = has_manifest_repo_cache
                    || scoped_discovery_complete == Some(true)
                    || self.project().query_runtime.is_tier0_complete();
                let coverage = if inventory_complete {
                    json!({"state": "complete"})
                } else {
                    json!({
                        "state": "partial",
                        "reason": "Background Focus inventory is still discovering project files"
                    })
                };
                (
                    serde_json::to_string_pretty(&json!({
                        "files": rows,
                        "source": source,
                        "coverage": coverage,
                        "inventory_file_count": inventory_count,
                        "returned": rows.len(),
                        "truncated": truncated,
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

fn compiled_features() -> Vec<String> {
    LanguageCapabilityProfile::all_compiled()
        .into_iter()
        .map(|profile| profile.language)
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

    #[test]
    fn project_files_uses_inventory_on_cold_store() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/app.ts"), "export function app() {}\n").unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let router = ToolRouter::new_empty(store, root.path().to_path_buf());

        let (body, is_error) = router.handle_files(&serde_json::json!({"path_prefix": "src"}));

        assert!(!is_error, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["source"], "inventory");
        let files = value["files"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{value}");
        assert_eq!(files[0]["path"], "src/app.ts");
        assert_eq!(files[0]["status"], "inventory");
    }

    #[test]
    fn project_files_applies_language_and_limit_in_the_catalog_query() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        for (path, language) in [
            ("src/app.ts", atlas_engine::Language::TypeScript),
            ("src/lib.rs", atlas_engine::Language::Rust),
        ] {
            store
                .upsert_file(&atlas_engine::FileInfo {
                    file_id: atlas_engine::FileId::generate(path),
                    path: path.into(),
                    language,
                    content_hash: "hash".into(),
                    status: atlas_engine::ParseStatus::Success,
                })
                .unwrap();
        }
        let router = ToolRouter::new_empty(store, root.path().to_path_buf());

        let (body, is_error) = router.handle_files(&serde_json::json!({
            "path_prefix": "src",
            "language": "rust",
            "limit": 1
        }));

        assert!(!is_error, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["files"].as_array().unwrap().len(), 1, "{value}");
        assert_eq!(value["files"][0]["path"], "src/lib.rs");
        assert_eq!(value["source"], "indexed");
    }
}
