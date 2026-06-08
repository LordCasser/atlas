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

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use types::{
        FileId, FileInfo, ImportDef, ImportId, ImportKind, Language, ParseStatus, SymbolDef,
        SymbolId, SymbolKind, TextRange,
    };

    fn make_store() -> Arc<db::Store> {
        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn seed_file(store: &db::Store, file_id: FileId, path: &str) {
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language: Language::TypeScript,
                content_hash: "abc".to_string(),
                status: ParseStatus::Success,
            })
            .unwrap();
    }

    fn make_symbol(file_id: FileId, _path: &str, name: &str, qualified_name: &str) -> SymbolDef {
        let sym_id = SymbolId::generate(&file_id, "typescript", qualified_name, "function", None);
        let parts: Vec<_> = qualified_name.split('.').map(|s| s.to_string()).collect();
        SymbolDef {
            id: sym_id,
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            symbol_path: parts,
            file_id,
            language: Language::TypeScript,
            range: TextRange {
                start_line: 1,
                end_line: 5,
                ..Default::default()
            },
            name_range: TextRange::default(),
            signature: Some(format!("fn {name}()")),
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    #[test]
    fn get_imports_returns_seeded_imports() {
        let store = make_store();
        let file_id = FileId::generate("src/mod.ts");
        seed_file(&store, file_id, "src/mod.ts");

        let import = ImportDef {
            id: ImportId::generate(&file_id, "import", "react", Some("useState"), 0),
            file_id,
            kind: ImportKind::Import,
            module: "react".to_string(),
            imported_name: "useState".to_string(),
            local_name: None,
            is_wildcard: false,
            is_relative: false,
            range: TextRange {
                start_line: 0,
                end_line: 0,
                ..Default::default()
            },
            alias: None,
        };
        store.insert_imports(&[import]).unwrap();

        let repo = FileFactsRepo::new(store);
        let imports = repo.get_imports(&file_id).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "react");
        assert_eq!(imports[0].imported_name, "useState");
    }

    #[test]
    fn get_imports_empty_for_file_with_no_imports() {
        let store = make_store();
        let file_id = FileId::generate("src/noimports.ts");
        seed_file(&store, file_id, "src/noimports.ts");

        let repo = FileFactsRepo::new(store);
        let imports = repo.get_imports(&file_id).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn get_exports_returns_empty_per_decision_1() {
        let store = make_store();
        let file_id = FileId::generate("src/foo.ts");
        seed_file(&store, file_id, "src/foo.ts");

        let repo = FileFactsRepo::new(store);
        let exports = repo.get_exports(&file_id).unwrap();
        assert!(exports.is_empty());
    }

    #[test]
    fn get_peers_returns_other_symbols_excluding_subject() {
        let store = make_store();
        let file_id = FileId::generate("src/lib.ts");
        seed_file(&store, file_id, "src/lib.ts");

        let a = make_symbol(file_id, "src/lib.ts", "alpha", "Lib.alpha");
        let b = make_symbol(file_id, "src/lib.ts", "beta", "Lib.beta");
        let c = make_symbol(file_id, "src/lib.ts", "gamma", "Lib.gamma");

        store.insert_symbols(&[a.clone(), b.clone(), c.clone()]).unwrap();

        let repo = FileFactsRepo::new(store);
        let peers = repo.get_peers(&file_id, &a.id, 10).unwrap();
        // Should exclude a, return b and c.
        assert_eq!(peers.len(), 2);
        let names: Vec<&str> = peers.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
        assert!(!names.contains(&"alpha"));
    }

    #[test]
    fn get_peers_respects_limit() {
        let store = make_store();
        let file_id = FileId::generate("src/big.ts");
        seed_file(&store, file_id, "src/big.ts");

        let subject = make_symbol(file_id, "src/big.ts", "main", "Big.main");
        let mut symbols = vec![subject.clone()];
        for i in 0..10 {
            symbols.push(make_symbol(
                file_id,
                "src/big.ts",
                &format!("f{i}"),
                &format!("Big.f{i}"),
            ));
        }
        store.insert_symbols(&symbols).unwrap();

        let repo = FileFactsRepo::new(store);
        let peers = repo.get_peers(&file_id, &subject.id, 3).unwrap();
        assert_eq!(peers.len(), 3, "should respect peer limit of 3");
    }
}
