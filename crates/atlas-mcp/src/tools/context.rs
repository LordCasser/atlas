//! Context tool: builds rich markdown context for a symbol.
//!
//! Includes transparent lazy structural extraction when the symbol is not yet
//! indexed. After lazy extraction writes new facts to the DB, the in-memory
//! graph snapshot is force-refreshed so that the context builder sees the
//! newly parsed edges — closing the MCP call-flow gap where graph init
//! happened before the handler's own structural extraction.

use super::lazy_response::LazyResponse;
use super::{MAX_SYMBOL_NAME_LENGTH, ToolRouter};
use super::symbol_selector::{
    parse_symbol_input, ResolvedSymbol, ScoredCandidate, SymbolInput, SymbolResolution,
    SymbolResolutionPolicy, SymbolSelector, MAX_AGGREGATION_CANDIDATES,
};

use atlas_engine::InvestigationFocus;
use serde_json::json;

/// Result of [`ToolRouter::resolve_context_symbol`].
enum ContextResolution {
    /// Symbol found — proceed with context building.
    Found(
        atlas_engine::SymbolId,
        Vec<String>,
        /// Resolution metadata from the engine (present for Tier 1 resolution).
        Option<ResolvedSymbol>,
    ),
    /// Multiple candidates — caller should return structured JSON.
    Ambiguous(Vec<ScoredCandidate>),
    /// Not found — caller should return an error.
    NotFound(String),
}

impl ToolRouter {
    pub(crate) fn handle_context(
        &mut self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
        // Parse symbol as unified SymbolInput (string or structured selector)
        let input: SymbolInput = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let qname = match &input {
            SymbolInput::Name(name) => name.as_str(),
            SymbolInput::Selector(sel) => sel.qualified_name.as_str(),
        };
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!(
                    "Symbol name exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"
                ),
                true,
            );
        }
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_file_peers = args
            .get("includeFilePeers")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        ctx.send_progress(0.2, &format!("Building context for '{qname}'..."));

        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }

        let lr = LazyResponse::new("symbol", args);
        let query_id = lr.query_id().to_string();

        // Try to find symbol by qname before resolution for initial investigation
        let initial_sid = self
            .active_mut().store
            .find_symbols_by_qname(qname)
            .ok()
            .and_then(|v| if v.len() == 1 { Some(v[0].id) } else { None });
        if let Some(sid) = initial_sid {
            self.update_investigation(InvestigationFocus::Symbol(sid));
        }
        let investigation = self.active_mut().job_runtime.investigation_state.active_investigation.clone();

        let (resolution, focus_result) = match self.resolve_context_symbol(
            ctx,
            &input,
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        ) {
            Ok(r) => r,
            Err(err) => return (err, true),
        };

        // Apply focus-aware envelope fields if focus extraction occurred
        let lr = if let Some(ref result) = focus_result {
            crate::tools::apply_focus_result_to_lr(lr, result)
        } else {
            lr
        };

        match resolution {
            ContextResolution::Found(sid, lazy_warnings, resolved) => {
                // Update investigation with the actually resolved symbol
                self.update_investigation(InvestigationFocus::Symbol(sid));

                // Force-refresh the graph to pick up edges written by lazy structural
                if let Err(e) = self.force_refresh_graph() {
                    return (format!("Graph refresh error: {e:#}"), true);
                }

                ctx.send_progress(0.7, "Building context view...");
                let cb = match self.active_mut().graph_runtime.provider().context_builder() {
                    Some(cb) => cb,
                    None => return ("Graph not initialized".to_string(), true),
                };
                match cb.build_context_for_symbol(&sid, include_file_peers) {
                    Ok(view) => {
                        ctx.send_progress(0.8, "Context complete");
                        self.build_context_response(
                            &view, qname, include_code, &sid,
                            root_warnings, lazy_warnings,
                            resolved, lr, args,
                        )
                    }
                    Err(e) => (format!("Context build error: {e}"), true),
                }
            }
            ContextResolution::Ambiguous(candidates) => {
                let count = candidates.len();
                let result = json!({
                    "ambiguous": true,
                    "count": count,
                    "candidates": candidates,
                    "next_query": "Pick a candidate and use its symbol_ref as symbol parameter.",
                });
                lr.with_root_warnings(root_warnings)
                    .with_is_error(false)
                    .build_with_args(result, args, self)
            }
            ContextResolution::NotFound(msg) => (msg, true),
        }
    }

    /// Build the context JSON response for a resolved symbol.
    fn build_context_response(
        &mut self,
        view: &atlas_engine::ContextView,
        qname: &str,
        include_code: bool,
        sid: &atlas_engine::SymbolId,
        root_warnings: Vec<String>,
        lazy_warnings: Vec<String>,
        resolved: Option<ResolvedSymbol>,
        lr: LazyResponse,
        args: &serde_json::Value,
    ) -> (String, bool) {
        // ── resolved ───────────────────────────────────────────────────
        let resolved_json = resolved.as_ref().and_then(|r| serde_json::to_value(r).ok());

        // ── subject ────────────────────────────────────────────────────
        let subject = serde_json::to_value(&view.subject).unwrap_or(json!(null));

        // ── subject_source ─────────────────────────────────────────────
        let subject_source = if include_code {
            if let Some(src) = self.active_mut().store_query_runtime.read_symbol_source(sid) {
                let lines: Vec<String> = src.lines().map(|l| l.to_string()).collect();
                let total = lines.len() as u32;
                Some(json!({
                    "lines": lines,
                    "start_line": view.subject_source.as_ref().map(|s| s.start_line).unwrap_or(0),
                    "total_lines": total,
                    "truncated": false,
                }))
            } else {
                view.subject_source.as_ref().map(|s| {
                    json!({
                        "lines": s.lines,
                        "start_line": s.start_line,
                        "total_lines": s.total_lines,
                        "truncated": s.truncated,
                    })
                })
            }
        } else {
            view.subject_source.as_ref().map(|s| {
                json!({
                    "lines": s.lines,
                    "start_line": s.start_line,
                    "total_lines": s.total_lines,
                    "truncated": s.truncated,
                })
            })
        };

        // ── caller_details ─────────────────────────────────────────────
        let caller_details: Vec<serde_json::Value> = view
            .caller_details
            .iter()
            .map(|c| {
                json!({
                    "symbol": serde_json::to_value(&c.symbol).unwrap_or(json!(null)),
                    "callsite_line": c.callsite_line,
                    "callsite_snippet": c.callsite_snippet,
                    "edge_kind": c.edge_kind.as_str(),
                })
            })
            .collect();

        // ── callee_details ─────────────────────────────────────────────
        let callee_details: Vec<serde_json::Value> = view
            .callee_details
            .iter()
            .map(|c| {
                json!({
                    "symbol": serde_json::to_value(&c.symbol).unwrap_or(json!(null)),
                    "callsite_line": c.callsite_line,
                    "callsite_snippet": c.callsite_snippet,
                    "edge_kind": c.edge_kind.as_str(),
                    "callee_signature": c.callee_signature,
                })
            })
            .collect();

        // ── file_peers ─────────────────────────────────────────────────
        let file_peers: Vec<serde_json::Value> = view
            .file_peers
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or(json!(null)))
            .collect();

        // ── trail ──────────────────────────────────────────────────────
        let mut trail = json!({
            "full_source": format!(
                "explore with includeCode=true, symbol: \"{}\"",
                view.subject.qualified_name
            ),
        });
        if !view.callee_details.is_empty() {
            trail["calls"] = json!(format!(
                "symbol with view=context, qname: \"{}\"",
                view.callee_details[0].symbol.qualified_name
            ));
        }
        if !view.caller_details.is_empty() {
            trail["called_by"] = json!(format!(
                "trace with kind=callers, symbol: \"{}\"",
                view.subject.name
            ));
        }

        // ── assemble result ────────────────────────────────────────────
        let mut result = json!({
            "symbol": qname,
            "view": "context",
            "subject": subject,
            "subject_file_path": view.subject_file_path,
            "caller_details": caller_details,
            "callee_details": callee_details,
            "file_peers": file_peers,
            "importers": view.importers,
            "dependencies": view.dependencies,
            "trail": trail,
        });
        if let Some(ss) = subject_source {
            result["subject_source"] = ss;
        }
        if let Some(rj) = resolved_json {
            result["resolved"] = rj;
        }

        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("context".into()));
        }

        lr.with_root_warnings(root_warnings)
            .with_lazy_warnings(lazy_warnings)
            .with_is_error(false)
            .build_with_args(result, &stored_args, self)
    }

    /// Resolve a symbol for context display with multi-tier fallback.
    ///
    /// Tier 1: unified symbol resolution via engine-layer SymbolSelector
    /// Tier 2: name-based match — finds symbols whose simple name matches,
    ///         then picks the highest-scored unambiguous match
    /// Tier 3: lazy structural extraction + re-query
    /// Tier 4: name match with multiple candidates → return candidates
    ///
    /// Returns `(ContextResolution, Option<FocusResult>)` — the focus result
    /// from any lazy structural extraction that occurred during resolution.
    fn resolve_context_symbol(
        &mut self,
        ctx: &super::ToolCallContext,
        input: &SymbolInput,
        _include_roots: Vec<atlas_engine::IncludeRoot>,
        _investigation: Option<&atlas_engine::Investigation>,
        _query_id: Option<&str>,
    ) -> Result<(ContextResolution, Option<atlas_engine::focus::runtime::FocusResult>), String> {

        let mut warnings = Vec::new();
        let mut focus_result_acc: Option<atlas_engine::focus::runtime::FocusResult> = None;

        // Extract qname for diagnostics and tier-2 name search
        let qname = match input {
            SymbolInput::Name(name) => name.as_str(),
            SymbolInput::Selector(sel) => sel.qualified_name.as_str(),
        };

        // ── Tier 1: unified symbol resolution via engine SymbolSelector ──
        match self.resolve_symbol_input(input, SymbolResolutionPolicy::UniqueOrCandidates)? {
            SymbolResolution::Single {
                symbol_id,
                resolved,
            } => {
                // Look up symbol info for file_id
                if let Ok(Some(sym)) = self.active_mut().store.find_symbol_by_id(&symbol_id) {
                    let (focus_result, focus_warnings) = self.prepare_focus_query(
                        Some(atlas_engine::QueryIntent::Context {
                            symbol_name: qname.to_string(),
                            file_id: Some(sym.file_id),
                            symbol_id: None,
                        }),
                    );
                    warnings.extend(focus_warnings);
                    if focus_result_acc.is_none() {
                        focus_result_acc = focus_result;
                    }
                    return Ok((ContextResolution::Found(
                        symbol_id,
                        warnings,
                        Some(resolved),
                    ), focus_result_acc));
                }
                // Symbol ID not in store — fall through
            }
            SymbolResolution::Ambiguous { candidates, .. } => {
                return Ok((ContextResolution::Ambiguous(candidates), focus_result_acc));
            }
            SymbolResolution::NotFound { .. } => {
                // Fall through to Tier 2 name-based search
            }
        }

        // ── Tier 2: name-based search (look for symbol by simple name) ──
        let name_matches = self.active_mut().store.find_symbols_by_name(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on find_symbols_by_name: {}", e);
            Default::default()
        });
        if name_matches.len() == 1 {
            // Unambiguous — use it directly
            let (focus_result, focus_warnings) = self.prepare_focus_query(
                Some(atlas_engine::QueryIntent::Context {
                    symbol_name: qname.to_string(),
                    file_id: Some(name_matches[0].file_id),
                    symbol_id: None,
                }),
            );
            warnings.extend(focus_warnings);
            if focus_result_acc.is_none() {
                focus_result_acc = focus_result;
            }
            return Ok((ContextResolution::Found(
                name_matches[0].id,
                warnings,
                None,
            ), focus_result_acc));
        }
        if name_matches.len() > 1 {
            // Multiple matches — try case-insensitive qualified-name substring
            let q_lower = qname.to_lowercase();
            let matching_qnames: Vec<_> = name_matches
                .iter()
                .filter(|s| s.qualified_name.to_lowercase().contains(&q_lower))
                .collect();
            if matching_qnames.len() == 1 {
                let (focus_result, focus_warnings) = self.prepare_focus_query(
                    Some(atlas_engine::QueryIntent::Context {
                        symbol_name: qname.to_string(),
                        file_id: Some(matching_qnames[0].file_id),
                        symbol_id: None,
                    }),
                );
                warnings.extend(focus_warnings);
                if focus_result_acc.is_none() {
                    focus_result_acc = focus_result;
                }
                return Ok((ContextResolution::Found(
                    matching_qnames[0].id,
                    warnings,
                    None,
                ), focus_result_acc));
            }
            if matching_qnames.len() > 1 {
                let candidates: Vec<ScoredCandidate> = matching_qnames
                    .iter()
                    .take(MAX_AGGREGATION_CANDIDATES)
                    .map(|s| {
                        let line = s.range.start_line.saturating_add(1);
                        let file_path = self.active_mut().store_query_runtime.resolve_file_path(&s.file_id);
                        ScoredCandidate {
                            qualified_name: s.qualified_name.clone(),
                            file_path: file_path.clone(),
                            line,
                            kind: s.kind.as_str().to_string(),
                            language: s.language.as_str().to_string(),
                            score: 0,
                            reasons: vec!["name_match".into()],
                            symbol_ref: SymbolSelector {
                                qualified_name: s.qualified_name.clone(),
                                file_path: Some(file_path),
                                line: Some(line),
                                kind: Some(s.kind.as_str().to_string()),
                                language: Some(s.language.as_str().to_string()),
                            },
                            symbol_id: s.id,
                        }
                    })
                    .collect();
                return Ok((ContextResolution::Ambiguous(candidates), focus_result_acc));
            }
        }

        // ── Tier 3: try lazy structural, then re-query ──
        ctx.send_progress(0.5, "Extracting structural data...");
        let (focus_result, focus_warnings) = self.prepare_focus_query(
            Some(atlas_engine::QueryIntent::Context {
                symbol_name: qname.to_string(),
                file_id: None,
                symbol_id: None,
            }),
        );
        warnings.extend(focus_warnings);
        if focus_result_acc.is_none() {
            focus_result_acc = focus_result;
        }

        // Re-query after lazy extraction using engine SymbolSelector
        let re_input = SymbolInput::Name(qname.to_string());
        match self.resolve_symbol_input(&re_input, SymbolResolutionPolicy::UniqueOrCandidates)? {
            SymbolResolution::Single { symbol_id, .. } => {
                return Ok((ContextResolution::Found(
                    symbol_id,
                    warnings,
                    None,
                ), focus_result_acc));
            }
            SymbolResolution::Ambiguous { candidates, .. } => {
                return Ok((ContextResolution::Ambiguous(candidates), focus_result_acc));
            }
            SymbolResolution::NotFound { .. } => {
                // Still not found — fall through to Tier 4
            }
        }

        // Re-check name after lazy extraction
        let fresh_matches = self.active_mut().store.find_symbols_by_name(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on retry find_symbols_by_name: {}", e);
            Default::default()
        });
        if fresh_matches.len() == 1 {
            return Ok((ContextResolution::Found(
                fresh_matches[0].id,
                warnings,
                None,
            ), focus_result_acc));
        }
        if fresh_matches.len() > 1 {
            let candidates: Vec<ScoredCandidate> = fresh_matches
                .iter()
                .take(MAX_AGGREGATION_CANDIDATES)
                .map(|s| {
                    let line = s.range.start_line.saturating_add(1);
                    let file_path = self.active_mut().store_query_runtime.resolve_file_path(&s.file_id);
                    ScoredCandidate {
                        qualified_name: s.qualified_name.clone(),
                        file_path: file_path.clone(),
                        line,
                        kind: s.kind.as_str().to_string(),
                        language: s.language.as_str().to_string(),
                        score: 0,
                        reasons: vec!["name_match".into()],
                        symbol_ref: SymbolSelector {
                            qualified_name: s.qualified_name.clone(),
                            file_path: Some(file_path),
                            line: Some(line),
                            kind: Some(s.kind.as_str().to_string()),
                            language: Some(s.language.as_str().to_string()),
                        },
                        symbol_id: s.id,
                    }
                })
                .collect();
            return Ok((ContextResolution::Ambiguous(candidates), focus_result_acc));
        }

        // ── Tier 4: nothing found ──
        let mut err = format!(
            "Symbol '{qname}' not found by qualified name or simple name. Try 'search' first to discover the correct qualified_name for this symbol."
        );
        err.push_str(self.active_mut().store_query_runtime.not_indexed_guidance());
        Ok((ContextResolution::NotFound(err), focus_result_acc))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use atlas_engine::Store;

    // ── Helpers ────────────────────────────────────────────────────────

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn register_test_file(store: &Store, path: &str) -> atlas_engine::FileId {
        let file_id = atlas_engine::FileId::generate(path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path: path.into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: "hash1".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        file_id
    }

    fn insert_test_symbol(
        store: &Store,
        file_id: atlas_engine::FileId,
        simple_name: &str,
        qualified_name: &str,
        kind: atlas_engine::SymbolKind,
        line: u32,
    ) {
        let line0 = line.saturating_sub(1);
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: line0,
            start_column: 1,
            end_line: line0,
            end_column: 11,
        };
        let sym = atlas_engine::SymbolDef {
            id: atlas_engine::SymbolId::generate(
                &file_id,
                "typescript",
                simple_name,
                kind.as_str(),
                None,
            ),
            kind,
            name: simple_name.into(),
            qualified_name: qualified_name.into(),
            symbol_path: vec![simple_name.into()],
            file_id,
            language: atlas_engine::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym]).unwrap();
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[test]
    fn context_string_qname_multiple_matches_returns_ambiguous() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        insert_test_symbol(
            &store,
            file_a,
            "turn",
            "turn",
            atlas_engine::SymbolKind::Function,
            10,
        );
        insert_test_symbol(
            &store,
            file_b,
            "turn",
            "turn",
            atlas_engine::SymbolKind::Variable,
            20,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = super::super::ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "turn"});
        let (resp_str, is_error) = router.handle_context(&ctx, &args);

        assert!(!is_error, "expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("expected valid JSON");

        // Check envelope fields
        assert_eq!(resp["ambiguous"], serde_json::json!(true));
        let count = resp["count"].as_u64().expect("count should be a number");
        assert_eq!(count, 2);
        let candidates = resp["candidates"].as_array().expect("candidates should be an array");
        assert_eq!(candidates.len(), 2);

        // Each candidate should have symbol_ref
        for c in candidates {
            assert!(c["symbol_ref"].is_object(), "candidate must have symbol_ref object");
            assert!(c["symbol_ref"]["qualified_name"].is_string());
            assert!(c["symbol_ref"]["file_path"].is_string());
            assert!(c["symbol_ref"]["line"].is_number());
            assert!(c["symbol_ref"]["kind"].is_string());
        }

        // No hex id at top level
        assert!(resp["id"].is_null(), "response should not have id field");
    }

    #[test]
    fn context_selector_precise_input_returns_full_context() {
        let store = test_store();
        let file_a = register_test_file(&store, "src/main.rs");
        let file_b = register_test_file(&store, "src/other.rs");

        // Two symbols with the same qname but different files/lines
        insert_test_symbol(
            &store,
            file_a,
            "process",
            "crate.process",
            atlas_engine::SymbolKind::Function,
            42,
        );
        insert_test_symbol(
            &store,
            file_b,
            "process",
            "crate.process",
            atlas_engine::SymbolKind::Function,
            100,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = super::super::ToolCallContext::empty();
        // Use structured selector with file_path + line hint to disambiguate
        let args = serde_json::json!({
            "symbol": {
                "qualified_name": "crate.process",
                "file_path": "src/main.rs",
                "line": 42
            }
        });
        let (resp_str, is_error) = router.handle_context(&ctx, &args);

        assert!(!is_error, "expected no error, got: {resp_str}");
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("expected valid JSON");

        // Should be a full context response, not ambiguous
        assert!(resp["ambiguous"].is_null());
        assert!(resp["subject"].is_object());
        assert!(
            resp["subject"]["qualified_name"].as_str() == Some("crate.process")
        );
    }

    #[test]
    fn context_ambiguous_response_has_no_hex_id() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        let file_b = register_test_file(&store, "b.ts");

        insert_test_symbol(
            &store,
            file_a,
            "shared",
            "shared",
            atlas_engine::SymbolKind::Function,
            1,
        );
        insert_test_symbol(
            &store,
            file_b,
            "shared",
            "shared",
            atlas_engine::SymbolKind::Variable,
            1,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = super::super::ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "shared"});
        let (resp_str, _is_error) = router.handle_context(&ctx, &args);
        let resp: serde_json::Value =
            serde_json::from_str(&resp_str).expect("expected valid JSON");

        let result = &resp;

        // No hex-id field anywhere in the result
        assert!(!resp_str.contains("\"id\":"));
        // candidates should use symbol_ref, never raw id
        if let Some(candidates) = result["candidates"].as_array() {
            for c in candidates {
                assert!(c["id"].is_null(), "candidate must not have id field: {c}");
                assert!(
                    c["symbol_id"].is_null(),
                    "candidate must not have symbol_id field: {c}"
                );
                assert!(
                    c["symbol_ref"].is_object(),
                    "candidate must have symbol_ref"
                );
            }
        }
    }

    #[test]
    fn context_nonexistent_qname_returns_not_found() {
        let store = test_store();
        let file_a = register_test_file(&store, "a.ts");
        insert_test_symbol(
            &store,
            file_a,
            "exists",
            "exists",
            atlas_engine::SymbolKind::Function,
            1,
        );

        let mut router = ToolRouter::new_empty(store, PathBuf::from("/tmp"));
        router.ensure_graph_initialized().unwrap();

        let ctx = super::super::ToolCallContext::empty();
        let args = serde_json::json!({"symbol": "nonexistent"});
        let (resp_str, is_error) = router.handle_context(&ctx, &args);

        assert!(is_error, "expected error for nonexistent symbol");
        assert!(
            resp_str.contains("not found"),
            "error should mention 'not found', got: {resp_str}"
        );
    }
}
