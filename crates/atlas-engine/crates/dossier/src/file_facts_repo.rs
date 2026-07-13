//! `FileFactsRepository` implementation backed by `db::Store`.
//!
//! This module implements the `FileFactsRepository` trait defined in
//! [`crate::traits`], providing file-level facts (imports, exports, and
//! peer symbols) by delegating to the persistence layer.
//!
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::traits::FileFactsRepository;
use crate::types::{ExportFact, ExportKind, ExportSource};

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

    fn get_exports(&self, file_id: &types::FileId) -> Result<Vec<ExportFact>> {
        let symbols = self.store.find_symbols_by_file(file_id)?;
        let imports = self.store.find_imports_by_file(file_id)?;
        let mut facts = Vec::new();
        let mut seen = HashSet::new();
        let mut explicitly_mapped_symbols = HashSet::new();

        // Local export clauses and default exports are persisted as
        // ImportKind::ExportFrom with an empty module. Reuse that fact to map
        // the outward name back to the existing local SymbolDef.
        for export in imports.iter().filter(|import| {
            import.kind == types::ImportKind::ExportFrom && import.module.is_empty()
        }) {
            let Some(symbol) = symbols
                .iter()
                .find(|symbol| symbol.name == export.imported_name)
            else {
                continue;
            };
            let is_default = export.local_name.as_deref() == Some("default");
            let exported_name = if is_default {
                "default".to_string()
            } else {
                export
                    .local_name
                    .clone()
                    .unwrap_or_else(|| export.imported_name.clone())
            };
            explicitly_mapped_symbols.insert(symbol.id);
            if seen.insert((exported_name.clone(), None, Some(symbol.id))) {
                facts.push(ExportFact {
                    exported_name,
                    local_symbol_id: Some(symbol.id.to_hex()),
                    module: None,
                    export_kind: if is_default {
                        ExportKind::Default_
                    } else {
                        ExportKind::Named
                    },
                    source: ExportSource::ExplicitSyntax,
                    line: export.range.start_line + 1,
                });
            }
        }

        // Standalone declarations such as `export struct Foo` are represented
        // directly by SymbolDef.exported. A default/local export fact above is
        // more specific and suppresses the generic named form.
        for symbol in symbols
            .iter()
            .filter(|symbol| symbol.exported && !explicitly_mapped_symbols.contains(&symbol.id))
        {
            if seen.insert((symbol.name.clone(), None, Some(symbol.id))) {
                facts.push(ExportFact {
                    exported_name: symbol.name.clone(),
                    local_symbol_id: Some(symbol.id.to_hex()),
                    module: None,
                    export_kind: ExportKind::Named,
                    source: ExportSource::ExplicitSyntax,
                    line: symbol.range.start_line + 1,
                });
            }
        }

        // Re-exports already exist as ImportDef::ExportFrom. They have no
        // local SymbolId, so preserve the outward name and source module.
        for export in imports.iter().filter(|import| {
            import.kind == types::ImportKind::ExportFrom && !import.module.is_empty()
        }) {
            // The TypeScript query emits `export.module` as a statement-level
            // carrier for both wildcard and named re-exports. It is a wildcard
            // only when that same statement has no named export captures.
            let wildcard = export.is_wildcard
                && !imports.iter().any(|candidate| {
                    candidate.kind == types::ImportKind::ExportFrom
                        && candidate.module == export.module
                        && candidate.range == export.range
                        && !candidate.imported_name.is_empty()
                });
            if export.imported_name.is_empty() && !wildcard {
                continue;
            }
            let exported_name = if wildcard {
                "*".to_string()
            } else {
                export
                    .local_name
                    .clone()
                    .unwrap_or_else(|| export.imported_name.clone())
            };
            let export_kind = if wildcard {
                ExportKind::Wildcard
            } else if exported_name == "default" {
                ExportKind::Default_
            } else {
                ExportKind::Named
            };
            if seen.insert((exported_name.clone(), Some(export.module.clone()), None)) {
                facts.push(ExportFact {
                    exported_name,
                    local_symbol_id: None,
                    module: Some(export.module.clone()),
                    export_kind,
                    source: ExportSource::ExplicitSyntax,
                    line: export.range.start_line + 1,
                });
            }
        }

        facts.sort_by_key(|fact| (fact.line, fact.exported_name.clone()));
        Ok(facts)
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
    fn get_exports_returns_exported_symbols() {
        let store = make_store();
        let file_id = FileId::generate("src/foo.ts");
        seed_file(&store, file_id, "src/foo.ts");

        let mut symbol = make_symbol(file_id, "src/foo.ts", "Foo", "Foo");
        symbol.exported = true;
        store.insert_symbols(&[symbol.clone()]).unwrap();

        let repo = FileFactsRepo::new(store);
        let exports = repo.get_exports(&file_id).unwrap();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].exported_name, "Foo");
        assert_eq!(exports[0].local_symbol_id, Some(symbol.id.to_hex()));
        assert_eq!(exports[0].module, None);
        assert_eq!(exports[0].export_kind, ExportKind::Named);
        assert_eq!(exports[0].source, ExportSource::ExplicitSyntax);
    }

    #[test]
    fn get_exports_preserves_named_and_wildcard_reexports() {
        let store = make_store();
        let file_id = FileId::generate("src/index.ts");
        seed_file(&store, file_id, "src/index.ts");

        let named_range = TextRange {
            start_byte: 0,
            end_byte: 44,
            start_line: 0,
            end_line: 0,
            ..Default::default()
        };
        let wildcard_range = TextRange {
            start_byte: 45,
            end_byte: 71,
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        let imports = vec![
            ImportDef {
                id: ImportId::generate(&file_id, "export_from", "./model", Some(""), 0),
                file_id,
                kind: ImportKind::ExportFrom,
                module: "./model".to_string(),
                imported_name: String::new(),
                local_name: None,
                is_wildcard: true,
                is_relative: true,
                range: named_range,
                alias: None,
            },
            ImportDef {
                id: ImportId::generate(&file_id, "export_from", "./model", Some("Model"), 0),
                file_id,
                kind: ImportKind::ExportFrom,
                module: "./model".to_string(),
                imported_name: "Model".to_string(),
                local_name: Some("PublicModel".to_string()),
                is_wildcard: false,
                is_relative: true,
                range: named_range,
                alias: None,
            },
            ImportDef {
                id: ImportId::generate(&file_id, "export_from", "./helpers", Some(""), 45),
                file_id,
                kind: ImportKind::ExportFrom,
                module: "./helpers".to_string(),
                imported_name: String::new(),
                local_name: None,
                is_wildcard: true,
                is_relative: true,
                range: wildcard_range,
                alias: None,
            },
        ];
        store.insert_imports(&imports).unwrap();

        let exports = FileFactsRepo::new(store).get_exports(&file_id).unwrap();
        assert_eq!(exports.len(), 2);
        assert_eq!(exports[0].exported_name, "PublicModel");
        assert_eq!(exports[0].module.as_deref(), Some("./model"));
        assert_eq!(exports[0].export_kind, ExportKind::Named);
        assert_eq!(exports[1].exported_name, "*");
        assert_eq!(exports[1].module.as_deref(), Some("./helpers"));
        assert_eq!(exports[1].export_kind, ExportKind::Wildcard);
        assert!(
            exports
                .iter()
                .all(|export| export.local_symbol_id.is_none())
        );
    }

    #[test]
    fn get_peers_returns_other_symbols_excluding_subject() {
        let store = make_store();
        let file_id = FileId::generate("src/lib.ts");
        seed_file(&store, file_id, "src/lib.ts");

        let a = make_symbol(file_id, "src/lib.ts", "alpha", "Lib.alpha");
        let b = make_symbol(file_id, "src/lib.ts", "beta", "Lib.beta");
        let c = make_symbol(file_id, "src/lib.ts", "gamma", "Lib.gamma");

        store
            .insert_symbols(&[a.clone(), b.clone(), c.clone()])
            .unwrap();

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
