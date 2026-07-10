//! File-level dependency handlers: manifest and structural modes.
//!
//! Moved out of `mod.rs` for readability (DEBT-3); no logic changes.

use super::*;
use crate::tools::tool_schemas::merge_edge_deps;

impl ToolRouter {
    // ── file_dependencies ────────────────────────────────────────────

    /// Handle `file_dependencies` tool — resolve file_path → file_id,
    /// dispatch by `direction`.
    pub(crate) fn handle_file_dependencies(&self, args: &Value) -> (String, bool) {
        let file_path = get_str(args, "file_path");
        if file_path.is_empty() {
            return ("Missing required 'file_path' parameter".to_string(), true);
        }
        let direction = get_str(args, "direction");
        if !matches!(direction, "incoming" | "outgoing" | "both" | "") {
            return (
                format!(
                    "Unknown direction: '{direction}'. Must be one of: incoming, outgoing, both"
                ),
                true,
            );
        }
        let analysis_mode = get_str(args, "analysis");
        let is_manifest = analysis_mode.is_empty() || analysis_mode == "manifest";
        if !is_manifest && analysis_mode != "structural" {
            return (
                format!(
                    "Unknown analysis mode: '{analysis_mode}'. Must be one of: manifest, structural"
                ),
                true,
            );
        }

        // Resolve file_path to file_id for sub-handlers
        let clean = file_path.trim_start_matches("./").trim_start_matches('/');
        let file_id = {
            let active = self.project();
            match active.store.resolve_file_id(&active.root, clean) {
                Ok(Some(id)) => id,
                Ok(None) => return (format!("File not found: {file_path}"), true),
                Err(e) => return (format!("Failed to resolve file: {e}"), true),
            }
        };

        if is_manifest {
            return self.handle_file_dependencies_manifest(file_id, direction, args);
        }

        // ── structural mode ─────────────────────────────────────────────
        let mut lazy_warnings = Vec::new();
        let mut built_file_count = 0usize;
        let mut focus_result = None;
        let mut _capability_mask = atlas_engine::structs::FactCoverage::default();
        let mut _coverage = "full";
        let mut _reason: Option<&str> = None;

        let has_full_index = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };
        if !has_full_index {
            // Construct a Calls intent with the resolved file_id so focus
            // extraction uses a FocusSeed::File seed.  No QueryIntent variant
            // accepts multiple file_ids (e.g. edge-dependent candidates), so
            // we scope extraction to the primary file.  The closure engine
            // expands via CallGraph + ImportNeighborhood strategies which is
            // appropriate for structural dependency extraction.  (P1-F6)
            let intent = Some(atlas_engine::QueryIntent::Calls {
                symbol_name: String::new(),
                file_id: Some(file_id),
                symbol_id: None,
                direction: None,
                depth: None,
            });
            let (prepared, focus_warnings) = self.prepare_focus_query(intent);
            lazy_warnings = focus_warnings;
            built_file_count = prepared.as_ref().map(|r| r.built_files.len()).unwrap_or(0);
            focus_result = prepared;
        } else {
            _capability_mask = self.project().store.derive_capability_for_files(&[file_id]);
        }

        let file_id_hex = file_id.to_hex();
        let mut mapped = serde_json::Map::new();
        mapped.insert("file_id".into(), Value::String(file_id_hex));
        if let Some(v) = args.get("limit") {
            mapped.insert("limit".into(), v.clone());
        }
        let mapped_args = Value::Object(mapped);

        match direction {
            "incoming" => {
                let (out, err) = self.handle_dependents(&mapped_args);
                let body = serde_json::from_str::<Value>(&out).unwrap_or_default();
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
            }
            "outgoing" | "" => {
                let (out, err) = self.handle_dependencies(&mapped_args);
                let body = serde_json::from_str::<Value>(&out).unwrap_or_default();
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&mapped_args);
                let (in_str, in_err) = self.handle_dependents(&mapped_args);
                let body = json!({
                    "outgoing": serde_json::from_str::<Value>(&out_str).unwrap_or_default(),
                    "incoming": serde_json::from_str::<Value>(&in_str).unwrap_or_default(),
                });
                let summary = if built_file_count > 0 {
                    format!("Lazy-built {built_file_count} files (structural mode)")
                } else {
                    "Full index available".into()
                };
                let err = out_err || in_err;
                let lr = AnalysisEnvelope::new("file_dependencies", args)
                    .with_lazy_warnings(lazy_warnings)
                    .with_is_error(err);
                let lr = if let Some(ref result) = focus_result {
                    crate::tools::apply_focus_result_to_lr(lr, result)
                } else {
                    lr.with_analysis_scope("structural".into())
                        .with_analysis_summary(summary)
                };
                lr.build(body, self)
            }
            _ => unreachable!("direction was validated above"),
        }
    }

    /// Manifest-mode file_dependencies — reads existing DB facts directly,
    /// no lazy structural extraction.
    fn handle_file_dependencies_manifest(
        &self,
        file_id: FileId,
        direction: &str,
        args: &Value,
    ) -> (String, bool) {
        let file_id_hex = file_id.to_hex();
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        match direction {
            "incoming" => {
                let (out_str, out_err) = self.handle_dependents(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err;

                // Supplement with symbol_edges-based re-export / call dependencies
                let edge_deps = self.manifest_edge_dependents(
                    &file_id,
                    limit.saturating_sub(
                        serde_json::from_str::<Value>(&out_str)
                            .ok()
                            .and_then(|v| v["total_dependents"].as_u64())
                            .unwrap_or(0) as usize,
                    ),
                );
                let mut value =
                    serde_json::from_str::<Value>(&out_str).unwrap_or_else(|_| json!({}));
                merge_edge_deps(&mut value, &edge_deps, "dependents", "total_dependents");
                let resp =
                    add_manifest_analysis(serde_json::to_string_pretty(&value).unwrap_or_default());
                (resp, err)
            }
            "outgoing" | "" => {
                let (out_str, out_err) = self.handle_dependencies(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err;

                // Supplement with symbol_edges-based export dependencies
                let edge_deps = self.manifest_edge_dependencies(
                    &file_id,
                    limit.saturating_sub(
                        serde_json::from_str::<Value>(&out_str)
                            .ok()
                            .and_then(|v| v["total_dependencies"].as_u64())
                            .unwrap_or(0) as usize,
                    ),
                );
                let mut value =
                    serde_json::from_str::<Value>(&out_str).unwrap_or_else(|_| json!({}));
                merge_edge_deps(&mut value, &edge_deps, "dependencies", "total_dependencies");
                let resp =
                    add_manifest_analysis(serde_json::to_string_pretty(&value).unwrap_or_default());
                (resp, err)
            }
            "both" => {
                let (out_str, out_err) = self.handle_dependencies(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let (in_str, in_err) = self.handle_dependents(&json!({
                    "file_id": file_id_hex,
                    "limit": limit,
                }));
                let err = out_err || in_err;

                let edge_out = self.manifest_edge_dependencies(&file_id, limit);
                let edge_in = self.manifest_edge_dependents(&file_id, limit);

                let mut outgoing = serde_json::from_str::<Value>(&out_str).unwrap_or_default();
                let mut incoming = serde_json::from_str::<Value>(&in_str).unwrap_or_default();
                merge_edge_deps(
                    &mut outgoing,
                    &edge_out,
                    "dependencies",
                    "total_dependencies",
                );
                merge_edge_deps(&mut incoming, &edge_in, "dependents", "total_dependents");

                let result = json!({
                    "outgoing": outgoing,
                    "incoming": incoming,
                    "analysis": manifest_analysis_value(),
                });
                (
                    serde_json::to_string_pretty(&result).unwrap_or_default(),
                    err,
                )
            }
            _ => unreachable!("direction was validated above"),
        }
    }

    /// Query symbol_edges for incoming file dependencies (manifest mode).
    /// Returns files whose symbols have edges targeting symbols in `file_id`.
    fn manifest_edge_dependents(&self, file_id: &FileId, max_results: usize) -> Value {
        if max_results == 0 {
            return json!([]);
        }
        let our_symbols = match self.project().store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.project().store.find_edges_for_files(&[*file_id]) {
            Ok(e) => e,
            Err(_) => return json!([]),
        };

        // Incoming: edges where target is in our file → source's file depends on us
        let mut source_ids: HashSet<SymbolId> = HashSet::new();
        for edge in &edges {
            if our_set.contains(&edge.target) && !our_set.contains(&edge.source) {
                source_ids.insert(edge.source);
            }
        }

        if source_ids.is_empty() {
            return json!([]);
        }
        let ids_vec: Vec<SymbolId> = source_ids.into_iter().collect();
        let symbols = match self.project().store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
            if file_paths.insert(path.clone()) {
                results.push(json!({
                    "file": path,
                    "import": "symbol_edge",
                }));
            }
        }
        json!(results)
    }

    /// Query symbol_edges for outgoing file dependencies (manifest mode).
    /// Returns files whose symbols are targeted by symbols in `file_id`.
    fn manifest_edge_dependencies(&self, file_id: &FileId, max_results: usize) -> Value {
        if max_results == 0 {
            return json!([]);
        }
        let our_symbols = match self.project().store.find_symbols_by_file(file_id) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        if our_symbols.is_empty() {
            return json!([]);
        }

        let our_ids: Vec<SymbolId> = our_symbols.iter().map(|s| s.id).collect();
        let our_set: HashSet<SymbolId> = our_ids.iter().copied().collect();

        let edges = match self.project().store.find_edges_for_files(&[*file_id]) {
            Ok(e) => e,
            Err(_) => return json!([]),
        };

        // Outgoing: edges where source is in our file → target's file is our dependency
        let mut target_ids: HashSet<SymbolId> = HashSet::new();
        for edge in &edges {
            if our_set.contains(&edge.source) && !our_set.contains(&edge.target) {
                target_ids.insert(edge.target);
            }
        }

        if target_ids.is_empty() {
            return json!([]);
        }
        let ids_vec: Vec<SymbolId> = target_ids.into_iter().collect();
        let symbols = match self.project().store.find_symbols_by_ids(&ids_vec) {
            Ok(s) => s,
            Err(_) => return json!([]),
        };
        let mut file_paths: HashSet<String> = HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        for sym in &symbols {
            if file_paths.len() >= max_results {
                break;
            }
            let path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
            if file_paths.insert(path.clone()) {
                results.push(json!({
                    "module": path,
                    "imported_name": sym.name,
                    "kind": "symbol_edge",
                }));
            }
        }
        json!(results)
    }
}
