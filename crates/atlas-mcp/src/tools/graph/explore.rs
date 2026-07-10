//! Explore handler: dossier assembly for a single symbol with scoped
//! resolution fallback, source-mode control, and relation/evidence limiting.

use super::*;

/// Parse the `source_mode` parameter from explore tool arguments.
fn parse_source_mode(args: &serde_json::Value) -> atlas_engine::dossier::types::SourceMode {
    match args.get("source_mode").and_then(|v| v.as_str()) {
        Some("full") => atlas_engine::dossier::types::SourceMode::Full,
        Some("none") => atlas_engine::dossier::types::SourceMode::None_,
        _ => atlas_engine::dossier::types::SourceMode::Excerpt,
    }
}

impl ToolRouter {
    fn scoped_explore_resolution(
        &self,
        qname: &str,
        scope: &str,
        include_roots: Vec<String>,
    ) -> Result<Option<SymbolResolution>, String> {
        let structural = self.project().materialize.structural().clone();
        let svc = ScopedSearchService::new_with_project_root(
            self.project().store.clone(),
            structural,
            Some(self.project().root.clone()),
        );
        let resp = svc
            .execute(ScopedSearchRequest {
                query: qname.to_string(),
                scope: Some(scope.to_string()),
                analysis: SearchAnalysis::Auto,
                limit: MAX_AMBIGUOUS_CANDIDATES,
                include_roots,
                ..Default::default()
            })
            .map_err(|e| format!("Scoped explore search failed: {e}"))?;

        if resp.results.is_empty() {
            return Ok(None);
        }

        let mut exact: Vec<SymbolDef> = resp
            .results
            .iter()
            .filter(|hit| hit.symbol.name == qname || hit.symbol.qualified_name == qname)
            .map(|hit| hit.symbol.clone())
            .collect();
        if exact.is_empty() {
            exact = resp.results.iter().map(|hit| hit.symbol.clone()).collect();
        }

        if exact.len() == 1 {
            let sym = exact.remove(0);
            let file_path = self
                .project()
                .store_query_runtime
                .resolve_file_path(&sym.file_id);
            let line = sym.range.start_line.saturating_add(1);
            return Ok(Some(SymbolResolution::Single {
                symbol_id: sym.id,
                resolved: crate::tools::symbol_selector::ResolvedSymbol {
                    qualified_name: sym.qualified_name,
                    file_path,
                    line,
                    kind: sym.kind.as_str().to_string(),
                    language: sym.language.as_str().to_string(),
                    match_info: MatchInfo {
                        mode: MatchMode::UniqueQname,
                        ignored_mismatches: Vec::new(),
                        path_match: None,
                        line_delta: None,
                    },
                },
            }));
        }

        let candidates = exact
            .iter()
            .take(MAX_AMBIGUOUS_CANDIDATES)
            .map(|sym| {
                let file_path = self
                    .project()
                    .store_query_runtime
                    .resolve_file_path(&sym.file_id);
                let line = sym.range.start_line.saturating_add(1);
                ScoredCandidate {
                    qualified_name: sym.qualified_name.clone(),
                    file_path: file_path.clone(),
                    line,
                    kind: sym.kind.as_str().to_string(),
                    language: sym.language.as_str().to_string(),
                    score: 9_000,
                    reasons: vec![format!("scope:{scope}")],
                    symbol_ref: SymbolSelector {
                        qualified_name: sym.qualified_name.clone(),
                        file_path: Some(file_path),
                        line: Some(line),
                        kind: Some(sym.kind.as_str().to_string()),
                        language: Some(sym.language.as_str().to_string()),
                    },
                    symbol_id: sym.id,
                }
            })
            .collect();
        Ok(Some(SymbolResolution::Ambiguous {
            candidates,
            score_gap: 0,
        }))
    }

    pub(crate) fn handle_explore(&self, args: &serde_json::Value) -> (String, bool) {
        let input = match parse_symbol_arg(args) {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = symbol_input_qname(&input);
        if let Err(e) = crate::tools::validate_symbol_name_length(qname) {
            return (e, true);
        }
        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        let include_roots_for_scope: Vec<String> =
            include_roots.iter().map(|root| root.path.clone()).collect();
        let source_mode = parse_source_mode(args);
        let source_lines = args
            .get("source_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(40) as u32;
        let evidence_limit = args
            .get("evidence_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let relation_limit = args
            .get("relation_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(12) as usize;
        let peer_limit = args
            .get("peer_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(12) as usize;
        let max_source_bytes: usize = 65536;
        let include_file_context = args
            .get("include_file_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let include_recommendations = args
            .get("include_recommendations")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let scope = get_str_opt(args, "scope")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let has_file_hint = matches!(
            &input,
            SymbolInput::Selector(sel)
                if sel.file_path.as_deref().is_some_and(|p| !p.trim().is_empty())
        );
        let prepared_focus = if scope.is_none() && has_file_hint {
            Some(self.prepare_focus_query_with_roots(
                Some(atlas_engine::QueryIntent::Explore {
                    symbol_name: qname.to_string(),
                    file_id: self.resolve_selector_file_id(&input),
                    symbol_id: None,
                }),
                include_roots.clone(),
            ))
        } else {
            None
        };

        // Resolve with UniqueOrCandidates policy.
        let resolution = if let Some(scope) = scope {
            match self.scoped_explore_resolution(qname, scope, include_roots_for_scope) {
                Ok(Some(r)) => r,
                Ok(None) => SymbolResolution::NotFound {
                    qname: qname.to_string(),
                    suggestions: Vec::new(),
                },
                Err(e) => return (e, true),
            }
        } else {
            match self.resolve_symbol_input(&input, SymbolResolutionPolicy::UniqueOrCandidates) {
                Ok(r) => r,
                Err(e) => return (e, true),
            }
        };

        let lr = AnalysisEnvelope::new("explore", args).with_root_warnings(root_warnings);

        let (sym_id, mut resolved_opt) = match resolution {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => (symbol_id, Some(resolved)),
            SymbolResolution::Ambiguous {
                candidates,
                score_gap,
            } => {
                let candidate_list: Vec<serde_json::Value> = candidates
                    .iter()
                    .map(|c| {
                        json!({
                            "qualified_name": c.qualified_name,
                            "file_path": c.file_path,
                            "line": c.line,
                            "kind": c.kind,
                            "language": c.language,
                            "score": c.score,
                            "reasons": c.reasons,
                            "symbol_ref": c.symbol_ref,
                        })
                    })
                    .collect();
                let resp = json!({
                    "symbol": qname,
                    "ambiguous": true,
                    "score_gap": score_gap,
                    "candidates": candidate_list,
                });
                return lr.with_is_error(false).build(resp, self);
            }
            SymbolResolution::NotFound {
                ref qname,
                ref suggestions,
            } => {
                let mut resp = json!({
                    "symbol": qname,
                    "status": "unresolved",
                    "message": "The symbol is not available in the current local focus closure yet. Background scoped analysis has been started; retry this explore request after the suggested delay, or pass a SymbolSelector with file_path/scope to constrain the local region.",
                });
                if !suggestions.is_empty() {
                    resp["suggestions"] = json!(suggestions);
                }
                if let Some(scope) = scope {
                    resp["scope"] = json!(scope);
                }
                let (background_jobs, candidate_files) = if let Some(scope) = scope {
                    let file_ids = self
                        .project()
                        .store
                        .list_file_inventory_ids_in_scope(scope, 24)
                        .unwrap_or_default();
                    (self.enqueue_background_file_focus(&file_ids), Vec::new())
                } else {
                    let file_ids = self.candidate_file_ids_for_symbol(qname);
                    let files = self.candidate_file_paths(&file_ids);
                    (self.enqueue_background_file_focus(&file_ids), files)
                };
                if !candidate_files.is_empty() {
                    resp["candidate_files"] = json!(candidate_files);
                };
                let summary = if background_jobs.is_empty() {
                    "explore returned a bounded unresolved result; retry after focus bootstrap has warmed more inventory"
                } else {
                    "explore returned a bounded unresolved result; background scoped analysis is preparing local symbol facts"
                };
                return lr
                    .with_is_error(false)
                    .with_analysis_scope("local".into())
                    .with_analysis_summary(summary.into())
                    .with_analysis_basis(vec!["manifest".into(), "structural".into()])
                    .with_analysis_retry_after_ms(2000)
                    .build(resp, self);
            }
        };

        let seed_sym = match self.project().store.find_symbol_by_id(&sym_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                let mut err = format!("Symbol not found in store: {qname}");
                err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
            Err(e) => {
                let mut err = format!("Lookup error: {e}");
                err.push_str(self.project().store_query_runtime.not_indexed_guidance());
                return (err, true);
            }
        };

        self.update_investigation(InvestigationFocus::Symbol(seed_sym.id));

        // Lazy structural: prepare focus query for graph edges
        let (focus_result, focus_warnings) = prepared_focus.unwrap_or_else(|| {
            self.prepare_focus_query_with_roots(
                Some(atlas_engine::QueryIntent::Explore {
                    symbol_name: qname.to_string(),
                    file_id: Some(seed_sym.file_id),
                    symbol_id: None,
                }),
                include_roots,
            )
        });

        // Focus may replace manifest or stale structural facts for the same
        // deterministic SymbolId. Never build the dossier from the pre-focus
        // copy: its range can be only the declaration name even though the DB
        // now contains the complete definition.
        let sym = match self.project().store.find_symbol_by_id(&sym_id) {
            Ok(Some(symbol)) => symbol,
            Ok(None) => {
                return (
                    format!("Symbol disappeared after focus preparation: {qname}"),
                    true,
                );
            }
            Err(error) => {
                return (
                    format!("Lookup after focus preparation failed: {error}"),
                    true,
                );
            }
        };
        if let Ok(SymbolResolution::Single { resolved, .. }) =
            self.resolve_symbol_input(&input, SymbolResolutionPolicy::UniqueOrCandidates)
        {
            resolved_opt = Some(resolved);
        }
        let file_path = self
            .project()
            .store_query_runtime
            .resolve_file_path(&sym.file_id);

        let (store_clone, root_clone) = {
            let active = self.project();
            (active.store.clone(), active.root.clone())
        };
        let sym_repo = atlas_engine::dossier::SymbolRepo::new(store_clone.clone());
        let source_repo = atlas_engine::dossier::SourceRepo::new(store_clone.clone(), root_clone);
        let project = self.project();
        let graph = match project.graph_runtime.provider().graph_snapshot() {
            Some(g) => g,
            None => return ("Graph not initialized".to_string(), true),
        };
        let relation_repo = atlas_engine::dossier::RelationRepo::new(store_clone.clone(), graph);
        let file_repo = atlas_engine::dossier::FileFactsRepo::new(store_clone);

        let request = atlas_engine::dossier::types::ExploreRequest {
            symbol: qname.to_string(),
            source_mode,
            source_lines,
            evidence_limit,
            relation_limit,
            peer_limit,
            max_source_bytes,
            include_file_context,
            include_recommendations,
        };

        let tier_str = "unknown".to_string();

        let mut dossier = match atlas_engine::dossier::builder::ExploreDossierBuilder::build(
            &sym,
            &file_path,
            &sym_repo,
            &relation_repo,
            &file_repo,
            &source_repo,
            &request,
            tier_str,
        ) {
            Ok(d) => d,
            Err(e) => return (format!("Failed to build dossier: {e}"), true),
        };

        source_repo.clear_cache();

        // Merge focus warnings into dossier warnings
        dossier.warnings.extend(focus_warnings);

        let mut resp_value =
            serde_json::to_value(&dossier).unwrap_or_else(|e| json!({"error": e.to_string()}));
        if let Some(response) = resp_value.as_object_mut() {
            response.remove("precisionTier");
        }

        // Include resolution metadata when resolved
        if let Some(ref resolved) = resolved_opt {
            resp_value["resolution"] = json!({
                "policy": "unique_or_candidates",
                "resolved": resolved,
            });
        }

        let lr = lr.with_lazy_warnings(dossier.warnings);
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };
        lr.build_with_args(resp_value, args, self)
    }
}
