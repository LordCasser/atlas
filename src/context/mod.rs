//! Context building for AI: assemble relevant codebase context.
//!
//! The context builder answers questions like:
//!   - "What symbols are directly adjacent to X?"
//!   - "What does the call chain for X look like?"
//!   - "Which files import/depend on X?"

pub mod formatter;
pub mod search;

use crate::db::Store;
use crate::graph::GraphEngine;
use crate::types::{FileId, SymbolDef, SymbolId};
use std::sync::Arc;

mod builder;

pub use builder::{ContextSlice, ContextView};

/// AI context builder: constructs symbol-rich context from the codebase graph.
pub struct ContextBuilder {
    store: Arc<Store>,
    graph: Arc<GraphEngine>,
}

impl ContextBuilder {
    pub fn new(store: Arc<Store>, graph: Arc<GraphEngine>) -> Self {
        Self { store, graph }
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Build a context view around a specific symbol: its callers, callees,
    /// containing scope, and adjacent symbols.
    pub fn build_context_for_symbol(
        &self,
        symbol_id: &SymbolId,
    ) -> anyhow::Result<ContextView> {
        let sym = self.store.find_symbol_by_id(symbol_id)?;
        let Some(ref sym) = sym else {
            anyhow::bail!("symbol not found: {}", symbol_id);
        };

        let caller_view = self.graph.callers(symbol_id);
        let callee_view = self.graph.callees(symbol_id);
        let file_id = sym.file_id;

        // Peers: symbols in the same file
        let file_symbols = self.store.find_symbols_by_file(&file_id)?;

        // Importers are symbols; dependencies are files
        let importer_symbols = self.graph.importers(&file_id);
        let dep_files = self.graph.dependencies(&file_id);

        let view = ContextView {
            subject: sym.clone(),
            callers: resolve_symbols(&self.store, &self.graph.resolve_node_ids(&caller_view.callers))?,
            callees: resolve_symbols(&self.store, &self.graph.resolve_node_ids(&callee_view.callees))?,
            file_peers: file_symbols,
            importers: resolve_symbols_to_paths(&self.store, &importer_symbols)?,
            dependencies: resolve_files(&self.store, &dep_files)?,
        };

        Ok(view)
    }

    /// Build a context slice — lightweight neighbor-only view (no full file listing).
    pub fn build_context_slice(
        &self,
        symbol_id: &SymbolId,
    ) -> anyhow::Result<ContextSlice> {
        let sym = self.store.find_symbol_by_id(symbol_id)?;
        let Some(sym) = sym else {
            anyhow::bail!("symbol not found: {}", symbol_id);
        };

        let caller_view = self.graph.callers(symbol_id);
        let callee_view = self.graph.callees(symbol_id);

        let slice = ContextSlice {
            subject: sym,
            callers: resolve_symbols(&self.store, &self.graph.resolve_node_ids(&caller_view.callers))?,
            callees: resolve_symbols(&self.store, &self.graph.resolve_node_ids(&callee_view.callees))?,
        };

        Ok(slice)
    }

    /// Build context from a search query — finds the top match, then builds context.
    pub fn build_context_for_query(
        &self,
        query: &str,
    ) -> anyhow::Result<Option<ContextView>> {
        let results = self.store.search_symbols(query)?;
        if let Some(top) = results.first() {
            self.build_context_for_symbol(&top.id).map(Some)
        } else {
            Ok(None)
        }
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn resolve_symbols(store: &Store, ids: &[SymbolId]) -> anyhow::Result<Vec<SymbolDef>> {
    let mut symbols = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(sym) = store.find_symbol_by_id(id)? {
            symbols.push(sym);
        }
    }
    Ok(symbols)
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
    let mut paths = std::collections::BTreeSet::new();
    for id in ids {
        if let Some(sym) = store.find_symbol_by_id(id)? {
            if let Some(info) = store.get_file(&sym.file_id)? {
                paths.insert(info.path);
            }
        }
    }
    Ok(paths.into_iter().collect())
}
