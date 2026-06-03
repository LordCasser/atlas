//! TraceEngine — unified public API for trace queries.
//!
//! Wraps Locator, Slicer, and CallerPathExplorer behind a single service
//! object with capability-gated execution and a uniform
//! [`TraceQueryResponse<T>`] envelope.
//!
//! # Design
//!
//! - **Capability-gating**: queries that require dataflow (trace_variable) or
//!   call-graph (trace_callers) check the language profile before execution.
//!   If the language doesn't support the feature, the response is `ok` with
//!   `partial_result = true` and a diagnostic, never an error.
//! - **Uniform envelope**: every query returns `TraceQueryResponse<T>` so CLI
//!   and MCP code paths only serialize one shape.
//! - **File resolution**: `resolve_file_id_with_root` maps user-facing paths
//!   to internal `FileId` with exact, suffix, and absolute-path probing.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;
use serde_json;

use crate::trace::{CallerPathExplorer, ForwardPathExplorer, Locator, Slicer};
use db::Store;
use types::caller_path::{CallerChain, ForwardChain};
use types::capability::{CapabilityLevel, FeatureSupport, LanguageCapabilityProfile};
use types::ids::{FileId, SymbolId};
use types::trace::{
    BoundaryKind, BoundaryMarker, Evidence, LazySummary, TraceDiagnostic, TracePath, TracePoint,
};

use super::virtual_edges::SummaryEdgeProvider;

// ─── Response envelope ────────────────────────────────────────────────────

/// Uniform response for every trace query.
///
/// `ok` indicates the query was processed without error (even if the result is
/// partial or empty).  `partial_result` + `diagnostics` carry structured hints
/// about completeness.
#[derive(Debug, Clone, Serialize)]
pub struct TraceQueryResponse<T> {
    /// Whether the query succeeded at the transport level.
    pub ok: bool,
    /// The kind of query (e.g. "trace_point", "trace_variable", "trace_callers").
    pub kind: String,
    /// Language capability profile for the resolved language.
    /// Always present in JSON (null when unavailable) per trace-contract.md.
    #[serde(default)]
    pub capability: Option<LanguageCapabilityProfile>,
    /// Best-effort / incomplete result.
    #[serde(default)]
    pub partial_result: bool,
    /// Structured diagnostics (warnings, notes).
    #[serde(default)]
    pub diagnostics: Vec<TraceDiagnostic>,
    /// The query result, if any.
    pub result: Option<T>,
    /// Metadata about lazy dataflow loading that occurred during this query.
    /// None if lazy loading was not triggered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy_summary: Option<LazySummary>,
}

impl<T> TraceQueryResponse<T> {
    /// Full success with a concrete result.
    pub fn ok(kind: &str, result: T, capability: Option<LanguageCapabilityProfile>) -> Self {
        Self {
            ok: true,
            kind: kind.to_string(),
            capability,
            partial_result: false,
            diagnostics: vec![],
            result: Some(result),
            lazy_summary: None,
        }
    }

    /// Query succeeded but no result could be produced (e.g. unsupported
    /// language, no data node, no callers). Consumers should inspect
    /// `diagnostics` for the reason.
    pub fn partial(
        kind: &str,
        diagnostic: TraceDiagnostic,
        capability: Option<LanguageCapabilityProfile>,
    ) -> Self {
        Self {
            ok: true,
            kind: kind.to_string(),
            capability,
            partial_result: true,
            diagnostics: vec![diagnostic],
            result: None,
            lazy_summary: None,
        }
    }

    /// Query failed due to a system error (I/O, DB corruption, etc.).
    pub fn err(kind: &str, message: &str) -> Self {
        Self {
            ok: false,
            kind: kind.to_string(),
            capability: None,
            partial_result: false,
            diagnostics: vec![TraceDiagnostic::error(message)],
            result: None,
            lazy_summary: None,
        }
    }
}

// ─── TraceEngine ──────────────────────────────────────────────────────────

/// Single entry-point for all trace queries.
///
/// Holds an `Arc<Store>` and optional project root, and provides three public
/// query methods: [`trace_point`], [`trace_variable`], and [`trace_callers`].
pub struct TraceEngine {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    /// Cached canonical project root (computed once at construction).
    /// Avoids repeated `canonicalize()` syscalls in `is_file_in_project`.
    canonical_root: Option<PathBuf>,
    /// Cache of file path → content, keyed by canonical path.
    /// Avoids repeated `read_to_string` for the same file in
    /// `extract_snippet` / `extract_context_snippet`.
    file_cache: RefCell<HashMap<PathBuf, String>>,
}

impl TraceEngine {
    /// Create a new trace engine backed by the given store (no project root).
    ///
    /// Without a project root, [`Evidence.snippet`] will always be `None`.
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            project_root: None,
            canonical_root: None,
            file_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Create a new trace engine with a project root for snippet extraction.
    ///
    /// When a project root is provided, [`Evidence.snippet`] will be populated
    /// by reading the relevant source line from disk.
    pub fn new_with_root(store: Arc<Store>, project_root: PathBuf) -> Self {
        let canonical_root = project_root.canonicalize().ok();
        Self {
            store,
            project_root: Some(project_root),
            canonical_root,
            file_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Create a new trace engine with a [`Workspace`] for snippet extraction.
    ///
    /// Convenience wrapper around [`Self::new_with_root`] that uses the
    /// workspace's canonical project root.
    pub fn new_with_workspace(store: Arc<Store>, workspace: &workspace::Workspace) -> Self {
        let project_root = workspace.root().to_path_buf();
        let canonical_root = project_root.canonicalize().ok();
        Self {
            store,
            project_root: Some(project_root),
            canonical_root,
            file_cache: RefCell::new(HashMap::new()),
        }
    }

    // ── Public query methods ───────────────────────────────────────────

    /// Resolve a source position to a full [`TracePoint`].
    ///
    /// Always available regardless of language capability.
    pub fn trace_point(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
    ) -> TraceQueryResponse<TracePoint> {
        let cap = self.resolve_capability(file_id);

        match Locator::locate(self.store.as_ref(), file_id, line, column) {
            Ok(mut point) => {
                point.capability = cap.clone();
                TraceQueryResponse::ok("trace_point", point, cap)
            }
            Err(e) => TraceQueryResponse::err("trace_point", &format!("{e}")),
        }
    }

    /// Trace dataflow backward from a source position.
    ///
    /// Requires `CapabilityLevel::DataflowBasic` or higher.  If the language
    /// does not support dataflow, the response is partial with an
    /// `unsupported_language` diagnostic.
    pub fn trace_variable(
        &self,
        file_id: &FileId,
        line: u32,
        column: u32,
        max_depth: usize,
    ) -> TraceQueryResponse<TracePath> {
        let cap = self.resolve_capability(file_id);

        // Capability gate: prefer FeatureMatrix (type-safe), fallback to CapabilityLevel
        let dataflow_supported = cap
            .as_ref()
            .and_then(|c| c.features.as_ref())
            .map(|f| f.local_dataflow.is_supported())
            .unwrap_or_else(|| {
                // Fallback for profiles without FeatureMatrix
                let level = cap
                    .as_ref()
                    .map(|c| c.capability_level)
                    .unwrap_or(CapabilityLevel::None);
                level >= CapabilityLevel::DataflowBasic
            });
        if !dataflow_supported {
            let reason = cap
                .as_ref()
                .and_then(|c| c.features.as_ref())
                .and_then(|f| match &f.local_dataflow {
                    FeatureSupport::Unsupported { reason } => Some(reason.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    format!(
                        "capability: {:?}",
                        cap.as_ref()
                            .map(|c| c.capability_level)
                            .unwrap_or(CapabilityLevel::None)
                    )
                });
            return TraceQueryResponse::partial(
                "trace_variable",
                TraceDiagnostic::warning(&format!(
                    "Dataflow not supported for this language ({reason})"
                ))
                .with_code("unsupported_language"),
                cap,
            );
        }

        let sink = match Locator::locate(self.store.as_ref(), file_id, line, column) {
            Ok(p) => p,
            Err(e) => return TraceQueryResponse::err("trace_variable", &format!("{e}")),
        };

        if sink.data_node.is_none() {
            return TraceQueryResponse::partial(
                "trace_variable",
                TraceDiagnostic::warning("No data node at this position").with_code("no_data_node"),
                cap,
            );
        }

        match Slicer::slice(
            self.store.as_ref(),
            &sink,
            max_depth,
            Some(&SummaryEdgeProvider),
        ) {
            Ok(Some(mut path)) => {
                path.capability = cap.clone();
                self.enrich_trace_path_steps(&mut path);
                TraceQueryResponse::ok("trace_variable", path, cap)
            }
            Ok(None) => TraceQueryResponse::partial(
                "trace_variable",
                TraceDiagnostic::warning("Slicer could not walk backward from this data node")
                    .with_code("no_trace_path"),
                cap,
            ),
            Err(e) => TraceQueryResponse::err("trace_variable", &format!("{e}")),
        }
    }

    /// Trace the call chain backward from a target symbol to its farthest
    /// caller.
    ///
    /// Requires `call_graph` feature in the language's profile.  If
    /// not available, the response is partial with an `unsupported_language`
    /// diagnostic.
    pub fn trace_callers(
        &self,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> TraceQueryResponse<CallerChain> {
        // Resolve capability from symbol's language
        let cap = self
            .store
            .find_symbol_by_id(target_id)
            .ok()
            .flatten()
            .map(|s| LanguageCapabilityProfile::for_language(s.language));

        if !self.has_call_graph_cap(&cap) {
            return TraceQueryResponse::partial(
                "trace_callers",
                TraceDiagnostic::warning("Call graph not supported for this language")
                    .with_code("unsupported_language"),
                cap,
            );
        }

        match CallerPathExplorer::explore(self.store.as_ref(), target_id, max_depth) {
            Ok(Some(mut chain)) => {
                self.enrich_caller_chain_steps(&mut chain);

                let mut diagnostics = build_boundary_diagnostics(step_boundaries(&chain.steps));

                // If truncated by depth (not by boundary), add a classified diagnostic
                if chain.truncated && diagnostics.is_empty() {
                    diagnostics.push(
                        TraceDiagnostic::warning(&format!(
                            "Caller path truncated: reached depth {} of max_depth={}. More callers may exist beyond this limit.",
                            chain.max_depth_reached, max_depth
                        ))
                        .with_code("max_depth_truncated"),
                    );
                }

                // Trail: actionable next steps for the Agent
                add_caller_chain_trail(&mut diagnostics, &chain);

                let partial = chain.truncated || !diagnostics.is_empty();
                TraceQueryResponse {
                    ok: true,
                    kind: "trace_callers".to_string(),
                    capability: cap,
                    partial_result: partial,
                    diagnostics,
                    result: Some(chain),
                    lazy_summary: None,
                }
            }
            Ok(None) => TraceQueryResponse::partial(
                "trace_callers",
                TraceDiagnostic::warning("No callers found — this is a root/top-level function")
                    .with_code("no_callers"),
                cap,
            ),
            Err(e) => TraceQueryResponse::err("trace_callers", &format!("{e}")),
        }
    }

    // ── Symbol resolution ─────────────────────────────────────────────

    /// Find all symbol IDs matching a name across all indexed files.
    ///
    /// Uses indexed `symbols.name` lookup — O(log n) per lookup.
    pub fn find_symbol_ids_by_name(&self, name: &str) -> anyhow::Result<Vec<SymbolId>> {
        Ok(self
            .store
            .find_symbols_by_name(name)?
            .into_iter()
            .map(|s| s.id)
            .collect())
    }

    /// Trace callers by symbol name (human-friendly lookup).
    ///
    /// Searches all files for symbols matching `name`, then runs
    /// [`trace_callers`] on the first match. If multiple symbols share the
    /// name, the first one found (alphabetical by file path) is used.
    ///
    /// When multiple symbols match the same name (e.g. `bufio.Scanner.Next`
    /// vs `Context.Next`), the response includes a `multiple_matches`
    /// diagnostic listing all candidates so the Agent can re-query with a
    /// specific hex ID.
    pub fn trace_callers_by_name(
        &self,
        name: &str,
        max_depth: usize,
    ) -> TraceQueryResponse<CallerChain> {
        let symbols = match self.store.find_symbols_by_name(name) {
            Ok(s) => s,
            Err(e) => return TraceQueryResponse::err("trace_callers", &e.to_string()),
        };
        if symbols.is_empty() {
            return TraceQueryResponse::partial(
                "trace_callers",
                TraceDiagnostic::warning(&format!("Symbol '{name}' not found in index"))
                    .with_code("symbol_not_found"),
                None,
            );
        }

        // When multiple symbols share the same name, list all candidates
        // so the Agent can disambiguate.  Do NOT silently pick the first —
        // this was the root cause of tracing `bufio.Scanner.Next` when the
        // user meant `Context.Next`.
        //
        // Enhancement: annotate each candidate with whether it belongs to
        // the same project (in_project).  If exactly one in-project candidate
        // exists, auto-select it (same-project heuristic).
        if symbols.len() > 1 {
            let candidates_with_meta: Vec<serde_json::Value> = symbols
                .iter()
                .take(10)
                .map(|s| {
                    let file_path = self.resolve_file_path(&s.file_id).unwrap_or_default();
                    let in_project = self.is_file_in_project(&file_path);
                    let container_info = s.container.map(|cid| {
                        self.store
                            .find_symbol_by_id(&cid)
                            .ok()
                            .flatten()
                            .map(|cs| cs.qualified_name)
                            .unwrap_or_else(|| cid.to_hex())
                    });
                    serde_json::json!({
                        "hex_id": s.id.to_hex(),
                        "qualified_name": s.qualified_name,
                        "kind": s.kind.as_str(),
                        "file": file_path,
                        "container": container_info,
                        "in_project": in_project,
                    })
                })
                .collect();

            // Heuristic: if exactly one candidate is in the same project,
            // auto-select it.  This handles the common case where a method
            // name (e.g. "Next") collides with a stdlib symbol.
            let in_project_candidates: Vec<&serde_json::Value> = candidates_with_meta
                .iter()
                .filter(|c| c["in_project"].as_bool().unwrap_or(false))
                .collect();

            if in_project_candidates.len() == 1 {
                if let Some(hex) = in_project_candidates[0]["hex_id"].as_str() {
                    if let Ok(target_id) = hex.parse::<SymbolId>() {
                        return self.trace_callers(&target_id, max_depth);
                    }
                }
            }

            let candidate_names: Vec<&str> = symbols
                .iter()
                .take(5)
                .map(|s| s.qualified_name.as_str())
                .collect();
            let suggested_hint = if in_project_candidates.is_empty() {
                " All candidates are from outside the project (stdlib/dependencies); inspect `in_project` field to choose manually.".to_string()
            } else {
                // in_project > 1 (== 1 already handled by auto-select above)
                format!(
                    " {} in-project candidates; inspect `in_project` field in detail to choose.",
                    in_project_candidates.len()
                )
            };
            let msg = format!(
                "Symbol '{}' matched {} symbols: [{}].{} Re-run with `symbol: \"<hex_id>\"` to trace a specific symbol.",
                name,
                symbols.len(),
                candidate_names.join(", "),
                suggested_hint,
            );
            return TraceQueryResponse {
                ok: true,
                kind: "trace_callers".to_string(),
                capability: None,
                partial_result: true,
                diagnostics: vec![
                    TraceDiagnostic::warning(&msg)
                        .with_code("multiple_matches")
                        .with_detail(
                            serde_json::to_string(&candidates_with_meta).unwrap_or_default(),
                        ),
                ],
                result: None,
                lazy_summary: None,
            };
        }

        // Exactly one match — trace it directly
        let target_id = &symbols[0].id;
        self.trace_callers(target_id, max_depth)
    }

    // ── Forward trace ──────────────────────────────────────────────────

    /// Trace the forward call chain from `source` to `target` by name.
    ///
    /// Resolves both names via indexed lookup.  When either name has multiple
    /// matches, returns a `multiple_matches` diagnostic listing all candidates
    /// so the Agent can re-query with specific hex IDs.
    pub fn trace_forward_by_name(
        &self,
        source_name: &str,
        target_name: &str,
        max_depth: usize,
    ) -> TraceQueryResponse<ForwardChain> {
        let source_symbols = match self.store.find_symbols_by_name(source_name) {
            Ok(s) => s,
            Err(e) => return TraceQueryResponse::err("trace_forward", &e.to_string()),
        };
        let target_symbols = match self.store.find_symbols_by_name(target_name) {
            Ok(s) => s,
            Err(e) => return TraceQueryResponse::err("trace_forward", &e.to_string()),
        };

        // Check for missing symbols
        if source_symbols.is_empty() {
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Source symbol '{source_name}' not found in index"
                ))
                .with_code("symbol_not_found"),
                None,
            );
        }
        if target_symbols.is_empty() {
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Target symbol '{target_name}' not found in index"
                ))
                .with_code("symbol_not_found"),
                None,
            );
        }

        // Multi-match disambiguation — same pattern as trace_callers_by_name
        if source_symbols.len() > 1 || target_symbols.len() > 1 {
            let mut parts: Vec<String> = Vec::new();
            if source_symbols.len() > 1 {
                let names: Vec<&str> = source_symbols
                    .iter()
                    .take(5)
                    .map(|s| s.qualified_name.as_str())
                    .collect();
                parts.push(format!(
                    "source '{}' matched {}: [{}]",
                    source_name,
                    source_symbols.len(),
                    names.join(", ")
                ));
            }
            if target_symbols.len() > 1 {
                let names: Vec<&str> = target_symbols
                    .iter()
                    .take(5)
                    .map(|s| s.qualified_name.as_str())
                    .collect();
                parts.push(format!(
                    "target '{}' matched {}: [{}]",
                    target_name,
                    target_symbols.len(),
                    names.join(", ")
                ));
            }
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Ambiguous names: {}. Re-run with `from` and `to` hex IDs to trace a specific path.",
                    parts.join("; ")
                ))
                .with_code("multiple_matches"),
                None,
            );
        }

        // Exactly one match each — trace forward
        self.trace_forward(&source_symbols[0].id, &target_symbols[0].id, max_depth)
    }

    /// Trace the forward call chain from `source_id` to `target_id`.
    ///
    /// Answers "how does A reach B?" by walking forward through call edges.
    pub fn trace_forward(
        &self,
        source_id: &SymbolId,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> TraceQueryResponse<ForwardChain> {
        let source_sym = self.store.find_symbol_by_id(source_id).ok().flatten();

        // Check symbol existence first — a missing symbol is a different class
        // of problem than an unsupported language.
        if source_sym.is_none() {
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Source symbol '{}' not found in the index. The symbol may need structural parsing — try narrowing scope with 'search' on the containing file, or run 'index' to ensure the project is fully indexed.",
                    source_id.to_hex()
                ))
                .with_code("symbol_not_found"),
                None,
            );
        }

        let source = source_sym.as_ref().unwrap();
        let cap = Some(LanguageCapabilityProfile::for_language(source.language));

        // Capability gate — same as trace_callers
        if !self.has_call_graph_cap(&cap) {
            let lang_name = source.language.as_str();
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Forward call-graph tracing is not available for {lang_name}. Consider using 'trace_caller_path' (reverse trace from target) or 'callgraph' for neighborhood exploration. If you believe call-graph edges should exist, verify that the '{lang_name}' feature is compiled into the Atlas binary (--features {lang_name}).",
                ))
                .with_code("unsupported_language")
                .with_detail(format!(
                    r#"{{"alternatives":["trace_caller_path","callgraph","context"],"language":"{lang_name}"}}"#
                )),
                cap,
            );
        }

        // Check that target exists too — the source may be fine but the
        // target was mis-specified or isn't indexed.
        let target_sym = self.store.find_symbol_by_id(target_id).ok().flatten();
        if target_sym.is_none() {
            return TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning(&format!(
                    "Target symbol '{}' not found in the index. The symbol may need structural parsing — try running 'context' with its qualified name to trigger on-demand parsing.",
                    target_id.to_hex()
                ))
                .with_code("target_not_found"),
                cap,
            );
        }

        match ForwardPathExplorer::explore(self.store.as_ref(), source_id, target_id, max_depth) {
            Ok(Some(mut chain)) => {
                self.enrich_forward_chain_steps(&mut chain);

                let mut diagnostics = build_boundary_diagnostics(step_boundaries(&chain.steps));

                // If truncated by depth (not by boundary), add a classified diagnostic
                if chain.truncated && diagnostics.is_empty() {
                    diagnostics.push(
                        TraceDiagnostic::warning(&format!(
                            "Forward path truncated: reached depth {} of max_depth={}. More callees may exist beyond this limit.",
                            chain.max_depth_reached, max_depth
                        ))
                        .with_code("max_depth_truncated"),
                    );
                }

                // Trail: actionable next steps for the Agent
                add_forward_chain_trail(&mut diagnostics, &chain);

                let partial = chain.truncated || !diagnostics.is_empty();
                TraceQueryResponse {
                    ok: true,
                    kind: "trace_forward".to_string(),
                    capability: cap,
                    partial_result: partial,
                    diagnostics,
                    result: Some(chain),
                    lazy_summary: None,
                }
            }
            Ok(None) => TraceQueryResponse::partial(
                "trace_forward",
                TraceDiagnostic::warning("No path found from source to target")
                    .with_code("no_path_found"),
                cap,
            ),
            Err(e) => TraceQueryResponse::err("trace_forward", &format!("{e}")),
        }
    }

    // ── File resolution ────────────────────────────────────────────────

    /// Resolve a user-facing file path to a [`FileId`], or `Ok(None)` if not
    /// found. Uses indexed `files.path` lookup — O(log n) for exact match,
    /// O(suffix matches) for suffix scan.
    pub fn resolve_file_id_with_root(
        &self,
        project_root: &Path,
        file_path: &str,
    ) -> anyhow::Result<Option<FileId>> {
        let clean = file_path.trim_start_matches("./").trim_start_matches('/');
        self.store.resolve_file_id(project_root, clean)
    }

    // ── Internal ───────────────────────────────────────────────────────

    /// Populate [`Evidence`] on every step of a [`TracePath`].
    ///
    /// Resolves file paths from the store and looks up data node names.
    /// This provides human-readable context for agent/AI consumers without
    /// additional database queries.
    fn enrich_trace_path_steps(&self, path: &mut TracePath) {
        for step in &mut path.steps {
            step.evidence = self.build_step_evidence_data(&step.file_id, &step.from_node_id);
        }
    }

    /// Populate [`Evidence`] on every step of a [`CallerChain`].
    ///
    /// Resolves file paths, caller/callee symbol names, and extracts
    /// multi-line callsite context from the source file.  The evidence
    /// snippet now shows ~3 lines around the actual call site rather than
    /// just the caller's first line.
    fn enrich_caller_chain_steps(&self, chain: &mut CallerChain) {
        for step in &mut chain.steps {
            // Evidence: use callsite location for the snippet (not caller's first line).
            let callsite_line = step.range.as_ref().map(|r| r.start_line);
            step.evidence =
                self.build_step_evidence_with_context(&step.file_id, &step.caller, callsite_line);

            // Caller snippet: the line where the call is made, with context.
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(&step.caller) {
                if let Some(ref fp) = self.resolve_file_path(&sym.file_id) {
                    if let Some(cs_line) = callsite_line {
                        step.caller_snippet = self.extract_context_snippet(fp, cs_line, 3);
                    }
                }
            }
            // Callee snippet: first line of the callee definition (signature).
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(&step.callee) {
                if let Some(ref fp) = self.resolve_file_path(&sym.file_id) {
                    step.callee_snippet = self.extract_snippet(fp, sym.range.start_line);
                }
            }
        }
    }

    /// Populate [`Evidence`] on every step of a [`ForwardChain`].
    fn enrich_forward_chain_steps(&self, chain: &mut ForwardChain) {
        for step in &mut chain.steps {
            step.evidence = self.build_step_evidence_symbol(&step.file_id, &step.caller);
            // Also populate caller/callee snippets for forward trace
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(&step.caller) {
                let file_path = self.resolve_file_path(&sym.file_id);
                if let Some(ref fp) = file_path {
                    step.caller_snippet = self.extract_snippet(fp, sym.range.start_line);
                }
            }
            if let Ok(Some(sym)) = self.store.find_symbol_by_id(&step.callee) {
                let file_path = self.resolve_file_path(&sym.file_id);
                if let Some(ref fp) = file_path {
                    step.callee_snippet = self.extract_snippet(fp, sym.range.start_line);
                }
            }
        }
    }

    /// Build an [`Evidence`] from a file_id and a data node id.
    fn build_step_evidence_data(
        &self,
        file_id: &FileId,
        node_id: &types::ids::DataNodeId,
    ) -> Option<Evidence> {
        let file_path = self.resolve_file_path(file_id)?;
        let data_node = self.store.get_data_node(node_id).ok().flatten();
        let symbol_name = data_node.as_ref().and_then(|n| n.name.clone());
        let line = data_node.as_ref().map(|n| n.range.start_line);
        let snippet = line.and_then(|l| self.extract_snippet(&file_path, l));
        Some(Evidence {
            file_path,
            snippet,
            symbol_name,
        })
    }

    /// Build an [`Evidence`] with a multi-line context snippet at a specific
    /// callsite line (rather than the caller's first line).
    fn build_step_evidence_with_context(
        &self,
        file_id: &FileId,
        symbol_id: &SymbolId,
        callsite_line: Option<u32>,
    ) -> Option<Evidence> {
        let file_path = self.resolve_file_path(file_id)?;
        let symbol = self.store.find_symbol_by_id(symbol_id).ok().flatten();
        let symbol_name = symbol.as_ref().map(|s| s.name.clone());
        // Use the callsite line for the snippet — this is the actual line
        // where the call happens, far more useful than the caller's first line.
        let snippet = match callsite_line {
            Some(line) => self.extract_context_snippet(&file_path, line, 3),
            None => None,
        };
        Some(Evidence {
            file_path,
            snippet,
            symbol_name,
        })
    }

    /// Build an [`Evidence`] from a file_id and a symbol id.
    fn build_step_evidence_symbol(
        &self,
        file_id: &FileId,
        symbol_id: &SymbolId,
    ) -> Option<Evidence> {
        let file_path = self.resolve_file_path(file_id)?;
        let symbol = self.store.find_symbol_by_id(symbol_id).ok().flatten();
        let symbol_name = symbol.as_ref().map(|s| s.name.clone());
        let line = symbol.as_ref().map(|s| s.range.start_line);
        let snippet = line.and_then(|l| self.extract_snippet(&file_path, l));
        Some(Evidence {
            file_path,
            snippet,
            symbol_name,
        })
    }

    /// Extract a multi-line context snippet around the given line.
    ///
    /// Returns up to `context_lines` lines before and after the target line,
    /// joined with newlines.  The target line is included.
    fn extract_context_snippet(
        &self,
        file_path: &str,
        line_0based: u32,
        context_lines: usize,
    ) -> Option<String> {
        let root = self.project_root.as_ref()?;
        let canonical_root = self.canonical_root.as_ref()?;
        let full_path = root.join(file_path);
        let canonical = full_path.canonicalize().ok()?;
        if !canonical.starts_with(canonical_root) {
            return None;
        }
        // Check file-content cache to avoid repeated disk reads.
        let content = {
            let mut cache = self.file_cache.borrow_mut();
            if let Some(cached) = cache.get(&canonical) {
                cached.clone()
            } else {
                let text = std::fs::read_to_string(&canonical).ok()?;
                cache.insert(canonical.clone(), text.clone());
                text
            }
        };
        let all_lines: Vec<&str> = content.lines().collect();
        let center = line_0based as usize;
        if center >= all_lines.len() {
            return None;
        }
        let start = center.saturating_sub(context_lines);
        let end = (center + context_lines + 1).min(all_lines.len());
        let lines: Vec<String> = all_lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let actual_line = start + i;
                let marker = if actual_line == center { ">" } else { " " };
                format!("{marker} {l}")
            })
            .collect();
        Some(lines.join("\n"))
    }

    /// Extract a one-line snippet from the source file at the given 0-based line.
    ///
    /// Reads the file from `project_root/file_path` and returns the line
    /// trimmed.  Returns `None` if the project root is not set, the file
    /// cannot be read, or the line is out of bounds.
    fn extract_snippet(&self, file_path: &str, line_0based: u32) -> Option<String> {
        let root = self.project_root.as_ref()?;
        let canonical_root = self.canonical_root.as_ref()?;
        let full_path = root.join(file_path);
        let canonical = full_path.canonicalize().ok()?;
        if !canonical.starts_with(canonical_root) {
            return None;
        }
        // Check file-content cache to avoid repeated disk reads.
        let content = {
            let mut cache = self.file_cache.borrow_mut();
            if let Some(cached) = cache.get(&canonical) {
                cached.clone()
            } else {
                let text = std::fs::read_to_string(&canonical).ok()?;
                cache.insert(canonical.clone(), text.clone());
                text
            }
        };
        let line_idx = line_0based as usize;
        content.lines().nth(line_idx).map(|l| l.trim().to_string())
    }

    /// Check whether a file path belongs to the current project.
    ///
    /// Used for same-project heuristic: when multiple symbols share a name
    /// (e.g. `Context.Next` vs `bufio.Scanner.Next`), prefer the one whose
    /// source file lives under the project root.
    fn is_file_in_project(&self, file_path: &str) -> bool {
        let Some(root) = self.project_root.as_ref() else {
            return false;
        };
        let Some(canonical_root) = self.canonical_root.as_ref() else {
            return false;
        };
        let Ok(full_path) = root.join(file_path).canonicalize() else {
            return false;
        };
        full_path.starts_with(canonical_root)
    }

    /// Resolve a file path from a file_id.
    fn resolve_file_path(&self, file_id: &FileId) -> Option<String> {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|info| info.path)
    }

    /// Look up the language capability profile for a file.
    fn resolve_capability(&self, file_id: &FileId) -> Option<LanguageCapabilityProfile> {
        self.store
            .get_file(file_id)
            .ok()
            .flatten()
            .map(|fi| LanguageCapabilityProfile::for_language(fi.language))
    }

    /// Check whether the given capability profile supports call-graph traversal.
    fn has_call_graph_cap(&self, cap: &Option<LanguageCapabilityProfile>) -> bool {
        cap.as_ref()
            .and_then(|c| c.features.as_ref())
            .map(|f| f.call_graph.is_supported())
            .unwrap_or_else(|| {
                cap.as_ref()
                    .map(|c| c.supported_features.contains(&"call_graph".to_string()))
                    .unwrap_or(false)
            })
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

/// Build [`TraceDiagnostic`] entries from [`BoundaryMarker`]s found in trace steps.
///
/// Each boundary marker produces a warning diagnostic with a machine-readable
/// code and a JSON-serialised detail payload so MCP consumers can render
/// structured boundary information.
fn build_boundary_diagnostics<'a>(
    boundaries: impl IntoIterator<Item = &'a BoundaryMarker>,
) -> Vec<TraceDiagnostic> {
    boundaries
        .into_iter()
        .map(|marker| {
            let code = match &marker.kind {
                BoundaryKind::CallbackRegistration { .. } => "callback_registration_boundary",
                BoundaryKind::FunctionPointer { .. } => "dynamic_dispatch_boundary",
                BoundaryKind::VirtualDispatch { .. } => "virtual_dispatch_boundary",
                BoundaryKind::DynamicMethodCall { .. } => "dynamic_method_boundary",
                _ => "boundary",
            };
            let detail_json = serde_json::to_string(marker).unwrap_or_else(|_| "{}".into());
            TraceDiagnostic::warning(&marker.message)
                .with_code(code)
                .with_detail(detail_json)
        })
        .collect()
}

/// Collect [`BoundaryMarker`] references from steps that have one.
fn step_boundaries<T>(steps: &[T]) -> impl Iterator<Item = &BoundaryMarker>
where
    T: HasBoundary,
{
    steps.iter().filter_map(|s| s.boundary())
}

/// Trait abstracting over step types that carry an optional [`BoundaryMarker`].
trait HasBoundary {
    fn boundary(&self) -> Option<&BoundaryMarker>;
}

impl HasBoundary for types::caller_path::CallerChainStep {
    fn boundary(&self) -> Option<&BoundaryMarker> {
        self.boundary.as_ref()
    }
}

impl HasBoundary for types::caller_path::ForwardChainStep {
    fn boundary(&self) -> Option<&BoundaryMarker> {
        self.boundary.as_ref()
    }
}

// ── Trail diagnostics: next-step guidance for Agent consumers ──────────────

/// Append actionable next-step diagnostics to a caller-chain response.
fn add_caller_chain_trail(diagnostics: &mut Vec<TraceDiagnostic>, chain: &CallerChain) {
    if chain.steps.is_empty() {
        return;
    }
    let root_name = &chain.root.name;
    let target_name = &chain.target.qualified_name;
    let hop_count = chain.steps.len();
    diagnostics.push(
        TraceDiagnostic::info(&format!(
            "Trail: {}→{} ({hop_count} hops). Next: `context` with `symbol: \"{}\"` for full source of root; `trace_caller_path` with `symbol_name: \"{}\"` to trace beyond root; `trace_forward` from root hex to trace the forward chain.",
            root_name, target_name,
            chain.root.qualified_name,
            root_name,
        ))
        .with_code("next_steps"),
    );
}

/// Append actionable next-step diagnostics to a forward-chain response.
fn add_forward_chain_trail(diagnostics: &mut Vec<TraceDiagnostic>, chain: &ForwardChain) {
    if chain.steps.is_empty() {
        return;
    }
    let source_name = &chain.source.name;
    let target_name = &chain.target.qualified_name;
    let hop_count = chain.steps.len();
    diagnostics.push(
        TraceDiagnostic::info(&format!(
            "Trail: {}→{} ({hop_count} hops). Next: `context` with `symbol: \"{}\"` for full target source; `callgraph` from target to see its callees.",
            source_name, target_name,
            chain.target.qualified_name,
        ))
        .with_code("next_steps"),
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_ok() {
        let resp = TraceQueryResponse::ok("trace_point", "some value", None);
        assert!(resp.ok);
        assert_eq!(resp.kind, "trace_point");
        assert!(!resp.partial_result);
        assert!(resp.diagnostics.is_empty());
        assert_eq!(resp.result, Some("some value"));
    }

    #[test]
    fn response_partial() {
        let diag = TraceDiagnostic::warning("no data node").with_code("no_data_node");
        let resp: TraceQueryResponse<()> =
            TraceQueryResponse::partial("trace_variable", diag.clone(), None);
        assert!(resp.ok);
        assert!(resp.partial_result);
        assert_eq!(resp.diagnostics.len(), 1);
        assert_eq!(resp.diagnostics[0].code, Some("no_data_node".into()));
        assert!(resp.result.is_none());
    }

    #[test]
    fn response_err() {
        let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_variable", "disk error");
        assert!(!resp.ok);
        assert_eq!(resp.kind, "trace_variable");
        assert_eq!(resp.diagnostics.len(), 1);
        assert!(resp.result.is_none());
    }

    #[test]
    fn response_serializes_fields_present() {
        let resp = TraceQueryResponse::ok("trace_point", "hello", None);
        let json = serde_json::to_string(&resp).unwrap();
        // All 6 mandatory envelope fields must appear even when empty/default/null.
        assert!(json.contains(r#""ok""#));
        assert!(json.contains(r#""kind""#));
        assert!(
            json.contains(r#""capability""#),
            "capability field must always be present"
        );
        assert!(json.contains(r#""partial_result""#));
        assert!(json.contains(r#""diagnostics""#));
        assert!(json.contains(r#""result""#));
    }

    #[test]
    fn engine_constructs() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let engine = TraceEngine::new(store);
        // Just verify it doesn't crash on construction.
        let _ = &engine;
    }

    #[test]
    fn response_err_has_capability_field() {
        let resp: TraceQueryResponse<()> = TraceQueryResponse::err("trace_point", "file not found");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains(r#""capability""#),
            "err response must still contain capability field (null)"
        );
    }

    #[test]
    fn response_partial_has_capability_field() {
        let diag = TraceDiagnostic::warning("unsupported").with_code("unsupported_language");
        let resp: TraceQueryResponse<()> =
            TraceQueryResponse::partial("trace_variable", diag, None);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains(r#""capability""#),
            "partial response must still contain capability field (null)"
        );
    }

    #[test]
    fn boundary_diagnostics_from_callback_registration() {
        let marker = BoundaryMarker {
            kind: BoundaryKind::CallbackRegistration {
                registrant: "set_callback".into(),
                callback: "on_event".into(),
            },
            message: "boundary hit".into(),
            suggestion: "explore".into(),
            bridge_target: Some("abc123".into()),
        };
        let diagnostics = build_boundary_diagnostics([&marker]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code.as_deref(),
            Some("callback_registration_boundary")
        );
        assert!(
            diagnostics[0].detail.is_some(),
            "detail must contain serialized marker"
        );
        // detail should be valid JSON representing the marker
        let d: BoundaryMarker =
            serde_json::from_str(diagnostics[0].detail.as_ref().unwrap()).unwrap();
        assert!(matches!(d.kind, BoundaryKind::CallbackRegistration { .. }));
    }

    #[test]
    fn boundary_diagnostics_empty_for_no_markers() {
        let diagnostics = build_boundary_diagnostics(std::iter::empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn boundary_marker_json_roundtrip() {
        let marker = BoundaryMarker {
            kind: BoundaryKind::FunctionPointer {
                pointer_name: "fn_ptr".into(),
            },
            message: "function pointer detected".into(),
            suggestion: "resolve at runtime".into(),
            bridge_target: None,
        };
        let json = serde_json::to_string(&marker).unwrap();
        let back: BoundaryMarker = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.kind, BoundaryKind::FunctionPointer { .. }));
        assert_eq!(back.message, marker.message);
        assert_eq!(back.suggestion, marker.suggestion);
        assert!(back.bridge_target.is_none());
    }
}
