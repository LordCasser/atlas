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
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;

use crate::trace::{CallerPathExplorer, Locator, Slicer};
use atlas_db::Store;
use atlas_types::caller_path::CallerChain;
use atlas_types::capability::{CapabilityLevel, FeatureSupport, LanguageCapabilityProfile};
use atlas_types::ids::{FileId, SymbolId};
use atlas_types::trace::{Evidence, TraceDiagnostic, TracePath, TracePoint};

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
}

impl TraceEngine {
    /// Create a new trace engine backed by the given store (no project root).
    ///
    /// Without a project root, [`Evidence.snippet`] will always be `None`.
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            project_root: None,
        }
    }

    /// Create a new trace engine with a project root for snippet extraction.
    ///
    /// When a project root is provided, [`Evidence.snippet`] will be populated
    /// by reading the relevant source line from disk.
    pub fn new_with_root(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root: Some(project_root),
        }
    }

    /// Create a new trace engine with a [`Workspace`] for snippet extraction.
    ///
    /// Convenience wrapper around [`Self::new_with_root`] that uses the
    /// workspace's canonical project root.
    pub fn new_with_workspace(store: Arc<Store>, workspace: &atlas_workspace::Workspace) -> Self {
        Self {
            store,
            project_root: Some(workspace.root().to_path_buf()),
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
            Err(e) => TraceQueryResponse::err("trace_point", &format!("{}", e)),
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
                    "Dataflow not supported for this language ({})",
                    reason
                ))
                .with_code("unsupported_language"),
                cap,
            );
        }

        let sink = match Locator::locate(self.store.as_ref(), file_id, line, column) {
            Ok(p) => p,
            Err(e) => return TraceQueryResponse::err("trace_variable", &format!("{}", e)),
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
            Err(e) => TraceQueryResponse::err("trace_variable", &format!("{}", e)),
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

        // Capability gate: need call_graph — prefer FeatureMatrix, fallback to string list
        let has_call_graph = cap
            .as_ref()
            .and_then(|c| c.features.as_ref())
            .map(|f| f.call_graph.is_supported())
            .unwrap_or_else(|| {
                cap.as_ref()
                    .map(|c| c.supported_features.contains(&"call_graph".to_string()))
                    .unwrap_or(false)
            });
        if !has_call_graph {
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
                let partial = chain.truncated;
                let diagnostics = if partial {
                    vec![
                        TraceDiagnostic::warning(&format!(
                            "Caller path truncated at max_depth={} (reached depth {})",
                            max_depth, chain.max_depth_reached
                        ))
                        .with_code("max_depth_truncated"),
                    ]
                } else {
                    vec![]
                };
                TraceQueryResponse {
                    ok: true,
                    kind: "trace_callers".to_string(),
                    capability: cap,
                    partial_result: partial,
                    diagnostics,
                    result: Some(chain),
                }
            }
            Ok(None) => TraceQueryResponse::partial(
                "trace_callers",
                TraceDiagnostic::warning("No callers found — this is a root/top-level function")
                    .with_code("no_callers"),
                cap,
            ),
            Err(e) => TraceQueryResponse::err("trace_callers", &format!("{}", e)),
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
    pub fn trace_callers_by_name(
        &self,
        name: &str,
        max_depth: usize,
    ) -> TraceQueryResponse<CallerChain> {
        let ids = match self.find_symbol_ids_by_name(name) {
            Ok(ids) => ids,
            Err(e) => return TraceQueryResponse::err("trace_callers", &e.to_string()),
        };
        match ids.first() {
            Some(id) => self.trace_callers(id, max_depth),
            None => TraceQueryResponse::partial(
                "trace_callers",
                TraceDiagnostic::warning(&format!("Symbol '{}' not found in index", name))
                    .with_code("symbol_not_found"),
                None,
            ),
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
    /// Resolves file paths and caller symbol names from the store.
    fn enrich_caller_chain_steps(&self, chain: &mut CallerChain) {
        for step in &mut chain.steps {
            step.evidence = self.build_step_evidence_symbol(&step.file_id, &step.caller);
        }
    }

    /// Build an [`Evidence`] from a file_id and a data node id.
    fn build_step_evidence_data(
        &self,
        file_id: &FileId,
        node_id: &atlas_types::ids::DataNodeId,
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

    /// Extract a one-line snippet from the source file at the given 0-based line.
    ///
    /// Reads the file from `project_root/file_path` and returns the line
    /// trimmed.  Returns `None` if the project root is not set, the file
    /// cannot be read, or the line is out of bounds.
    fn extract_snippet(&self, file_path: &str, line_0based: u32) -> Option<String> {
        let root = self.project_root.as_ref()?;
        let full_path = root.join(file_path);
        let canonical = full_path.canonicalize().ok()?;
        // Canonicalize root too — macOS symlinks /var→/private/var
        let canonical_root = root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        let content = std::fs::read_to_string(&canonical).ok()?;
        let line_idx = line_0based as usize;
        content.lines().nth(line_idx).map(|l| l.trim().to_string())
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
}
