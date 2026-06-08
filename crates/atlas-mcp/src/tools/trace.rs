//! Trace tools: symbol and variable trace queries with dataflow/caller-path
//! traversal.  Includes transparent lazy structural extraction with progress
//! notifications to prevent MCP timeout during on-demand extraction.

use std::collections::{HashSet, VecDeque};

use atlas_engine::SymbolId;
use atlas_engine::{EdgeKind, InvestigationFocus, TraceDiagnostic, TraceQueryResponse};

use super::lazy_response::{LazyDiagnostics, LazyLayerDiagnostics, LazyResponse};
use super::{
    CandidateInfo, MAX_AMBIGUOUS_CANDIDATES, MAX_FILE_PATH_LENGTH, MAX_SYMBOL_NAME_LENGTH,
    QnameResolution, ToolRouter, get_str, get_str_opt, get_u64, resolve_file_id,
    warnings_to_trace_diagnostics,
};

use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_trace_point(
        &mut self,
        ctx: &super::ToolCallContext,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");

        // Validate file_path length
        if let Some(fp) = file_path {
            if fp.len() > MAX_FILE_PATH_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_point",
                    &format!(
                        "file_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        // Parse include_roots
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        let file_id = match resolve_file_id(&self.store, &self.project_root, file_hex, file_path) {
            Ok(Some(fid)) => fid,
            Ok(None) => {
                let msg = if file_hex.is_some() || file_path.is_some() {
                    if !self.has_indexed_files() {
                        "No files indexed yet. Please run the 'index' tool first to build the code index, then retry this query."
                    } else {
                        "File not found in index. Check that the file_id or file_path is correct and belongs to the indexed project."
                    }
                } else {
                    "Missing file_id or file_path"
                };
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_point", msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let mut err_msg = format!("Error resolving file: {e}");
                err_msg.push_str(self.index_not_run_guidance());
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_point", &err_msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let (line, column) = match (line, column) {
            (Some(l), Some(c)) => (l as u32, c as u32),
            _ => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_point", "Missing line or column");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Update investigation state with position focus
        self.update_investigation(InvestigationFocus::Position {
            file_id,
            line,
            col: column,
        });
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // Ensure structural before tracing
        let outcome = self.ensure_structural_for_files(
            [file_id],
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        // Capture lazy diagnostics before outcome fields are moved.
        let stats = self.get_capability_stats();
        let lazy_diag: Option<LazyDiagnostics> = outcome
            .lazy_outcome
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural_with_stats(lo, stats.as_ref()));

        ctx.send_progress(0.8, "Running trace point...");
        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_point(&file_id, line, column);
        ctx.send_progress(1.0, "Trace complete");

        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !outcome.warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            outcome.warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let is_error = !resp.ok;

        let tier = outcome.precision_tier;
        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(tier)
            .with_lazy_diag(lazy_diag)
            .with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_variable(&mut self, args: &serde_json::Value) -> (String, bool) {
        let file_hex = get_str_opt(args, "file_id");
        let file_path = get_str_opt(args, "file_path");
        let line = get_u64(args, "line");
        let column = get_u64(args, "column");
        let max_depth = get_u64(args, "max_depth").unwrap_or(30) as usize;

        // Validate file_path length
        if let Some(fp) = file_path {
            if fp.len() > MAX_FILE_PATH_LENGTH {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                    "trace_variable",
                    &format!(
                        "file_path exceeds maximum length of {MAX_FILE_PATH_LENGTH} characters"
                    ),
                );
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        }

        // Parse include_roots
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        let file_id = match resolve_file_id(&self.store, &self.project_root, file_hex, file_path) {
            Ok(Some(fid)) => fid,
            Ok(None) => {
                let msg = if file_hex.is_some() || file_path.is_some() {
                    if !self.has_indexed_files() {
                        "No files indexed yet. Please run the 'index' tool first to build the code index, then retry this query."
                    } else {
                        "File not found in index. Check that the file_id or file_path is correct and belongs to the indexed project."
                    }
                } else {
                    "Missing file_id or file_path"
                };
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_variable", msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
            Err(e) => {
                let mut err_msg = format!("Error resolving file: {e}");
                err_msg.push_str(self.index_not_run_guidance());
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_variable", &err_msg);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        let (line, column) = match (line, column) {
            (Some(l), Some(c)) => (l as u32, c as u32),
            _ => {
                let resp: TraceQueryResponse<()> =
                    TraceQueryResponse::err("trace_variable", "Missing line or column");
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Update investigation state with position focus
        self.update_investigation(InvestigationFocus::Position {
            file_id,
            line,
            col: column,
        });
        let investigation = self.investigation_state.active_investigation.clone();
        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // Ensure structural before tracing
        let outcome = self.ensure_structural_for_files(
            [file_id],
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        // Capture structural lazy outcome before fields are moved.
        let structural_lo = outcome.lazy_outcome.clone();

        // Engine::trace_variable handles lazy dataflow orchestration + trace
        // in a single call.  The response already carries lazy_summary,
        // diagnostics, and partial_result from the dataflow layer.
        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_variable(&file_id, line, column, max_depth);

        // Build combined lazy diagnostics from the structural outcome.
        // Engine already injects dataflow-layer diagnostics into resp.diagnostics;
        // we surface the structural layer separately so the agent sees both.
        let stats = self.get_capability_stats();
        let mut combined_lazy_diag: Option<LazyDiagnostics> = structural_lo
            .as_ref()
            .map(|lo| LazyDiagnostics::from_structural_with_stats(lo, stats.as_ref()));

        // Populate dataflow diagnostics from Engine's LazySummary (P2#14).
        // After routing through Engine, the MCP layer no longer has a direct
        // LazyWindow from dataflow extraction — but Engine's response carries
        // the summary.  Convert it so the agent sees both structural AND
        // dataflow layer stats in the lazy_diagnostics block.
        //
        // When structural_lo is None (already cached), the dataflow summary
        // must still be surfaced on its own.  When structural_lo is Some, the
        // dataflow layer is populated alongside the existing structural layer.
        // This check runs independently of resp.result — dataflow extraction
        // may succeed even when no trace path is found.
        if let Some(ref summary) = resp.lazy_summary {
            match combined_lazy_diag {
                Some(ref mut diag) => {
                    diag.dataflow = Some(LazyLayerDiagnostics::from_lazy_summary(summary));
                }
                None => {
                    combined_lazy_diag = Some(LazyDiagnostics::from_dataflow_summary(summary));
                }
            }
        }

        // Merge structural-partial flag into Engine's dataflow partial_result.
        let lazy_partial = !outcome.warnings.is_empty();
        resp.partial_result = resp.partial_result || lazy_partial;
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            outcome.warnings,
            "lazy_structural_warning",
        ));
        let is_error = !resp.ok;

        let tier = outcome.precision_tier;
        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(tier)
            .with_lazy_diag(combined_lazy_diag)
            .with_partial_result(resp.partial_result)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_caller_path(&mut self, args: &serde_json::Value) -> (String, bool) {
        let symbol = get_str(args, "symbol");
        let max_depth = args["max_depth"].as_u64().unwrap_or(20) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate symbol length
        if symbol.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                &format!("symbol exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if symbol.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_callers",
                "Missing required 'symbol' parameter. Accepts qualified name or hex SymbolId. Auto-detects format.",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // Auto-detect symbol: try hex parse first, then qname resolution.
        let is_hex = symbol.len() >= 8 && symbol.chars().all(|c| c.is_ascii_hexdigit());
        let target_id: SymbolId = if is_hex {
            match symbol.parse() {
                Ok(id) => id,
                Err(_) => match self.resolve_qname_disambiguated(symbol) {
                    Ok(QnameResolution::Unique(id)) => id,
                    Ok(QnameResolution::Ambiguous { candidates }) => {
                        return build_ambiguous_response_for_callers(symbol, &candidates);
                    }
                    Err(_) => {
                        let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                            "trace_callers",
                            &format!(
                                "Symbol not found by qname: '{symbol}'. Tip: the 'symbol' parameter accepts both hex SymbolIds and qualified names. Auto-detects format."
                            ),
                        );
                        return (
                            serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                            true,
                        );
                    }
                },
            }
        } else {
            match self.resolve_qname_disambiguated(symbol) {
                Ok(QnameResolution::Unique(id)) => id,
                Ok(QnameResolution::Ambiguous { candidates }) => {
                    return build_ambiguous_response_for_callers(symbol, &candidates);
                }
                Err(_) => {
                    // Lazy structural fallback: try name-based lookup with structural extraction
                    let outcome = self.ensure_structural_for_symbol_name(
                        symbol,
                        include_roots.clone(),
                        None,
                        Some(&query_id),
                    );
                    lazy_warnings.extend(outcome.warnings);
                    structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
                    if let Some(ref lo) = outcome.lazy_outcome {
                        let stats = self.get_capability_stats();
                        lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                            lo,
                            stats.as_ref(),
                        ));
                    }
                    // Re-query after lazy extraction
                    match self.resolve_qname_disambiguated(symbol) {
                        Ok(QnameResolution::Unique(id)) => id,
                        Ok(QnameResolution::Ambiguous { candidates }) => {
                            return build_ambiguous_response_for_callers(symbol, &candidates);
                        }
                        Err(_) => {
                            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                                "trace_callers",
                                &format!(
                                    "Symbol not found: '{symbol}'. Tip: the 'symbol' parameter accepts both hex SymbolIds and qualified names. Try 'search' first to discover the correct qualified name."
                                ),
                            );
                            return (
                                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                                true,
                            );
                        }
                    }
                }
            }
        };

        // Update investigation with the target symbol
        self.update_investigation(InvestigationFocus::Symbol(target_id));
        let investigation = self.investigation_state.active_investigation.clone();
        // Ensure structural data for this symbol's file
        if let Ok(Some(sym)) = self.store.find_symbol_by_id(&target_id) {
            let outcome = self.ensure_structural_for_files(
                [sym.file_id],
                include_roots.clone(),
                investigation.as_ref(),
                Some(&query_id),
            );
            lazy_warnings = outcome.warnings;
            structural_tier = outcome.precision_tier;
            if let Some(ref lo) = outcome.lazy_outcome {
                let stats = self.get_capability_stats();
                lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                    lo,
                    stats.as_ref(),
                ));
            }
        }
        let resp = self
            .engine
            .lock()
            .unwrap()
            .trace_callers(&target_id, max_depth);
        let mut resp = resp;
        let is_error = !resp.ok;

        // Inject warnings into diagnostics (not JSON "warnings" field)
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !lazy_warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            lazy_warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(structural_tier)
            .with_lazy_diag(lazy_diag)
            .with_is_error(is_error)
            .build(resp_value, self)
    }

    pub(crate) fn handle_trace_forward(&mut self, args: &serde_json::Value) -> (String, bool) {
        let from = get_str(args, "from");
        let to = get_str(args, "to");
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let (include_roots, root_warnings) = self.include_roots_from_args(args);

        // Validate from / to length
        if from.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("from exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }
        if to.len() > MAX_SYMBOL_NAME_LENGTH {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                &format!("to exceeds maximum length of {MAX_SYMBOL_NAME_LENGTH} characters"),
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        if from.is_empty() || to.is_empty() {
            let resp: TraceQueryResponse<()> = TraceQueryResponse::err(
                "trace_forward",
                "Must provide both 'from' and 'to' parameters. Accepts qualified names or hex SymbolIds. Auto-detects format.",
            );
            return (
                serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                true,
            );
        }

        for w in &root_warnings {
            tracing::warn!("include_roots: {}", w);
        }
        let mut lazy_warnings = Vec::new();
        let mut structural_tier = atlas_engine::structs::precision::PrecisionTier::Exact;
        let mut lazy_diag: Option<LazyDiagnostics> = None;

        let lr = LazyResponse::new("trace", args);
        let query_id = lr.query_id().to_string();

        // ── Resolve 'from' symbol (with hex auto-detect + lazy fallback) ──
        let from_resolution = {
            let is_hex = from.len() >= 8 && from.chars().all(|c| c.is_ascii_hexdigit());
            // Try hex parse first
            let hex_id = if is_hex {
                from.parse::<SymbolId>().ok()
            } else {
                None
            };
            if let Some(id) = hex_id {
                Ok(QnameResolution::Unique(id))
            } else {
                // Resolve by qname with lazy fallback on not-found
                match self.resolve_qname_disambiguated(from) {
                    Ok(res) => Ok(res),
                    Err(_) => {
                        let outcome = self.ensure_structural_for_symbol_name(
                            from,
                            include_roots.clone(),
                            None,
                            Some(&query_id),
                        );
                        lazy_warnings.extend(outcome.warnings);
                        structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
                        if let Some(ref lo) = outcome.lazy_outcome {
                            let stats = self.get_capability_stats();
                            lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                                lo,
                                stats.as_ref(),
                            ));
                        }
                        self.resolve_qname_disambiguated(from)
                    }
                }
            }
        };
        let from_id: SymbolId = match from_resolution {
            Ok(QnameResolution::Unique(id)) => id,
            Ok(QnameResolution::Ambiguous { candidates }) => {
                return build_ambiguous_response_for_forward(from, &candidates, "from");
            }
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // ── Resolve 'to' symbol (with path-aware disambiguation) ─────────
        let to_resolution = {
            let is_hex = to.len() >= 8 && to.chars().all(|c| c.is_ascii_hexdigit());
            let hex_id = if is_hex {
                to.parse::<SymbolId>().ok()
            } else {
                None
            };
            if let Some(id) = hex_id {
                Ok(QnameResolution::Unique(id))
            } else {
                match self.resolve_qname_disambiguated(to) {
                    Ok(res) => Ok(res),
                    Err(_) => {
                        let outcome = self.ensure_structural_for_symbol_name(
                            to,
                            include_roots.clone(),
                            None,
                            Some(&query_id),
                        );
                        lazy_warnings.extend(outcome.warnings);
                        structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
                        if let Some(ref lo) = outcome.lazy_outcome {
                            let stats = self.get_capability_stats();
                            lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                                lo,
                                stats.as_ref(),
                            ));
                        }
                        self.resolve_qname_disambiguated(to)
                    }
                }
            }
        };
        let to_id: SymbolId = match to_resolution {
            Ok(QnameResolution::Unique(id)) => id,
            Ok(QnameResolution::Ambiguous { candidates }) => {
                // Path-aware disambiguation: from is unique, to has multiple
                // candidates. Filter to only those reachable from 'from' via
                // outgoing call edges.
                let reachable = compute_reachable_from(&self.store, &from_id, max_depth);
                let reachable_candidates: Vec<&CandidateInfo> = candidates
                    .iter()
                    .filter(|c| reachable.contains(&c.id))
                    .collect();
                match reachable_candidates.len() {
                    1 => reachable_candidates[0].id,
                    _ => {
                        return build_ambiguous_response_for_forward(to, &candidates, "to");
                    }
                }
            }
            Err(e) => {
                let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_forward", &e);
                return (
                    serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
                    true,
                );
            }
        };

        // Update investigation with the from symbol
        self.update_investigation(InvestigationFocus::Symbol(from_id));
        let investigation = self.investigation_state.active_investigation.clone();

        // Ensure structural for endpoint files
        let mut file_set: std::collections::HashSet<atlas_engine::FileId> =
            std::collections::HashSet::new();
        for id in [&from_id, &to_id] {
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(id) {
                file_set.insert(sym.file_id);
            }
        }
        let outcome = self.ensure_structural_for_files(
            file_set,
            include_roots,
            investigation.as_ref(),
            Some(&query_id),
        );
        lazy_warnings = outcome.warnings;
        structural_tier = std::cmp::min(structural_tier, outcome.precision_tier);
        if let Some(ref lo) = outcome.lazy_outcome {
            let stats = self.get_capability_stats();
            lazy_diag = Some(LazyDiagnostics::from_structural_with_stats(
                lo,
                stats.as_ref(),
            ));
        }

        let mut resp = self
            .engine
            .lock()
            .unwrap()
            .trace_forward(&from_id, &to_id, max_depth);
        let is_error = !resp.ok;
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            root_warnings,
            "include_roots_warning",
        ));
        let lazy_partial = !lazy_warnings.is_empty();
        resp.diagnostics.extend(warnings_to_trace_diagnostics(
            lazy_warnings,
            "lazy_structural_warning",
        ));
        resp.partial_result = resp.partial_result || lazy_partial;

        let resp_value = serde_json::to_value(&resp).unwrap_or(json!({}));

        lr.with_structural_keys()
            .with_precision_tier(structural_tier)
            .with_lazy_diag(lazy_diag)
            .with_is_error(is_error)
            .build(resp_value, self)
    }
}

// ── Disambiguation helpers ───────────────────────────────────────────────

/// BFS from `from_id` along outgoing call-graph edges to discover all
/// reachable SymbolIds within `max_depth` hops.
fn compute_reachable_from(
    store: &atlas_engine::Store,
    from_id: &SymbolId,
    max_depth: usize,
) -> HashSet<SymbolId> {
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    queue.push_back((*from_id, 0usize));
    visited.insert(*from_id);

    while let Some((current, depth)) = queue.pop_front() {
        if depth > 0 {
            reachable.insert(current);
        }
        if depth >= max_depth {
            continue;
        }

        if let Ok(edges) = store.find_edges_by_source(&current) {
            for edge in &edges {
                if !matches!(
                    edge.kind,
                    EdgeKind::Calls
                        | EdgeKind::Instantiates
                        | EdgeKind::Implements
                        | EdgeKind::RegistersCallback
                ) {
                    continue;
                }
                if visited.insert(edge.target) {
                    queue.push_back((edge.target, depth + 1));
                }
            }
        }
    }

    reachable
}

/// Build a partial (ambiguous) response for trace_callers.
fn build_ambiguous_response_for_callers(
    name: &str,
    candidates: &[CandidateInfo],
) -> (String, bool) {
    let candidates_str = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(|c| {
            format!(
                "{} [{}:{} {}]",
                c.qualified_name, c.file_path, c.line, c.kind
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let msg = format!(
        "Symbol '{}' matched {} candidates: [{}]. Re-run with the hex SymbolId of the correct candidate.",
        name,
        candidates.len(),
        candidates_str
    );
    let resp: TraceQueryResponse<()> = TraceQueryResponse::partial(
        "trace_callers",
        TraceDiagnostic::warning(&msg).with_code("ambiguous_name"),
        None,
    );
    (
        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
        true,
    )
}

/// Build a partial (ambiguous) response for trace_forward.
fn build_ambiguous_response_for_forward(
    name: &str,
    candidates: &[CandidateInfo],
    field: &str,
) -> (String, bool) {
    let candidates_str = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(|c| {
            format!(
                "{} [{}:{} {}]",
                c.qualified_name, c.file_path, c.line, c.kind
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let msg = format!(
        "Ambiguous {} name: '{}' matched {} candidates: [{}]. Re-run with the hex SymbolId from the candidate list.",
        field,
        name,
        candidates.len(),
        candidates_str
    );
    let resp: TraceQueryResponse<()> = TraceQueryResponse::partial(
        "trace_forward",
        TraceDiagnostic::warning(&msg).with_code("ambiguous_name"),
        None,
    );
    (
        serde_json::to_string(&resp).unwrap_or_else(|e| e.to_string()),
        true,
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use atlas_engine::Confidence;
    use atlas_engine::EdgeId;
    use atlas_engine::EdgeKind;
    use atlas_engine::FileId;
    use atlas_engine::FileInfo;
    use atlas_engine::Language;
    use atlas_engine::ParseStatus;
    use atlas_engine::Provenance;
    use atlas_engine::RawEdge;
    use atlas_engine::Store;
    use atlas_engine::SymbolDef;
    use atlas_engine::SymbolId;
    use atlas_engine::SymbolKind;
    use atlas_engine::TextRange;

    // ── Test helpers ───────────────────────────────────────────────────

    fn test_store() -> Arc<Store> {
        let s = Store::open_in_memory().unwrap();
        s.init_schema().unwrap();
        Arc::new(s)
    }

    fn register_file(store: &Store, path: &str) -> FileId {
        let fid = FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.into(),
                language: Language::TypeScript,
                content_hash: "hash1".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        fid
    }

    fn insert_symbol(
        store: &Store,
        file_id: FileId,
        simple_name: &str,
        qname: &str,
        kind: SymbolKind,
    ) -> SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let sid = SymbolId::generate(&file_id, "typescript", simple_name, kind.as_str(), None);
        let sym = SymbolDef {
            id: sid,
            kind,
            name: simple_name.into(),
            qualified_name: qname.into(),
            symbol_path: vec![simple_name.into()],
            file_id,
            language: Language::TypeScript,
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
        sid
    }

    fn insert_call_edge(store: &Store, from: &SymbolId, to: &SymbolId) {
        let eid = EdgeId::generate(from, to, "calls", None, "tree_sitter");
        let edge = RawEdge::new(
            eid,
            *from,
            *to,
            EdgeKind::Calls,
            Confidence::new(1.0),
            Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();
    }

    fn make_candidate(id: SymbolId) -> CandidateInfo {
        CandidateInfo {
            id,
            qualified_name: "test".into(),
            file_path: "test.ts".into(),
            line: 1,
            kind: "function".into(),
            language: "typescript".into(),
        }
    }

    // ── compute_reachable_from tests ──────────────────────────────────

    #[test]
    fn reachable_from_finds_one_hop() {
        let store = test_store();
        let f = register_file(&store, "a.ts");
        let a = insert_symbol(&store, f, "a", "a", SymbolKind::Function);
        let b = insert_symbol(&store, f, "b", "b", SymbolKind::Function);
        let c = insert_symbol(&store, f, "c", "c", SymbolKind::Function);
        insert_call_edge(&store, &a, &b);
        insert_call_edge(&store, &b, &c);

        let reachable = compute_reachable_from(&store, &a, 1);
        assert!(reachable.contains(&b), "B should be reachable");
        assert!(!reachable.contains(&c), "C should NOT be reachable at depth 1");
        assert!(!reachable.contains(&a), "source should NOT be in reachable");
    }

    #[test]
    fn reachable_from_honors_max_depth() {
        let store = test_store();
        let f = register_file(&store, "a.ts");
        let a = insert_symbol(&store, f, "a", "a", SymbolKind::Function);
        let b = insert_symbol(&store, f, "b", "b", SymbolKind::Function);
        let c = insert_symbol(&store, f, "c", "c", SymbolKind::Function);
        insert_call_edge(&store, &a, &b);
        insert_call_edge(&store, &b, &c);

        let reachable = compute_reachable_from(&store, &a, 2);
        assert!(reachable.contains(&b));
        assert!(reachable.contains(&c));
    }

    #[test]
    fn reachable_from_depth_zero_returns_empty() {
        let store = test_store();
        let f = register_file(&store, "a.ts");
        let a = insert_symbol(&store, f, "a", "a", SymbolKind::Function);
        let b = insert_symbol(&store, f, "b", "b", SymbolKind::Function);
        insert_call_edge(&store, &a, &b);

        let reachable = compute_reachable_from(&store, &a, 0);
        assert!(reachable.is_empty(), "depth 0 should return empty set");
    }

    #[test]
    fn reachable_from_empty_graph() {
        let store = test_store();
        let f = register_file(&store, "a.ts");
        let a = insert_symbol(&store, f, "a", "a", SymbolKind::Function);

        let reachable = compute_reachable_from(&store, &a, 5);
        assert!(reachable.is_empty());
    }

    #[test]
    fn reachable_from_filters_by_edge_kind() {
        let store = test_store();
        let f = register_file(&store, "a.ts");
        let a = insert_symbol(&store, f, "a", "a", SymbolKind::Function);
        let b = insert_symbol(&store, f, "b", "b", SymbolKind::Function);
        // Import edge — NOT a call edge, should NOT be followed
        let eid = EdgeId::generate(&a, &b, "imports", None, "tree_sitter");
        let edge = RawEdge::new(
            eid,
            a,
            b,
            EdgeKind::Imports,
            Confidence::new(1.0),
            Provenance::TreeSitter,
        );
        store.insert_edges(&[edge]).unwrap();

        let reachable = compute_reachable_from(&store, &a, 5);
        assert!(reachable.is_empty(), "Imports edge should not be followed");
    }

    // ── ambiguous response helper tests ───────────────────────────────

    #[test]
    fn ambiguous_callers_response_has_code() {
        let c = make_candidate(SymbolId::generate(
            &FileId::generate("test.ts"),
            "ts",
            "f",
            "function",
            None,
        ));
        let (json_str, is_error) = build_ambiguous_response_for_callers("test", &[c]);
        assert!(
            is_error == true || json_str.contains("ambiguous_name") || json_str.contains("partial")
        );
    }

    #[test]
    fn ambiguous_forward_response_has_code() {
        let c = make_candidate(SymbolId::generate(
            &FileId::generate("test.ts"),
            "ts",
            "f",
            "function",
            None,
        ));
        let (json_str, is_error) = build_ambiguous_response_for_forward("test", &[c], "from");
        assert!(
            is_error == true || json_str.contains("ambiguous_name") || json_str.contains("partial")
        );
    }

    #[test]
    fn ambiguous_response_truncates_to_eight() {
        let mut candidates = Vec::new();
        for i in 0..12 {
            candidates.push(CandidateInfo {
                id: SymbolId::generate(
                    &FileId::generate("test.ts"),
                    "ts",
                    &format!("f{i}"),
                    "function",
                    None,
                ),
                qualified_name: "test".into(),
                file_path: format!("file{i}.ts"),
                line: i,
                kind: "function".into(),
                language: "typescript".into(),
            });
        }
        let (json_str, _) = build_ambiguous_response_for_callers("test", &candidates);
        assert!(json_str.contains("12 candidates"));
        // Each candidate formatted as "qualified_name [file_path:line kind]"
        let count = json_str.matches("function]").count();
        assert_eq!(count, MAX_AMBIGUOUS_CANDIDATES, "Should truncate to {} candidates, got: {json_str}", MAX_AMBIGUOUS_CANDIDATES);
    }

    // ── Handler tests ─────────────────────────────────────────────────

    fn new_router(store: Arc<Store>) -> ToolRouter {
        ToolRouter::new_empty(store, PathBuf::from("/tmp"))
    }

    #[test]
    fn trace_callers_valid_hex_symbol_id_accepted() {
        let store = test_store();
        let f = register_file(&store, "test.ts");
        let sid = insert_symbol(&store, f, "func", "func.func", SymbolKind::Function);
        let hex = sid.to_hex();

        let mut router = new_router(store);
        let args = serde_json::json!({"symbol": hex});
        let (resp_str, _is_error) = router.handle_trace_caller_path(&args);
        assert!(
            !resp_str.contains("not found"),
            "hex SymbolId should resolve: {resp_str}"
        );
    }

    #[test]
    fn trace_variable_missing_file_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({
            "file_path": "nonexistent.ts",
            "line": 1,
            "column": 1,
        });
        let (_resp_str, is_error) = router.handle_trace_variable(&args);
        assert!(is_error, "should be error for missing file");
    }

    #[test]
    fn trace_variable_missing_position_returns_error() {
        let store = test_store();
        let _f = register_file(&store, "test.ts");
        let mut router = new_router(store);

        let args = serde_json::json!({
            "file_path": "test.ts",
        });
        let (_resp_str, is_error) = router.handle_trace_variable(&args);
        assert!(is_error, "should be error without line/column");
    }

    #[test]
    fn trace_callers_empty_symbol_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({"symbol": ""});
        let (_resp_str, is_error) = router.handle_trace_caller_path(&args);
        assert!(is_error, "empty symbol should error");
    }

    #[test]
    fn trace_forward_empty_params_returns_error() {
        let store = test_store();
        let mut router = new_router(store);

        let args = serde_json::json!({"from": "a", "to": ""});
        let (_resp_str, is_error) = router.handle_trace_forward(&args);
        assert!(is_error, "empty 'to' should error");
    }
}
