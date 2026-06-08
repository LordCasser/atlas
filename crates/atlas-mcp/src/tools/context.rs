//! Context tool: builds rich markdown context for a symbol.
//!
//! Includes transparent lazy structural extraction when the symbol is not yet
//! indexed. After lazy extraction writes new facts to the DB, the in-memory
//! graph snapshot is force-refreshed so that the context builder sees the
//! newly parsed edges — closing the MCP call-flow gap where graph init
//! happened before the handler's own structural extraction.

use super::lazy_response::{LazyDiagnostics, LazyResponse};
use super::{MAX_SYMBOL_NAME_LENGTH, QnameResolution, ToolRouter, get_str};

use atlas_engine::InvestigationFocus;
use atlas_engine::structs::precision::PrecisionTier;
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_context(
        &mut self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let qname = get_str(args, "symbol");
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
        let initial_sid = match self.resolve_qname_disambiguated(qname) {
            Ok(QnameResolution::Unique(id)) => Some(id),
            _ => None,
        };
        if let Some(sid) = initial_sid {
            self.update_investigation(InvestigationFocus::Symbol(sid));
        }
        let investigation = self.investigation_state.active_investigation.clone();

        let (sid, lazy_warnings, tier, lazy_diag) = match self.resolve_context_symbol(
            ctx,
            qname,
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        ) {
            Ok(r) => r,
            Err(err) => return (err, true),
        };

        // Update investigation with the actually resolved symbol
        self.update_investigation(InvestigationFocus::Symbol(sid));

        // Force-refresh the graph to pick up edges written by lazy structural
        // (Tier 3 in resolve_context_symbol). Without this, the context builder
        // operates on a stale snapshot loaded before the handler's own
        // structural extraction.
        if let Err(e) = self.force_refresh_graph() {
            return (format!("Graph refresh error: {e:#}"), true);
        }

        ctx.send_progress(0.7, "Building context view...");
        match self
            .context_builder()
            .build_context_for_symbol(&sid, include_file_peers)
        {
            Ok(view) => {
                ctx.send_progress(1.0, "Context complete");

                // ── subject ────────────────────────────────────────────────
                let subject = serde_json::to_value(&view.subject).unwrap_or(json!(null));

                // ── subject_source ─────────────────────────────────────────
                // When includeCode, override with full source; otherwise use
                // the context builder's preview (first N lines).
                let subject_source = if include_code {
                    if let Some(src) = self.read_symbol_source(&sid) {
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

                // ── caller_details ─────────────────────────────────────────
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

                // ── callee_details ─────────────────────────────────────────
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

                // ── file_peers ─────────────────────────────────────────────
                let file_peers: Vec<serde_json::Value> = view
                    .file_peers
                    .iter()
                    .map(|p| serde_json::to_value(p).unwrap_or(json!(null)))
                    .collect();

                // ── trail ──────────────────────────────────────────────────
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

                // ── assemble result (body only, without envelope fields) ────
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

                let mut stored_args = args.clone();
                if let Some(obj) = stored_args.as_object_mut() {
                    obj.insert("view".into(), serde_json::Value::String("context".into()));
                }

                lr.with_precision_tier(tier)
                    .with_root_warnings(root_warnings)
                    .with_lazy_warnings(lazy_warnings)
                    .with_lazy_diag(lazy_diag)
                    .with_is_error(false)
                    .build_with_args(result, &stored_args, self)
            }
            Err(e) => (format!("Context build error: {e}"), true),
        }
    }

    /// Resolve a symbol for context display with multi-tier fallback.
    ///
    /// Tier 1: exact qualified-name match (e.g. "Engine.Engine")
    /// Tier 2: name-based match — finds symbols whose simple name matches,
    ///         then picks the highest-scored unambiguous match
    /// Tier 3: lazy structural extraction + re-query
    /// Tier 4: name match with multiple candidates → return suggestions
    ///
    /// Returns the resolved SymbolId, any lazy-structural warnings, the
    /// worst precision tier encountered, and optional lazy diagnostics
    /// built from the most recent structural extraction.
    fn resolve_context_symbol(
        &mut self,
        ctx: &super::ToolCallContext,
        qname: &str,
        include_roots: Vec<atlas_engine::IncludeRoot>,
        investigation: Option<&atlas_engine::Investigation>,
        query_id: Option<&str>,
    ) -> Result<
        (
            atlas_engine::SymbolId,
            Vec<String>,
            PrecisionTier,
            Option<LazyDiagnostics>,
        ),
        String,
    > {
        use std::cmp;

        let mut warnings = Vec::new();
        let mut worst_tier = PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        // ── Tier 1: exact qualified-name match (disambiguated) ──
        match self.resolve_qname_disambiguated(qname) {
            Ok(QnameResolution::Unique(id)) => {
                // Look up symbol info for file_id
                if let Ok(Some(sym)) = self.store.find_symbol_by_id(&id) {
                    let outcome = self.ensure_structural_for_files(
                        [sym.file_id],
                        include_roots,
                        investigation,
                        query_id,
                    );
                    warnings.extend(outcome.warnings);
                    worst_tier = cmp::min(worst_tier, outcome.precision_tier);
                    if let Some(ref lo) = outcome.lazy_outcome {
                        let stats = self.get_capability_stats();
                        lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                            lo,
                            stats.as_ref(),
                        ));
                    }
                    return Ok((id, warnings, worst_tier, lazy_diag));
                }
                // Symbol ID not in store — fall through
            }
            Ok(QnameResolution::Ambiguous { candidates }) => {
                let names: Vec<String> = candidates
                    .iter()
                    .take(8)
                    .map(|c| {
                        format!(
                            "{} [{}:{}:{}]",
                            c.qualified_name, c.file_path, c.line, c.kind
                        )
                    })
                    .collect();
                let mut err = format!(
                    "Symbol '{}' is ambiguous ({} matches). Did you mean one of: [{}]? Use the exact qualified_name.",
                    qname,
                    candidates.len(),
                    names.join(", ")
                );
                err.push_str(self.index_not_run_guidance());
                return Err(err);
            }
            Err(_) => {
                // Not found by qname — fall through to Tier 2
            }
        }

        // ── Tier 2: name-based search (look for symbol by simple name) ──
        let name_matches = self.store.find_symbols_by_name(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on find_symbols_by_name: {}", e);
            Default::default()
        });
        if name_matches.len() == 1 {
            // Unambiguous — use it directly
            let outcome = self.ensure_structural_for_files(
                [name_matches[0].file_id],
                include_roots,
                investigation,
                query_id,
            );
            warnings.extend(outcome.warnings);
            worst_tier = cmp::min(worst_tier, outcome.precision_tier);
            if let Some(ref lo) = outcome.lazy_outcome {
                let stats = self.get_capability_stats();
                lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                    lo,
                    stats.as_ref(),
                ));
            }
            return Ok((name_matches[0].id, warnings, worst_tier, lazy_diag));
        }
        if name_matches.len() > 1 {
            // Multiple matches — try case-insensitive qualified-name substring
            // Check if the query is a case-insensitive prefix/suffix of any
            // qualified name for an unambiguous result.
            let q_lower = qname.to_lowercase();
            let matching_qnames: Vec<_> = name_matches
                .iter()
                .filter(|s| s.qualified_name.to_lowercase().contains(&q_lower))
                .collect();
            if matching_qnames.len() == 1 {
                let outcome = self.ensure_structural_for_files(
                    [matching_qnames[0].file_id],
                    include_roots,
                    investigation,
                    query_id,
                );
                warnings.extend(outcome.warnings);
                worst_tier = cmp::min(worst_tier, outcome.precision_tier);
                if let Some(ref lo) = outcome.lazy_outcome {
                    let stats = self.get_capability_stats();
                    lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                        lo,
                        stats.as_ref(),
                    ));
                }
                return Ok((matching_qnames[0].id, warnings, worst_tier, lazy_diag));
            }
            if matching_qnames.len() > 1 {
                let suggestions: Vec<&str> = matching_qnames
                    .iter()
                    .take(8)
                    .map(|s| s.qualified_name.as_str())
                    .collect();
                let mut err = format!(
                    "Symbol '{}' matched {} symbols by name. Did you mean one of: [{}]? Use the exact qualified_name from this list in the 'symbol' parameter.",
                    qname,
                    matching_qnames.len(),
                    suggestions.join(", ")
                );
                err.push_str(self.index_not_run_guidance());
                return Err(err);
            }
        }

        // ── Tier 3: try lazy structural, then re-query ──
        // `has_manual_full_index()` is checked inside
        // `ensure_structural_for_symbol_name`, but tier 3 does
        // symbol-based lookup so we check here to skip the progress
        // message when lazy work is a no-op.
        let is_manual_full = self.has_manual_full_index();
        if !is_manual_full {
            ctx.send_progress(0.5, "Extracting structural data...");
            let outcome = self.ensure_structural_for_symbol_name(
                qname,
                include_roots.clone(),
                investigation,
                query_id,
            );
            warnings.extend(outcome.warnings);
            worst_tier = cmp::min(worst_tier, outcome.precision_tier);
            if let Some(ref lo) = outcome.lazy_outcome {
                let stats = self.get_capability_stats();
                lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                    lo,
                    stats.as_ref(),
                ));
            }
        }

        // Re-query after lazy extraction (tier 1 again on freshly-parsed data)
        match self.resolve_qname_disambiguated(qname) {
            Ok(QnameResolution::Unique(id)) => {
                return Ok((id, warnings, worst_tier, lazy_diag));
            }
            Ok(QnameResolution::Ambiguous { candidates }) => {
                let names: Vec<String> = candidates
                    .iter()
                    .take(8)
                    .map(|c| {
                        format!(
                            "{} [{}:{}:{}]",
                            c.qualified_name, c.file_path, c.line, c.kind
                        )
                    })
                    .collect();
                let mut err = format!(
                    "After lazy extraction, symbol '{}' is still ambiguous ({} matches). Did you mean one of: [{}]?",
                    qname,
                    candidates.len(),
                    names.join(", ")
                );
                err.push_str(self.index_not_run_guidance());
                return Err(err);
            }
            Err(_) => {
                // Still not found — fall through to Tier 4
            }
        }

        // Re-check name after lazy extraction
        let fresh_matches = self.store.find_symbols_by_name(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on retry find_symbols_by_name: {}", e);
            Default::default()
        });
        if fresh_matches.len() == 1 {
            return Ok((fresh_matches[0].id, warnings, worst_tier, lazy_diag));
        }
        if fresh_matches.len() > 1 {
            let suggestions: Vec<&str> = fresh_matches
                .iter()
                .take(8)
                .map(|s| s.qualified_name.as_str())
                .collect();
            let mut err = format!(
                "Symbol '{}' matched {} symbols by name. Did you mean one of: [{}]? Use the exact qualified_name from this list in the 'symbol' parameter.",
                qname,
                fresh_matches.len(),
                suggestions.join(", ")
            );
            err.push_str(self.index_not_run_guidance());
            return Err(err);
        }

        // ── Tier 4: nothing found ──
        let mut err = format!(
            "Symbol '{qname}' not found by qualified name or simple name. Try 'search' first to discover the correct qualified_name for this symbol."
        );
        err.push_str(self.index_not_run_guidance());
        Err(err)
    }
}
