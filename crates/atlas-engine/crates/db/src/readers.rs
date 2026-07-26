//! Reader traits for layered data access.
//!
//! Each trait provides a bounded, query-time interface to one data domain.
//! All four traits are implemented on [`Store`](crate::Store) and delegate
//! to the store's inherent methods via UFCS.
//!
//! # Layers
//!
//! | Layer | Trait | What it reads |
//! |-------|-------|---------------|
//! | Symbol | `SymbolReader` | symbols, references, scope, imports, edges |
//! | Dataflow | `DataflowReader` | data nodes, dataflow edges |
//! | Call graph | `CallGraphReader` | callsites, bindings, CFG, function-level edges |
//! | File | `FileReader` | file info, path resolution, metadata |

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use types::*;

// ── Symbol Reader ──────────────────────────────────────────────────────────

/// Read-only access to symbols, references, scopes, imports, and edges.
pub trait SymbolReader {
    fn find_symbol_by_id(&self, id: &SymbolId) -> Result<Option<SymbolDef>>;
    fn find_symbols_by_file(&self, file_id: &FileId) -> Result<Vec<SymbolDef>>;
    fn search_symbols(&self, query: &str) -> Result<Vec<SymbolDef>>;
    fn search_symbols_with_limit(
        &self,
        query: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> Result<Vec<SymbolDef>>;
    fn search_symbols_by_name_like(
        &self,
        pattern: &str,
        language: Option<&Language>,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> Result<Vec<SymbolDef>>;
    fn count_symbols(&self) -> Result<usize>;
    fn find_symbols_by_qname(&self, qname: &str) -> Result<Vec<SymbolDef>>;
    fn get_all_symbols(&self) -> Result<Vec<SymbolDef>>;

    /// Find symbols by exact name match (uses index on `symbols.name`).
    fn find_symbols_by_name(&self, name: &str) -> Result<Vec<SymbolDef>>;

    fn find_references_by_file(&self, file_id: &FileId) -> Result<Vec<ReferenceUse>>;
    fn find_latest_visible_reference_target(
        &self,
        reference_id: &ReferenceId,
    ) -> Result<Option<SymbolId>>;
    fn find_scopes_by_file(&self, file_id: &FileId) -> Result<Vec<ScopeDef>>;
    fn find_imports_by_file(&self, file_id: &FileId) -> Result<Vec<ImportDef>>;

    fn find_edges_by_source(&self, source: &SymbolId) -> Result<Vec<RawEdge>>;
    fn find_edges_by_target(&self, target: &SymbolId) -> Result<Vec<RawEdge>>;
    fn get_all_edges(&self) -> Result<Vec<RawEdge>>;
}

// ── Dataflow Reader ────────────────────────────────────────────────────────

/// Read-only access to data nodes and dataflow edges.
pub trait DataflowReader {
    fn get_data_node(&self, id: &DataNodeId) -> Result<Option<DataNode>>;
    /// Batch lookup of multiple data nodes by ID.
    fn get_data_nodes(&self, ids: &[DataNodeId]) -> Result<HashMap<DataNodeId, DataNode>>;
    fn find_data_nodes_by_file(&self, file_id: &FileId) -> Result<Vec<DataNode>>;
    /// Quick check: does this file have any data_nodes?
    fn has_dataflow_for_file(&self, file_id: &FileId) -> Result<bool>;
    fn find_data_nodes_by_function(&self, function_id: &SymbolId) -> Result<Vec<DataNode>>;
    fn find_data_nodes_by_callsite(&self, callsite_id: &CallsiteId) -> Result<Vec<DataNode>>;
    fn find_dataflow_edges_by_source(&self, source: &DataNodeId) -> Result<Vec<DataFlowEdge>>;
    fn find_dataflow_edges_by_target(&self, target: &DataNodeId) -> Result<Vec<DataFlowEdge>>;
    /// Batch lookup of dataflow edges with sources in the given set.
    fn find_dataflow_edges_by_sources(&self, sources: &[DataNodeId]) -> Result<Vec<DataFlowEdge>>;
    /// Find dataflow edges whose source nodes belong to the given function.
    fn find_dataflow_edges_by_function(&self, function_id: &SymbolId) -> Result<Vec<DataFlowEdge>>;
    /// Find dataflow edges whose source nodes belong to the given file.
    fn find_dataflow_edges_by_file(&self, file_id: &FileId) -> Result<Vec<DataFlowEdge>>;
}

// ── Call Graph Reader ──────────────────────────────────────────────────────

/// Read-only access to callsites, bindings, and CFG.
pub trait CallGraphReader {
    fn find_callsites_by_file(&self, file_id: &FileId) -> Result<Vec<Callsite>>;
    fn find_callsites_by_name_and_receiver(
        &self,
        name: &str,
        receiver: &str,
        language: Language,
    ) -> Result<Vec<Callsite>>;
    fn find_resolved_callsites_by_callee(&self, callee: &SymbolId)
    -> Result<Vec<ResolvedCallsite>>;
    fn find_callsites_by_id(&self, id: &CallsiteId) -> Result<Vec<Callsite>>;
    fn find_callsite_by_reference_id(&self, reference_id: &ReferenceId)
    -> Result<Option<Callsite>>;
    fn find_resolved_callsites_by_id(&self, id: &CallsiteId) -> Result<Vec<ResolvedCallsite>>;
    fn find_resolved_callsite_by_reference_id(
        &self,
        reference_id: &ReferenceId,
    ) -> Result<Option<ResolvedCallsite>>;

    fn find_bindings_by_file(&self, file_id: &FileId) -> Result<Vec<BindingDef>>;
    fn find_bindings_by_function(&self, function_id: &SymbolId) -> Result<Vec<BindingDef>>;
    fn find_binding_uses_by_file(&self, file_id: &FileId) -> Result<Vec<BindingUse>>;
    fn find_binding_uses_by_binding(&self, binding_id: &BindingId) -> Result<Vec<BindingUse>>;

    fn find_cfg_nodes_by_function(&self, function_id: &SymbolId) -> Result<Vec<CfgNode>>;
    fn find_cfg_edges_by_source(&self, source: &CfgNodeId) -> Result<Vec<CfgEdge>>;
}

// ── Summary Reader ──────────────────────────────────────────────────────────

/// Read-only access to persisted function summary tables (Schema v2).
///
/// Used by [`CrossFunctionBridge`] to perform O(1) cross-function
/// dataflow bridging without runtime BFS.
pub trait SummaryReader {
    /// Query `summary_param_reaches` for a given parameter node.
    fn query_param_reaches(
        &self,
        param_id: &DataNodeId,
    ) -> Result<Vec<crate::summary::ParamReachRow>>;
    /// Query `summary_return_sources` for a given return node.
    fn query_return_sources(
        &self,
        return_id: &DataNodeId,
    ) -> Result<Vec<crate::summary::ReturnSourceRow>>;
    /// Query `summary_call_arg_sources` for a given call-argument node.
    fn query_call_arg_sources(
        &self,
        arg_node_id: &DataNodeId,
    ) -> Result<Vec<crate::summary::CallArgSourceRow>>;
}

// ── Composite Traits ────────────────────────────────────────────────────────

/// Composite reader bound for trace/analysis operations that need
/// symbol, dataflow, call-graph, and summary access in a single trait object.
///
/// This is implemented for any type that satisfies all four component
/// reader traits, including [`Store`](crate::Store).
pub trait TraceStore: SymbolReader + DataflowReader + CallGraphReader + SummaryReader {}

impl<T: SymbolReader + DataflowReader + CallGraphReader + SummaryReader> TraceStore for T {}

// ── File Reader ────────────────────────────────────────────────────────────

/// Read-only access to file metadata and path resolution.
pub trait FileReader {
    fn get_file(&self, file_id: &FileId) -> Result<Option<FileInfo>>;
    fn list_files(&self) -> Result<Vec<FileInfo>>;

    /// Find files whose path starts with the given prefix.
    ///
    /// Uses a SQL `LIKE` query to avoid loading all files into memory.
    fn find_files_by_path_prefix(&self, prefix: &str) -> Result<Vec<FileInfo>>;

    /// Resolve a user-facing path (relative or suffix) to a [`FileId`]
    /// using indexed lookups on `files.path`.
    fn resolve_file_id(&self, root: &Path, rel_path: &str) -> Result<Option<FileId>>;
    fn get_metadata(&self, key: &str) -> Result<Option<String>>;
}
