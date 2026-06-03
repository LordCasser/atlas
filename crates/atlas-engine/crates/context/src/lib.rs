//! Context building for AI: assemble relevant codebase context.
//!
//! The context builder answers questions like:
//!   - "What symbols are directly adjacent to X?"
//!   - "What does the call chain for X look like?"
//!   - "Which files import/depend on X?"

use db::Store;
use graph::GraphEngine;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use types::{FileId, SymbolDef, SymbolId};

mod builder;

pub use builder::{CalleeDetail, CallerDetail, ContextSlice, ContextView, SourceSnippet};

pub type SourceLookupFn = dyn Fn(&SymbolId) -> Option<String> + Send + Sync;

/// AI context builder: constructs symbol-rich context from the codebase graph.
pub struct ContextBuilder {
    store: Arc<Store>,
    graph: RwLock<Arc<GraphEngine>>,
    project_root: Option<PathBuf>,
    /// Optional callback for extracting symbol source via AST-aware parsing.
    /// When set, `read_source_snippet` delegates to this callback; when `None`,
    /// falls back to `TextRange`-based line extraction.
    source_fn: Option<Arc<SourceLookupFn>>,
}

impl ContextBuilder {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self {
            store,
            graph: RwLock::new(graph),
            project_root: None,
            source_fn: None,
        }
    }

    /// Set a project root for reading source snippets from disk.
    /// Without this, [`ContextView::subject_source`] and callsite snippets
    /// will always be empty.
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }

    /// Set an optional AST-aware source extraction function.
    ///
    /// When provided, `read_source_snippet` delegates to this callback
    /// instead of using `TextRange`-based line extraction.  The callback
    /// receives a `SymbolId` and returns the exact source text for that
    /// symbol (usually via tree-sitter re-parsing).
    pub fn with_source_fn(mut self, f: Arc<SourceLookupFn>) -> Self {
        self.source_fn = Some(f);
        self
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
            anyhow::bail!("symbol not found: {symbol_id}");
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

        // Filter out self-referencing edges (e.g. init functions calling
        // themselves, or resolution fallbacks defaulting to the source symbol).
        let filtered_callers: Vec<SymbolId> = resolved_callers
            .into_iter()
            .filter(|id| id != symbol_id)
            .collect();
        let filtered_callees: Vec<SymbolId> = resolved_callees
            .into_iter()
            .filter(|id| id != symbol_id)
            .collect();

        let caller_syms = resolve_symbols(&self.store, &filtered_callers)?;
        let callee_syms = resolve_symbols(&self.store, &filtered_callees)?;

        // Build detailed caller/callee info with source snippets
        let caller_details =
            self.build_caller_details(symbol_id, &filtered_callers, &caller_syms)?;
        let callee_details =
            self.build_callee_details(symbol_id, &filtered_callees, &callee_syms)?;

        let subject_file_path = self
            .store
            .get_file(&sym.file_id)
            .ok()
            .flatten()
            .map(|info| info.path);
        let subject_source = self.read_source_snippet(sym);

        let view = ContextView {
            subject: sym.clone(),
            subject_file_path,
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
            anyhow::bail!("symbol not found: {symbol_id}");
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
            let edge = edges.iter().find(|e| {
                e.source == caller_sym.id
                    && (e.kind == types::EdgeKind::Calls
                        || e.kind == types::EdgeKind::RegistersCallback)
            });
            let line = edge
                .and_then(|e| e.location.as_ref())
                .map(|r| r.start_line)
                .unwrap_or(caller_sym.range.start_line);
            let callsite_snippet = edge
                .and_then(|e| e.location.as_ref())
                .and_then(|loc| self.read_line_snippet(&caller_sym.file_id, loc.start_line))
                .unwrap_or_default();
            details.push(CallerDetail {
                symbol: caller_sym.clone(),
                callsite_line: line,
                callsite_snippet,
                edge_kind: edge.map(|e| e.kind).unwrap_or(types::EdgeKind::Calls),
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
            let edge = edges.iter().find(|e| {
                e.target == callee_sym.id
                    && (e.kind == types::EdgeKind::Calls
                        || e.kind == types::EdgeKind::RegistersCallback)
            });
            let line = edge
                .and_then(|e| e.location.as_ref())
                .map(|r| r.start_line)
                .unwrap_or(callee_sym.range.start_line);
            let callsite_snippet = edge
                .and_then(|e| e.location.as_ref())
                .and_then(|loc| self.read_line_snippet(&callee_sym.file_id, loc.start_line))
                .unwrap_or_default();
            details.push(CalleeDetail {
                symbol: callee_sym.clone(),
                callsite_line: line,
                callsite_snippet,
                edge_kind: edge.map(|e| e.kind).unwrap_or(types::EdgeKind::Calls),
                callee_signature: callee_sym.signature.clone(),
            });
        }
        Ok(details)
    }

    /// Read a single line from a source file at the given 0-based line.
    fn read_line_snippet(&self, file_id: &FileId, line_0based: u32) -> Option<String> {
        let root = self.project_root.as_ref()?;
        let file_info = self.store.get_file(file_id).ok().flatten()?;
        let full_path = root.join(&file_info.path);
        let canonical = full_path.canonicalize().ok()?;
        let canonical_root = root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        let content = std::fs::read_to_string(&canonical).ok()?;
        let line_idx = line_0based as usize;
        content.lines().nth(line_idx).map(|l| l.trim().to_string())
    }

    /// Read the subject symbol's source code from disk.
    ///
    /// When `source_fn` is set, delegates to AST-aware extraction (tree-sitter
    /// re-parsing). Otherwise falls back to `TextRange`-based line extraction.
    fn read_source_snippet(&self, sym: &SymbolDef) -> Option<SourceSnippet> {
        // Primary path: AST-aware extraction via the injected callback.
        if let Some(ref source_fn) = self.source_fn {
            if let Some(text) = source_fn(&sym.id) {
                let all_lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                let total_lines = all_lines.len() as u32;
                const MAX_CONTEXT_LINES: usize = 60;
                let truncated = all_lines.len() > MAX_CONTEXT_LINES;
                let lines = if truncated {
                    all_lines.into_iter().take(MAX_CONTEXT_LINES).collect()
                } else {
                    all_lines
                };
                return Some(SourceSnippet {
                    lines,
                    start_line: sym.range.start_line,
                    total_lines,
                    truncated,
                });
            }
            // Fall through to TextRange fallback if AST extraction returns None.
        }

        // Fallback: TextRange-based extraction (requires project_root).
        let root = self.project_root.as_ref()?;
        let file_info = self.store.get_file(&sym.file_id).ok().flatten()?;
        let full_path = root.join(&file_info.path);
        let canonical = full_path.canonicalize().ok()?;
        let canonical_root = root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        let content = std::fs::read_to_string(&canonical).ok()?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = sym.range.start_line as usize;
        let end = (sym.range.end_line as usize + 1).min(all_lines.len());
        if start >= all_lines.len() {
            return None;
        }
        let snippet_lines: Vec<String> = all_lines[start..end]
            .iter()
            .map(|l| l.to_string())
            .collect();
        let total_lines = all_lines.len() as u32;
        const MAX_CONTEXT_LINES: usize = 60;
        let truncated = snippet_lines.len() > MAX_CONTEXT_LINES;
        let lines = if truncated {
            snippet_lines.into_iter().take(MAX_CONTEXT_LINES).collect()
        } else {
            snippet_lines
        };
        Some(SourceSnippet {
            lines,
            start_line: start as u32,
            total_lines,
            truncated,
        })
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
