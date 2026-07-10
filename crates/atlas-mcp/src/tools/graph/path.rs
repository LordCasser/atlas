//! Path finding handler: K-ranked shortest paths between two symbols,
//! with ambiguity resolution, frontier diagnostics, and path-quality insights.

use super::*;

/// Default edge kinds for path finding — call relationships only.
/// Excludes non-control-flow edges (References, TypeOf, Contains, etc.)
/// to avoid semantically meaningless paths in security analysis.
const DEFAULT_PATH_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Implements,
    EdgeKind::RegistersCallback,
];

/// Build resolution metadata for path endpoint responses.
fn build_resolution_meta_for_path(resolution: &SymbolResolution) -> serde_json::Value {
    match resolution {
        SymbolResolution::Single { resolved, .. } => {
            json!({
                "policy": "aggregated",
                "count": 1,
                "matched_candidates": [{
                    "qualified_name": resolved.qualified_name,
                    "file_path": resolved.file_path,
                    "line": resolved.line,
                    "kind": resolved.kind,
                    "language": resolved.language,
                }],
            })
        }
        SymbolResolution::Ambiguous { candidates, .. } => {
            build_resolution_meta(candidates, candidates.len())
        }
        SymbolResolution::NotFound { qname, .. } => {
            json!({
                "policy": "aggregated",
                "count": 0,
                "matched_candidates": [],
                "qname": qname,
            })
        }
    }
}

impl ToolRouter {
    pub(crate) fn handle_path(&self, args: &serde_json::Value) -> (String, bool) {
        // Parse 'from' and 'to' as SymbolInput (string or selector object).
        let from_input = match parse_symbol_field(args, "from") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let to_input = match parse_symbol_field(args, "to") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let from_qname = symbol_input_qname(&from_input);
        let to_qname = symbol_input_qname(&to_input);
        if let Err(e) = crate::tools::validate_symbol_name_length(from_qname) {
            return (e, true);
        }
        if let Err(e) = crate::tools::validate_symbol_name_length(to_qname) {
            return (e, true);
        }
        let max_depth = get_u64(args, "max_depth").unwrap_or(5) as usize;
        let prefer_production = args
            .get("prefer_production")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let direction = Self::resolve_path_direction(args);

        let edge_kind_filter = match Self::resolve_path_edge_kinds(args) {
            Ok(f) => f,
            Err(e) => return (e, true),
        };
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Resolve both sides with Aggregate policy.
        let from_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &from_input,
            SymbolResolutionPolicy::Aggregate,
            None,
            Some(max_depth),
            &include_roots,
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };
        let to_resolution = match self.resolve_graph_symbol_with_focus_retry(
            &to_input,
            SymbolResolutionPolicy::Aggregate,
            None,
            Some(max_depth),
            &include_roots,
        ) {
            Ok(r) => r,
            Err(e) => return (e, true),
        };

        // Extract SymbolId lists from both resolutions.
        let from_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&from_resolution, from_qname) {
                Ok((ids, _)) => ids,
                Err(e) => {
                    if let Some(qname) = not_found_resolution_qname(&from_resolution) {
                        return self.retryable_symbol_not_found_response(
                            "path",
                            args,
                            qname,
                            Vec::new(),
                            Some("path requires the source symbol to be materialized first".into()),
                        );
                    }
                    return (e, true);
                }
            };

        let to_ids: Vec<SymbolId> =
            match resolution_to_symbol_ids_and_meta(&to_resolution, to_qname) {
                Ok((ids, _)) => ids,
                Err(e) => {
                    if let Some(qname) = not_found_resolution_qname(&to_resolution) {
                        let detail = self
                            .unresolved_call_target_hint(&from_ids, to_qname)
                            .unwrap_or_else(|| {
                                "path requires the target symbol to be materialized first".into()
                            });
                        return self.retryable_symbol_not_found_response(
                            "path",
                            args,
                            qname,
                            Vec::new(),
                            Some(detail),
                        );
                    }
                    if let Some(hint) = self.unresolved_call_target_hint(&from_ids, to_qname) {
                        return (format!("{e}.{hint}"), true);
                    }
                    return (e, true);
                }
            };

        // Update investigation with the first "from" symbol
        if let Some(&first_from) = from_ids.first() {
            self.update_investigation(InvestigationFocus::Symbol(first_from));
        }
        let lr = AnalysisEnvelope::new("path", args);

        // Transparent lazy structural: ensure both endpoint files have full
        // structural data before path finding. A cold focus project may lack
        // the intra-file call edges that BFS needs to discover a path.
        let intent = Some(atlas_engine::QueryIntent::Path {
            from_name: from_qname.to_string(),
            to_name: to_qname.to_string(),
            max_depth: Some(max_depth),
        });
        let (focus_result, focus_warnings) =
            self.prepare_focus_query_with_roots(intent, include_roots);
        let lazy_warnings = focus_warnings;
        // Cache for no-path diagnostics below (used in user-facing messages).
        let is_manual_full = {
            let active = self.project();
            active.query_runtime.has_full_index(&active.store)
        };

        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let snap = graph.snapshot();

        // Try all SymbolId pairs for the same qname.  In C/C++, a symbol
        // declared in a header (.h) and defined in a source file (.c)
        // produces two SymbolIds sharing the same qualified name — only the
        // definition's ID has outgoing call edges.  The first pair (from_ids[0]
        // → to_ids[0]) matches the pre-fix behaviour; fallback pairs are
        // tried only when the first attempt fails.
        let mut ranked = Vec::new();
        let mut winning_from = None;
        let mut winning_to = None;
        'id_search: for fid in &from_ids {
            for tid in &to_ids {
                if from_qname == to_qname && fid == tid {
                    continue;
                }
                // K=5: find up to 5 alternative paths, ranked by composite score.
                // Convert SymbolId → NodeIx for the snapshot.
                let from_ix = match snap.id_to_idx.get(fid) {
                    Some(ix) => *ix,
                    None => continue,
                };
                let to_ix = match snap.id_to_idx.get(tid) {
                    Some(ix) => *ix,
                    None => continue,
                };
                let candidates = snap.k_ranked_paths(
                    from_ix,
                    to_ix,
                    5,
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                    direction,
                    prefer_production,
                );
                if !candidates.is_empty() {
                    ranked = candidates;
                    winning_from = Some(*fid);
                    winning_to = Some(*tid);
                    break 'id_search;
                }
            }
        }

        /// Resolve a SymbolId to a compact "file:line" label for ambiguity
        /// reporting.
        fn symbol_label(store: &Store, id: &SymbolId) -> String {
            store
                .find_symbol_by_id(id)
                .ok()
                .flatten()
                .map(|s| {
                    format!(
                        "{}:{}",
                        store
                            .get_file(&s.file_id)
                            .ok()
                            .flatten()
                            .map(|f| f.path.clone())
                            .unwrap_or_else(|| s.file_id.to_hex()),
                        s.range.start_line + 1
                    )
                })
                .unwrap_or_else(|| id.to_hex())
        }

        /// Build JSON hops for a single path.
        fn build_hops(
            store_query: &crate::tools::runtime::store_query_runtime::StoreQueryRuntime,
            snap: &atlas_engine::GraphSnapshot,
            path: &atlas_engine::GraphPath,
            include_code: bool,
        ) -> Vec<serde_json::Value> {
            let mut hops: Vec<serde_json::Value> =
                Vec::with_capacity(path.node_indices.len() + path.edges.len());
            for i in 0..path.node_indices.len() {
                let mut node_json =
                    crate::tools::node_json(store_query, snap, path.node_indices[i], None);
                if include_code {
                    let node = snap.node(path.node_indices[i]);
                    if let Some(src) = store_query.read_symbol_source(&node.symbol_id) {
                        node_json["source"] = json!(src);
                    }
                }
                hops.push(node_json);
                if i < path.edges.len() {
                    let edge = snap.edge(path.edges[i].edge_ix);
                    hops.push(json!({
                        "edge_kind": edge.kind.as_str(),
                        "direction": path.edges[i].direction.as_str(),
                        "confidence": edge.confidence.as_f32(),
                    }));
                }
            }
            hops
        }

        if !ranked.is_empty() {
            let snap = graph.snapshot();

            // Primary path (rank 0) gets the full treatment.
            let primary = &ranked[0];
            let hops = build_hops(
                &self.project().store_query_runtime,
                snap,
                &primary.path,
                include_code,
            );
            let breakpoints: Vec<serde_json::Value> = primary.path.breakpoints.iter().map(|bp| {
                json!({ "kind": bp.kind.as_str(), "edge_index": bp.edge_index, "message": bp.message })
            }).collect();

            let mut resp = json!({
                "from": from_qname,
                "to": to_qname,
                "path_length": primary.path.node_indices.len(),
                "confidence": primary.path.confidence,
                "total_weight": primary.path.total_weight,
                "test_hops": primary.path.test_hops,
                "indirect_hops": primary.path.indirect_hops,
                "path": hops,
                "breakpoints": breakpoints,
                "score": {
                    "overall": primary.scores.overall,
                    "semantic": primary.scores.semantic,
                    "topology": primary.scores.topology,
                    "centrality": primary.scores.centrality,
                },
            });

            // Alternative paths (ranks 1+) — compact summaries.
            if ranked.len() > 1 {
                let alternatives: Vec<serde_json::Value> = ranked[1..]
                    .iter()
                    .map(|r| {
                        let alt_hops =
                            build_hops(&self.project().store_query_runtime, snap, &r.path, false);
                        json!({
                            "path": alt_hops,
                            "total_weight": r.path.total_weight,
                            "score": {
                                "overall": r.scores.overall,
                                "semantic": r.scores.semantic,
                                "topology": r.scores.topology,
                                "centrality": r.scores.centrality,
                            },
                        })
                    })
                    .collect();
                resp["alternatives"] = json!(alternatives);
            }

            // Ambiguity metadata — include resolution info
            if from_ids.len() > 1 || to_ids.len() > 1 {
                let mut ambiguity = json!({});
                if from_ids.len() > 1 {
                    if let Some(ref wid) = winning_from {
                        ambiguity["matched_from"] = json!(symbol_label(&self.project().store, wid));
                    }
                    ambiguity["from_count"] = json!(from_ids.len());
                    // Add from_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.project().store,
                                id,
                                Some(id) == winning_from.as_ref(),
                            )
                        })
                        .collect();
                    if !from_candidates.is_empty() {
                        ambiguity["from_candidates"] = json!(from_candidates);
                    }
                }
                if to_ids.len() > 1 {
                    if let Some(ref wid) = winning_to {
                        ambiguity["matched_to"] = json!(symbol_label(&self.project().store, wid));
                    }
                    ambiguity["to_count"] = json!(to_ids.len());
                    // Add to_candidates list (truncated to MAX_AMBIGUOUS_CANDIDATES)
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| {
                            candidate_json(
                                &self.project().store,
                                id,
                                Some(id) == winning_to.as_ref(),
                            )
                        })
                        .collect();
                    if !to_candidates.is_empty() {
                        ambiguity["to_candidates"] = json!(to_candidates);
                    }
                }
                // Add structured from_resolution metadata
                ambiguity["from_resolution"] = build_resolution_meta_for_path(&from_resolution);
                if to_ids.len() > 1 {
                    ambiguity["to_resolution"] = build_resolution_meta_for_path(&to_resolution);
                }
                // Add selection note
                if from_ids.len() > 1 || to_ids.len() > 1 {
                    ambiguity["selection_note"] =
                        json!("Selected first (from, to) pair with a discoverable path.");
                }
                resp["ambiguity"] = ambiguity;
            }

            // ── Path quality insight ───────────────────────────────────
            //
            // When the found path has low semantic quality (proxy/fallback
            // patterns, low centrality), the true primary path was likely
            // blocked by unresolved function pointers or dynamic dispatch.
            // Compute guidance on where annotations would help.

            let quality = if primary.scores.semantic >= 0.8 && primary.scores.overall >= 0.7 {
                "direct"
            } else if primary.scores.semantic >= 0.5 {
                "indirect"
            } else {
                "fallback"
            };

            let mut insight = json!({ "quality": quality });

            if quality == "fallback" || quality == "indirect" {
                // Find function-pointer registration sites reachable from
                // the path nodes — these are likely the reason the primary
                // path wasn't found.
                let mut fp_sites: Vec<serde_json::Value> = Vec::new();
                for &nix in &primary.path.node_indices {
                    let regs = snap.incoming_neighbors_with_kinds(&snap.node(nix).symbol_id);
                    for (reg_ix, ek) in &regs {
                        if *ek == atlas_engine::EdgeKind::RegistersCallback {
                            let reg_node = snap.node(*reg_ix);
                            fp_sites.push(json!({
                                "at": reg_node.qualified_name,
                                "registers": snap.node(nix).qualified_name,
                                "guidance": format!(
                                    "fp_dispatches(action=\"add\", field_qname='{}...', target_qname='{}')",
                                    reg_node.qualified_name, snap.node(nix).qualified_name
                                ),
                            }));
                        }
                    }
                    if fp_sites.len() >= 5 {
                        break;
                    }
                }
                if !fp_sites.is_empty() {
                    insight["fp_boundaries"] = json!(fp_sites);
                }

                // Compute forward frontier from source to show where
                // function-pointer boundaries exist.
                let frontier = snap.forward_frontier(
                    &[snap.node(primary.path.node_indices[0]).symbol_id],
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                );
                let blocked: Vec<serde_json::Value> = frontier
                    .frontier_nodes
                    .iter()
                    .take(5)
                    .filter(|n| n.outgoing_call_count == 0)
                    .map(|n| json!({ "qname": n.qname, "depth": n.depth }))
                    .collect();
                if !blocked.is_empty() {
                    insight["blocked_at"] = json!({
                        "message": format!(
                            "{} node(s) with no static forward edges — likely function-pointer or dynamic-dispatch boundaries",
                            blocked.len()
                        ),
                        "nodes": blocked,
                    });
                }

                insight["action"] = json!(
                    "The primary path is likely blocked by unresolved function pointers. Use 'fp_dispatches' (action='add') to declare known dispatches (e.g., curl handler tables, vtable assignments), then re-run the path query after annotation materialization."
                );
            }

            resp["path_quality"] = insight;

            let lr = lr
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(lazy_warnings);
            let lr = if let Some(ref result) = focus_result {
                crate::tools::apply_focus_result_to_lr(lr, result)
            } else {
                lr
            };
            lr.build(resp, self)
        } else {
            // No path found — diagnostic frontier.
            let total_pairs = from_ids.len() * to_ids.len();
            let mut no_path_warnings = lazy_warnings;
            let mut message = format!(
                "No path found within max_depth={} (tried {} SymbolId pair{})",
                max_depth.min(10),
                total_pairs,
                if total_pairs == 1 { "" } else { "s" },
            );
            // ... (same diagnostics as before) ...
            if total_pairs > 1 {
                if from_ids.len() > 10 || to_ids.len() > 10 {
                    message.push_str(&format!(
                        ". Note: '{}' matched {} SymbolId(s), '{}' matched {} SymbolId(s) — this is likely symbol-name ambiguity across files. Use a fully-qualified name to narrow the search.",
                        from_qname, from_ids.len(), to_qname, to_ids.len(),
                    ));
                } else {
                    message.push_str(". Note: the same qualified name maps to multiple SymbolIds (e.g., declaration vs definition). All pairs were tried.");
                }
            }
            if !is_manual_full && max_depth < 10 {
                message.push_str(". In focus mode this is only a current-closure result, not a repo-wide proof. Tip: try a higher max_depth (up to 10), resume the query after refinement, or run a full structural index (CLI: 'atlas index --analysis full') for deeper call-graph edges.");
            } else if !is_manual_full {
                message.push_str(". In focus mode this is only a current-closure result, not a repo-wide proof. Tip: the path may involve function pointers or dynamic dispatch not yet resolved; resume the query after refinement or run a full structural index (CLI: 'atlas index --analysis full').");
            } else {
                message.push_str(". The symbols may not be connected by call edges, or the path exceeds the depth limit. Try a higher max_depth.");
            }
            if !is_manual_full {
                no_path_warnings.push(
                    "No path was found in the current focus closure; this does not prove that no repo-wide path exists until full indexing or further refinement completes."
                        .to_string(),
                );
            }

            // Resolve endpoint symbol kinds for type-aware diagnostics.
            // Uses the first SymbolId per qname (most common case).
            let from_kind = from_ids
                .first()
                .and_then(|id| snap.node_by_id(id))
                .map(|n| n.kind);
            let to_kind = to_ids
                .first()
                .and_then(|id| snap.node_by_id(id))
                .map(|n| n.kind);
            if let (Some(fk), Some(tk)) = (from_kind, to_kind) {
                message.push_str(&format!(
                    " (from '{from_qname}' resolved as {fk:?}, to '{to_qname}' resolved as {tk:?})",
                ));
                if !is_callable_kind(tk) {
                    message.push_str(". Note: target is not a callable — specify a method or function instead (e.g. use the fully-qualified method name).");
                }
                if !is_callable_kind(fk) {
                    message.push_str(". Note: source is not a callable — outgoing call edges originate from functions/methods, not from type definitions.");
                }
            }

            const MAX_FRONTIER_NODES: usize = 20;
            let frontier_nodes: Vec<serde_json::Value> = if direction
                == TraversalDirection::Outgoing
            {
                let frontier = snap.forward_frontier(
                    &from_ids,
                    max_depth.min(10),
                    edge_kind_filter.as_deref(),
                );
                let total = frontier.frontier_nodes.len();
                if total > 0 {
                    let extra = if total > MAX_FRONTIER_NODES {
                        " These are likely dynamic-dispatch (function pointer / virtual call) boundaries."
                    } else {
                        ""
                    };
                    message.push_str(&format!(
                            "\nForward frontier reached depth {} — {} node(s) with no further static callees (showing first {}).{}",
                            frontier.depth_reached, total, total.min(MAX_FRONTIER_NODES), extra,
                        ));
                }
                frontier.frontier_nodes.iter().take(MAX_FRONTIER_NODES).map(|n| {
                        json!({ "qname": n.qname, "depth": n.depth, "outgoing_call_count": n.outgoing_call_count })
                    }).collect()
            } else {
                Vec::new()
            };
            // Build base response before envelope injection.
            let mut resp = json!({
                "from": from_qname, "to": to_qname,
                "path_length": 0, "path": [], "breakpoints": [],
                "message": &message, "frontier": frontier_nodes,
            });

            // Add candidates and disambiguation guidance when symbols are ambiguous.
            if from_ids.len() > 1 || to_ids.len() > 1 {
                if from_ids.len() > 1 {
                    let from_candidates: Vec<serde_json::Value> = from_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.project().store, id, false))
                        .collect();
                    if !from_candidates.is_empty() {
                        resp["from_candidates"] = json!(from_candidates);
                    }
                }
                if to_ids.len() > 1 {
                    let to_candidates: Vec<serde_json::Value> = to_ids
                        .iter()
                        .take(MAX_AMBIGUOUS_CANDIDATES)
                        .map(|id| candidate_json(&self.project().store, id, false))
                        .collect();
                    if !to_candidates.is_empty() {
                        resp["to_candidates"] = json!(to_candidates);
                    }
                }
                resp["message"] = json!(format!(
                    "{message}\nUse a SymbolSelector object (for example, {{\"qualified_name\": \"...\", \"file_path\": \"...\"}}) to disambiguate; symbol_ref from search or symbol results can be reused directly."
                ));
            }

            let lr = lr
                .with_root_warnings(root_warnings)
                .with_lazy_warnings(no_path_warnings);
            let lr = if let Some(ref result) = focus_result {
                crate::tools::apply_focus_result_to_lr(lr, result)
            } else {
                lr
            };
            lr.build(resp, self)
        }
    }

    /// Resolve the `direction` parameter for path finding.
    /// - Not provided or "outgoing" → TraversalDirection::Outgoing (only forward edges)
    /// - "incoming" → TraversalDirection::Incoming (only reverse/caller edges)
    /// - "both" → TraversalDirection::Both (forward + reverse; use when tracing
    ///   who-calls-X-to-reach-Y scenarios or backward provenance)
    fn resolve_path_direction(args: &serde_json::Value) -> TraversalDirection {
        match get_str_opt(args, "direction") {
            Some("outgoing") => TraversalDirection::Outgoing,
            Some("incoming") => TraversalDirection::Incoming,
            Some("both") => TraversalDirection::Both,
            _ => TraversalDirection::Outgoing,
        }
    }

    /// Resolve the `edge_kinds` parameter to an optional edge kind filter.
    /// - Not provided → defaults to call edges only (DEFAULT_PATH_EDGES)
    /// - Empty array or `["*"]` → follows all edge kinds (None)
    /// - Specific kinds → filtered to those kinds
    fn resolve_path_edge_kinds(args: &serde_json::Value) -> Result<Option<Vec<EdgeKind>>, String> {
        let raw = match args.get("edge_kinds") {
            None | Some(serde_json::Value::Null) => {
                return Ok(Some(DEFAULT_PATH_EDGES.to_vec()));
            }
            Some(v) => v,
        };
        let arr = raw
            .as_array()
            .ok_or_else(|| "edge_kinds must be an array of strings".to_string())?;
        if arr.is_empty() {
            return Ok(None); // all edge kinds
        }
        if arr.len() == 1 && arr[0].as_str() == Some("*") {
            return Ok(None); // wildcard → all edge kinds
        }
        let mut kinds = Vec::with_capacity(arr.len());
        for v in arr {
            let s = v.as_str().unwrap_or("");
            if s == "*" {
                return Err("'*' must be the only value in edge_kinds".to_string());
            }
            kinds.push(parse_edge_kind(s)?);
        }
        Ok(Some(kinds))
    }
}
