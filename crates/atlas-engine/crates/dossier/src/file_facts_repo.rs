//! `FileFactsRepository` implementation backed by `db::Store`.
//!
//! This module implements the `FileFactsRepository` trait defined in
//! [`crate::traits`], providing file-level facts (imports, exports, and
//! peer symbols) by delegating to the persistence layer.
//!
//! # Decision #1 — Exports
//!
//! `ExportFact` + `ExportSource` are the official extractor contract, but
//! most languages do not have an `ExportDef` extractor yet. Per Decision #1,
//! `get_exports` returns an empty `Vec` when export data is unavailable
//! (no visibility fallback). The caller is responsible for adding a
//! coverage warning.

use std::sync::Arc;

use anyhow::Result;

use crate::traits::FileFactsRepository;
use crate::types::ExportFact;

pub struct FileFactsRepo {
    store: Arc<db::Store>,
}

impl FileFactsRepo {
    pub fn new(store: Arc<db::Store>) -> Self {
        Self { store }
    }
}

impl FileFactsRepository for FileFactsRepo {
    fn get_imports(&self, file_id: &types::FileId) -> Result<Vec<types::ImportDef>> {
        self.store.find_imports_by_file(file_id)
    }

    /// v1: Export extraction is not yet implemented by language extractors.
    /// Returns empty vec until ExportDef extraction is added per Decision #1.
    /// The caller (ExploreDossierBuilder) will add a coverage warning when
    /// exports are empty.
    fn get_exports(&self, _file_id: &types::FileId) -> Result<Vec<ExportFact>> {
        Ok(Vec::new())
    }

    fn get_peers(
        &self,
        file_id: &types::FileId,
        exclude_id: &types::SymbolId,
        limit: usize,
    ) -> Result<Vec<types::SymbolDef>> {
        let symbols = self.store.find_symbols_by_file(file_id)?;
        let peers: Vec<_> = symbols
            .into_iter()
            .filter(|s| &s.id != exclude_id)
            .take(limit)
            .collect();
        Ok(peers)
    }
}
