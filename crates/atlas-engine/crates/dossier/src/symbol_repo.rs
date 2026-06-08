//! SymbolRepository implementation backed by `db::Store`.
//!
//! Delegates symbol resolution, signature lookup, and file-path queries
//! to the shared store.

use std::sync::Arc;

use crate::traits::SymbolRepository;

/// Concrete implementation of [`SymbolRepository`] backed by the Atlas store.
pub struct SymbolRepo {
    store: Arc<db::Store>,
}

impl SymbolRepo {
    pub fn new(store: Arc<db::Store>) -> Self {
        Self { store }
    }
}

impl SymbolRepository for SymbolRepo {
    fn resolve(&self, query: &str) -> anyhow::Result<Vec<types::SymbolDef>> {
        self.store.search_symbols(query)
    }

    fn get_signature(&self, symbol_id: &types::SymbolId) -> anyhow::Result<Option<String>> {
        match self.store.find_symbol_by_id(symbol_id)? {
            Some(sym) => Ok(sym.signature),
            None => Ok(None),
        }
    }

    fn get_symbol_by_id(&self, id: &types::SymbolId) -> anyhow::Result<Option<types::SymbolDef>> {
        self.store.find_symbol_by_id(id)
    }

    fn get_file_path(&self, file_id: &types::FileId) -> anyhow::Result<Option<String>> {
        Ok(self.store.get_file(file_id)?.map(|info| info.path))
    }
}
