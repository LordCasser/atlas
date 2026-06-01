//! Context tool: builds rich markdown context for a symbol.
//!
//! Includes transparent lazy structural extraction when the symbol is not yet
//! indexed. After lazy extraction writes new facts to the DB, the in-memory
//! graph snapshot is force-refreshed so that the context builder sees the
//! newly parsed edges — closing the MCP call-flow gap where graph init
//! happened before the handler's own structural extraction.

use super::lazy_response::LazyDiagnostics;
use super::{ToolRouter, get_str, MAX_SYMBOL_NAME_LENGTH};

use atlas_engine::structs::precision::PrecisionTier;
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_context(&mut self, args: &serde_json::Value) -> (String, bool) {
        let qname = get_str(args, "symbol");
        if qname.len() > MAX_SYMBOL_NAME_LENGTH {
            return (
                format!(
                    "Symbol name exceeds maximum length of {} characters",
                    MAX_SYMBOL_NAME_LENGTH
                ),
                true,
            );
        }
        let include_code = args
            .get("includeCode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.send_progress(0.2, &format!("Building context for '{}'...", qname));

        let (include_roots, root_warnings) = self.include_roots_from_args(args);
        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }

        let (sid, lazy_warnings, tier, lazy_diag) =
            match self.resolve_context_symbol(qname, include_roots) {
                Ok(r) => r,
                Err(err) => return (err, true),
            };

        // Force-refresh the graph to pick up edges written by lazy structural
        // (Tier 3 in resolve_context_symbol). Without this, the context builder
        // operates on a stale snapshot loaded before the handler's own
        // structural extraction.
        if let Err(e) = self.force_refresh_graph() {
            return (format!("Graph refresh error: {:#}", e), true);
        }

        self.send_progress(0.7, "Building context view...");
        match self.context_builder().build_context_for_symbol(&sid) {
            Ok(view) => {
                let md = view.to_markdown();
                self.send_progress(1.0, "Context complete");
                let mut result = json!({
                    "markdown": md,
                    "precision_tier": serde_json::to_value(tier).unwrap_or(json!(null)),
                });
                if tier != PrecisionTier::Exact {
                    if let Some(hint) = atlas_engine::precision::next_action_structural(tier) {
                        result["hint"] = json!(hint);
                    }
                }
                if include_code {
                    if let Some(src) = self.read_symbol_source(&sid) {
                        result["source"] = json!(src);
                    }
                }
                // Surface include_roots and lazy-structural warnings to the caller.
                let mut all_warnings: Vec<String> = root_warnings;
                all_warnings.extend(lazy_warnings);
                if !all_warnings.is_empty() {
                    result["warnings"] = json!(all_warnings);
                }
                if let Some(diag) = &lazy_diag {
                    result["lazy_diagnostics"] = serde_json::to_value(diag).unwrap_or(json!(null));
                }
                (
                    serde_json::to_string_pretty(&result).unwrap_or_else(|e| e.to_string()),
                    false,
                )
            }
            Err(e) => (format!("Context build error: {}", e), true),
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
        qname: &str,
        include_roots: Vec<atlas_engine::IncludeRoot>,
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

        // ── Tier 1: exact qualified-name match ──
        let symbols = self.store.find_symbols_by_qname(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on find_symbols_by_qname: {}", e);
            Default::default()
        });
        if let Some(id) = symbols.first().map(|s| s.id) {
            // Ensure structural data for this file (include_roots optional,
            // always relevant) so graph queries see complete edges.
            let outcome = self.ensure_structural_for_files([symbols[0].file_id], include_roots);
            warnings.extend(outcome.warnings);
            worst_tier = cmp::min(worst_tier, outcome.precision_tier);
            if let Some(ref lo) = outcome.lazy_outcome {
                lazy_diag = Some(LazyDiagnostics::from_structural(lo));
            }
            return Ok((id, warnings, worst_tier, lazy_diag));
        }

        // ── Tier 2: name-based search (look for symbol by simple name) ──
        let name_matches = self.store.find_symbols_by_name(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on find_symbols_by_name: {}", e);
            Default::default()
        });
        if name_matches.len() == 1 {
            // Unambiguous — use it directly
            let outcome =
                self.ensure_structural_for_files([name_matches[0].file_id], include_roots);
            warnings.extend(outcome.warnings);
            worst_tier = cmp::min(worst_tier, outcome.precision_tier);
            if let Some(ref lo) = outcome.lazy_outcome {
                lazy_diag = Some(LazyDiagnostics::from_structural(lo));
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
                let outcome =
                    self.ensure_structural_for_files([matching_qnames[0].file_id], include_roots);
                warnings.extend(outcome.warnings);
                worst_tier = cmp::min(worst_tier, outcome.precision_tier);
                if let Some(ref lo) = outcome.lazy_outcome {
                    lazy_diag = Some(LazyDiagnostics::from_structural(lo));
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
            self.send_progress(0.5, "Extracting structural data...");
            let outcome = self.ensure_structural_for_symbol_name(qname, include_roots.clone());
            warnings.extend(outcome.warnings);
            worst_tier = cmp::min(worst_tier, outcome.precision_tier);
            if let Some(ref lo) = outcome.lazy_outcome {
                lazy_diag = Some(LazyDiagnostics::from_structural(lo));
            }
        }

        // Re-query after lazy extraction (tier 1 again on freshly-parsed data)
        let retry = self.store.find_symbols_by_qname(qname).unwrap_or_else(|e| {
            tracing::warn!("DB error on retry find_symbols_by_qname: {}", e);
            Default::default()
        });
        if let Some(sym) = retry.first() {
            return Ok((sym.id, warnings, worst_tier, lazy_diag));
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
            "Symbol '{}' not found by qualified name or simple name. Try 'search' first to discover the correct qualified_name for this symbol.",
            qname
        );
        err.push_str(self.index_not_run_guidance());
        Err(err)
    }
}
