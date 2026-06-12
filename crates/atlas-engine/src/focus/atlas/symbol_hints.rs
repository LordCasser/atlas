//! Symbol hints builder — Tier 1 bootstrap using manifest extraction.
//!
//! Populates the `symbol_hints` table from manifest-level (top-level only)
//! extraction results.  Hints are lightweight; missing from hints does not
//! mean the symbol does not exist.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::ids::FileId;
use types::Language;

/// Builds the symbol_hints index using manifest extraction.
pub struct SymbolHintsBuilder {
    store: Arc<Store>,
    project_root: PathBuf,
}

impl SymbolHintsBuilder {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
        }
    }

    /// Build hints for a batch of files using manifest extraction.
    ///
    /// Each file is read from disk, manifest-extracted (top-level symbols
    /// only), and the resulting symbol names/kind/line are inserted as hints
    /// with default confidence 0.9 and source "manifest".
    ///
    /// Returns the total number of hints inserted.
    pub fn build_for_files(&self, file_ids: &[FileId]) -> Result<usize> {
        let mut total: usize = 0;

        for file_id in file_ids {
            let path = match self.store.find_file_inventory_path(file_id)? {
                Some(p) => p,
                None => continue, // Not in inventory yet — skip
            };

            let hints = self.extract_hints_for_path(file_id, &path)?;
            total += hints.len();
            if !hints.is_empty() {
                self.store.insert_symbol_hints_batch(&hints)?;
            }
        }

        Ok(total)
    }

    /// Extract manifest-level hints from a single file.
    fn extract_hints_for_path(&self, file_id: &FileId, rel_path: &str) -> Result<Vec<db::SymbolHint>> {
        let abs_path = self.project_root.join(rel_path);

        // Detect language from extension
        let language = match Language::from_path(&abs_path) {
            Some(lang) => lang,
            None => return Ok(Vec::new()),
        };

        // Create frontend for this language
        let frontend = match create_frontend(language) {
            Some(f) => f,
            None => return Ok(Vec::new()),
        };

        // Read source and compute content hash
        let source = match fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        // Manifest extraction — top-level symbols only, fast
        let facts = match extract_file_with_mode(
            &frontend,
            *file_id,
            &abs_path,
            &source,
            &content_hash,
            ExtractionMode::Manifest,
        ) {
            Ok(f) => f,
            Err(_) => return Ok(Vec::new()),
        };

        let file_id_bytes = file_id.as_bytes().to_vec();
        let hints: Vec<db::SymbolHint> = facts
            .symbols
            .iter()
            .map(|sym| db::SymbolHint {
                name: sym.name.clone(),
                file_id: file_id_bytes.clone(),
                kind: sym.kind.as_str().to_string(),
                line: sym.range.start_line,
                confidence: 0.9,
                source: "manifest".to_string(),
                freshness: String::new(),
            })
            .collect();

        Ok(hints)
    }
}
