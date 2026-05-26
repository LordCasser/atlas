//! Context building for AI: assemble relevant codebase context.
//!
//! The context builder answers questions like:
//!   - "What symbols are directly adjacent to X?"
//!   - "What does the call chain for X look like?"
//!   - "Which files import/depend on X?"

use db::Store;
use graph::GraphEngine;
use std::sync::{Arc, RwLock};
use types::{FileId, SymbolDef, SymbolId};

mod builder;

pub use builder::{CalleeDetail, CallerDetail, ContextSlice, ContextView, SourceSnippet};

/// AI context builder: constructs symbol-rich context from the codebase graph.
pub struct ContextBuilder {
    store: Arc<Store>,
    graph: RwLock<Arc<GraphEngine>>,
}

impl ContextBuilder {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self {
            store,
            graph: RwLock::new(graph),
        }
    }

    /// Replace the internal graph snapshot with a fresh one.
    pub fn refresh_graph(&self, graph: Arc<GraphEngine>) {
        *self.graph.write().unwrap_or_else(|e| e.into_inner()) = graph;
    }

    /// Acquire a read-lock on the graph, recovering from poison.
    fn graph(&self) -> std::sync::RwLockReadGuard<'_, Arc<GraphEngine>> {
        self.graph.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Return a clone of the current graph snapshot.
    pub fn graph_snapshot(&self) -> Arc<GraphEngine> {
        Arc::clone(&*self.graph())
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Build a context view around a specific symbol: its callers, callees,
    /// containing scope, adjacent symbols, and source snippets.
    ///
    /// The graph snapshot is locked once and held for the entire operation,
    /// guaranteeing consistent results even if `refresh_graph()` is called
    /// concurrently from another request.
    pub fn build_context_for_symbol(&self, symbol_id: &SymbolId) -> anyhow::Result<ContextView> {
        let sym = self.store.find_symbol_by_id(symbol_id)?;
        let Some(ref sym) = sym else {
            anyhow::bail!("symbol not found: {}", symbol_id);
        };

        // Single graph lock for consistent snapshot across all queries
        let g = self.graph();
        let caller_view = g.callers(symbol_id);
        let callee_view = g.callees(symbol_id);
        let file_id = sym.file_id;

        // Peers: symbols in the same file
        let file_symbols = self.store.find_symbols_by_file(&file_id)?;

        // Importers are symbols; dependencies are files
        let importer_symbols = g.importers(&file_id);
        let dep_files = g.dependencies(&file_id);

        // Resolve node indices to SymbolIds while still holding the lock
        let resolved_callers = g.resolve_node_ids(&caller_view.callers);
        let resolved_callees = g.resolve_node_ids(&callee_view.callees);
        drop(g); // release graph lock before store queries

        let caller_syms = resolve_symbols(&self.store, &resolved_callers)?;
        let callee_syms = resolve_symbols(&self.store, &resolved_callees)?;

        // Build detailed caller/callee info with source snippets
        let caller_details = self.build_caller_details(symbol_id, &resolved_callers, &caller_syms)?;
        let callee_details = self.build_callee_details(symbol_id, &resolved_callees, &callee_syms)?;

        let subject_source = self.read_source_snippet(sym);

        let view = ContextView {
            subject: sym.clone(),
            subject_source,
            callers: caller_syms,
            callees: callee_syms,
            caller_details,
            callee_details,
            file_peers: file_symbols,
            importers: resolve_symbols_to_paths(&self.store, &importer_symbols)?,
            dependencies: resolve_files(&self.store, &dep_files)?,
        };

        Ok(view)
    }

    /// Build a context slice — lightweight neighbor-only view (no full file listing).
    pub fn build_context_slice(&self, symbol_id: &SymbolId) -> anyhow::Result<ContextSlice> {
        let sym = self.store.find_symbol_by_id(symbol_id)?;
        let Some(sym) = sym else {
            anyhow::bail!("symbol not found: {}", symbol_id);
        };

        // Single graph lock for consistent snapshot
        let g = self.graph();
        let caller_view = g.callers(symbol_id);
        let callee_view = g.callees(symbol_id);
        let resolved_callers = g.resolve_node_ids(&caller_view.callers);
        let resolved_callees = g.resolve_node_ids(&callee_view.callees);
        drop(g); // release graph lock before store queries

        let slice = ContextSlice {
            subject: sym,
            callers: resolve_symbols(&self.store, &resolved_callers)?,
            callees: resolve_symbols(&self.store, &resolved_callees)?,
        };

        Ok(slice)
    }

    /// Build context from a search query — finds the top match, then builds context.
    pub fn build_context_for_query(&self, query: &str) -> anyhow::Result<Option<ContextView>> {
        let results = self.store.search_symbols(query)?;
        if let Some(top) = results.first() {
            self.build_context_for_symbol(&top.id).map(Some)
        } else {
            Ok(None)
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn build_caller_details(
        &self,
        subject_id: &SymbolId,
        _resolved_callers: &[SymbolId],
        caller_syms: &[SymbolDef],
    ) -> anyhow::Result<Vec<CallerDetail>> {
        // For each caller, find the Calls edge connecting it to the subject.
        let edges = self.store.find_edges_by_target(subject_id)?;
        let mut details = Vec::new();
        for caller_sym in caller_syms {
            // Find the edge from this caller to the subject
            let edge = edges.iter().find(|e| e.source == caller_sym.id
                && (e.kind == types::EdgeKind::Calls
                    || e.kind == types::EdgeKind::RegistersCallback));
            let line = edge
                .and_then(|e| e.location.as_ref())
                .map(|r| r.start_line)
                .unwrap_or(caller_sym.range.start_line);
            details.push(CallerDetail {
                symbol: caller_sym.clone(),
                callsite_line: line,
                callsite_snippet: String::new(),
                edge_kind: edge.map(|e| e.kind.clone()).unwrap_or(types::EdgeKind::Calls),
            });
        }
        Ok(details)
    }

    fn build_callee_details(
        &self,
        subject_id: &SymbolId,
        _resolved_callees: &[SymbolId],
        callee_syms: &[SymbolDef],
    ) -> anyhow::Result<Vec<CalleeDetail>> {
        let edges = self.store.find_edges_by_source(subject_id)?;
        let mut details = Vec::new();
        for callee_sym in callee_syms {
            let edge = edges.iter().find(|e| e.target == callee_sym.id
                && (e.kind == types::EdgeKind::Calls
                    || e.kind == types::EdgeKind::RegistersCallback));
            let line = edge
                .and_then(|e| e.location.as_ref())
                .map(|r| r.start_line)
                .unwrap_or(callee_sym.range.start_line);
            details.push(CalleeDetail {
                symbol: callee_sym.clone(),
                callsite_line: line,
                callsite_snippet: String::new(),
                edge_kind: edge.map(|e| e.kind.clone()).unwrap_or(types::EdgeKind::Calls),
                callee_signature: callee_sym.signature.clone(),
            });
        }
        Ok(details)
    }

    /// Read the subject symbol's source code from disk.
    ///
    /// ContextBuilder does not hold a project_root; source snippet reading
    /// is delegated to the MCP tool layer which can resolve file paths.
    fn read_source_snippet(&self, _sym: &SymbolDef) -> Option<SourceSnippet> {
        None
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn resolve_symbols(store: &Store, ids: &[SymbolId]) -> anyhow::Result<Vec<SymbolDef>> {
    store.find_symbols_by_ids(ids)
}

fn resolve_files(store: &Store, ids: &[FileId]) -> anyhow::Result<Vec<String>> {
    let mut paths = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(info) = store.get_file(id)? {
            paths.push(info.path);
        }
    }
    Ok(paths)
}

fn resolve_symbols_to_paths(store: &Store, ids: &[SymbolId]) -> anyhow::Result<Vec<String>> {
    let symbols = store.find_symbols_by_ids(ids)?;
    let mut paths = std::collections::BTreeSet::new();
    for sym in &symbols {
        if let Some(info) = store.get_file(&sym.file_id)? {
            paths.insert(info.path.clone());
        }
    }
    Ok(paths.into_iter().collect())
}
