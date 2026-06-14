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

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use types::{
        FileId, FileInfo, Language, ParseStatus, SymbolDef, SymbolId, SymbolKind, TextRange,
    };

    /// Create an in-memory Store with schema initialized.
    fn make_store() -> Arc<db::Store> {
        let store = db::Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    /// Insert a file and a symbol, return their IDs.
    fn seed_symbol(store: &db::Store) -> (SymbolId, FileId, SymbolDef) {
        let file_id = FileId::generate("src/test.ts");
        let file_info = FileInfo {
            file_id,
            path: "src/test.ts".to_string(),
            language: Language::TypeScript,
            content_hash: "abc123".to_string(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let sym_id = SymbolId::generate(&file_id, "typescript", "Foo.bar", "method", None);
        let sym = SymbolDef {
            id: sym_id,
            kind: SymbolKind::Method,
            name: "bar".to_string(),
            qualified_name: "Foo.bar".to_string(),
            symbol_path: vec!["Foo".to_string(), "bar".to_string()],
            file_id,
            language: Language::TypeScript,
            range: TextRange {
                start_line: 10,
                end_line: 20,
                ..Default::default()
            },
            name_range: TextRange::default(),
            signature: Some("bar(): void".to_string()),
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };
        store.insert_symbols(&[sym.clone()]).unwrap();

        (sym_id, file_id, sym)
    }

    #[test]
    fn resolve_known_symbol() {
        let store = make_store();
        let _ = seed_symbol(&store);
        let repo = SymbolRepo::new(store);

        let results = repo.resolve("bar").unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "bar");
    }

    #[test]
    fn resolve_unknown_symbol() {
        let store = make_store();
        let _ = seed_symbol(&store);
        let repo = SymbolRepo::new(store);

        let results = repo.resolve("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn get_signature_for_function() {
        let store = make_store();
        let (sym_id, _, _) = seed_symbol(&store);
        let repo = SymbolRepo::new(store);

        let sig = repo.get_signature(&sym_id).unwrap();
        assert_eq!(sig, Some("bar(): void".to_string()));
    }

    #[test]
    fn get_signature_for_nonexistent_symbol() {
        let store = make_store();
        let _ = seed_symbol(&store);
        let repo = SymbolRepo::new(store);
        let unknown_id = SymbolId::generate(
            &FileId::generate("x.ts"),
            "typescript",
            "X",
            "function",
            None,
        );

        let sig = repo.get_signature(&unknown_id).unwrap();
        assert_eq!(sig, None);
    }

    #[test]
    fn get_symbol_by_id_found() {
        let store = make_store();
        let (sym_id, _, expected) = seed_symbol(&store);
        let repo = SymbolRepo::new(store);

        let found = repo.get_symbol_by_id(&sym_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().qualified_name, expected.qualified_name);
    }

    #[test]
    fn get_symbol_by_id_not_found() {
        let store = make_store();
        let _ = seed_symbol(&store);
        let repo = SymbolRepo::new(store);
        let unknown_id = SymbolId::generate(
            &FileId::generate("y.ts"),
            "typescript",
            "Y",
            "function",
            None,
        );

        let found = repo.get_symbol_by_id(&unknown_id).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn get_file_path_found() {
        let store = make_store();
        let (_, file_id, _) = seed_symbol(&store);
        let repo = SymbolRepo::new(store);

        let path = repo.get_file_path(&file_id).unwrap();
        assert_eq!(path, Some("src/test.ts".to_string()));
    }

    #[test]
    fn get_file_path_not_found() {
        let store = make_store();
        let _ = seed_symbol(&store);
        let repo = SymbolRepo::new(store);
        let unknown_file = FileId::generate("nonexistent.ts");

        let path = repo.get_file_path(&unknown_file).unwrap();
        assert_eq!(path, None);
    }
}
