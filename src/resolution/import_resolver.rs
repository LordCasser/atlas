//! Import path resolver — maps import statements to candidate symbols.
//!
//! P2: Enhanced with PathAliasResolver (tsconfig.json paths).
//!
//! Resolution strategy:
//! 1. Path-alias-scoped file lookup (takes priority when path alias resolves)
//! 2. Generate candidate qualified names from the (possibly rewritten) import
//! 3. Look up candidates by qualified name
//! 4. Fallback: search by imported name

use crate::db::Store;
use crate::types::*;

use super::path_alias::PathAliasResolver;

/// Resolves import paths to potential symbols.
///
/// Resolution strategy (in priority order):
/// 1. When `PathAliasResolver` rewrites the import module (e.g. `@lib/helper`
///    → `src/lib/helper`), the rewritten path is used to find matching DB files
///    and the imported name is looked up in those files directly.  This ensures
///    that `import { compute } from '@lib/helper'` resolves to the `compute`
///    symbol in the aliased module, not a same-named symbol in another file.
/// 2. Candidate qualified names from the import definition.
/// 3. Fallback: global FTS5 name search.
pub struct ImportResolver {
    store: std::sync::Arc<Store>,
    path_alias: PathAliasResolver,
}

impl ImportResolver {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self {
            store,
            path_alias: PathAliasResolver::empty(),
        }
    }

    /// Create an ImportResolver with a configured path alias resolver.
    pub fn with_path_alias(store: std::sync::Arc<Store>, path_alias: PathAliasResolver) -> Self {
        Self { store, path_alias }
    }

    /// Resolve an import definition into candidate symbols.
    pub fn resolve_import(&self, import: &ImportDef) -> anyhow::Result<Vec<SymbolDef>> {
        // ── P2: Path-alias-scoped file lookup ──
        // When a path alias rewrites the module path, resolve the imported name
        // in files matching the rewritten path. This gives priority to the
        // aliased module over global name search.
        if self.path_alias.has_aliases() {
            let resolved_module = self
                .path_alias
                .resolve(&import.module)
                .unwrap_or_else(|| import.module.clone());
            if resolved_module != import.module {
                let target_name = import
                    .local_name
                    .as_deref()
                    .or_else(|| {
                        if import.imported_name.is_empty() {
                            None
                        } else {
                            Some(import.imported_name.as_str())
                        }
                    })
                    .unwrap_or("");
                if !target_name.is_empty() {
                    let file_results =
                        self.resolve_by_module_path(&resolved_module, target_name);
                    if !file_results.is_empty() {
                        return Ok(file_results);
                    }
                }
            }
        }

        // ── Candidate qualified names + global fallback ──
        let candidate_names = self.candidate_qnames(import);

        let mut results = Vec::new();
        for qname in &candidate_names {
            if let Ok(syms) = self.store.find_symbols_by_qname(qname) {
                results.extend(syms);
            }
        }

        if results.is_empty() {
            // Fallback: search by local name or imported name
            let fallback_name = import.local_name.clone().or_else(|| {
                let n = import.imported_name.clone();
                if n.is_empty() { None } else { Some(n) }
            });

            if let Some(name) = fallback_name {
                results.extend(self.store.search_symbols(&name)?);
            }
        }

        Ok(results)
    }

    /// Look up a symbol by name in files whose DB path matches `resolved_module`.
    ///
    /// Matching rule: a file path matches `resolved_module` when:
    /// - the path equals `resolved_module` exactly, OR
    /// - the path starts with `resolved_module/` (subdirectory, e.g. `index.ts`), OR
    /// - the path starts with `resolved_module.` (extension, e.g. `helper.ts`).
    fn resolve_by_module_path(&self, resolved_module: &str, target_name: &str) -> Vec<SymbolDef> {
        let files = match self.store.list_files() {
            Ok(files) => files,
            Err(_) => return Vec::new(),
        };

        let mut results = Vec::new();

        let dir_prefix = format!("{}/", resolved_module);
        let file_prefix = format!("{}.", resolved_module);

        for file in &files {
            if file.path == resolved_module
                || file.path.starts_with(&dir_prefix)
                || file.path.starts_with(&file_prefix)
            {
                if let Ok(symbols) = self.store.find_symbols_by_file(&file.file_id) {
                    results.extend(symbols.into_iter().filter(|s| s.name == target_name));
                }
            }
        }

        results
    }

    /// Generate candidate qualified names from an import definition.
    ///
    /// P2: Uses PathAliasResolver to rewrite the module path before
    /// generating candidates. For example, `@/utils` becomes `src/utils`
    /// if a tsconfig path alias is configured.
    fn candidate_qnames(&self, import: &ImportDef) -> Vec<String> {
        let mut candidates = Vec::new();

        let module = &import.module;
        let name = &import.imported_name;
        let local: &str = import.local_name.as_deref().unwrap_or("");

        // P2: Apply path alias resolution to the module path
        let resolved_module = if self.path_alias.has_aliases() {
            self.path_alias
                .resolve(module)
                .unwrap_or_else(|| module.clone())
        } else {
            module.clone()
        };

        match import.kind {
            ImportKind::Import | ImportKind::Package | ImportKind::Use => {
                let n: &str = if !local.is_empty() {
                    local
                } else {
                    name.as_str()
                };
                if !n.is_empty() {
                    candidates.push(n.to_string());
                }
            }
            ImportKind::FromImport => {
                if !resolved_module.is_empty() && !name.is_empty() {
                    candidates.push(format!("{}.{}", resolved_module, name));
                }
                // Also try with the original module path
                if module != &resolved_module && !module.is_empty() && !name.is_empty() {
                    candidates.push(format!("{}.{}", module, name));
                }
                if !name.is_empty() {
                    candidates.push(name.clone());
                }
            }
            ImportKind::Include => {
                let effective_module = if resolved_module != *module {
                    &resolved_module
                } else {
                    module
                };
                if !effective_module.is_empty() {
                    let stem = effective_module
                        .rsplit('/')
                        .next()
                        .and_then(|s| s.rsplit('.').next())
                        .unwrap_or(effective_module.as_str());
                    candidates.push(stem.to_string());
                }
            }
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Minimal SymbolDef for unit tests (range fields set to zero).
    fn test_symbol(file_id: FileId, name: &str, kind: SymbolKind) -> SymbolDef {
        let range = TextRange {
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        SymbolDef {
            id: SymbolId::generate(
                &file_id,
                "typescript",
                name,
                kind.as_str(),
                None::<&str>,
            ),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            symbol_path: vec![],
            file_id,
            language: Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
        }
    }

    #[test]
    fn test_candidate_qnames_from_import() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let resolver = ImportResolver::new(store);

        let import = ImportDef {
            id: crate::types::ids::ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::FromImport.as_str(),
                "./lib",
                Some("greet"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::FromImport,
            module: "./lib".to_string(),
            imported_name: "greet".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: true,
            range: Default::default(),
        };

        let candidates = resolver.candidate_qnames(&import);
        assert!(candidates.contains(&"./lib.greet".to_string()));
        assert!(candidates.contains(&"greet".to_string()));
    }

    #[test]
    fn test_path_alias_resolution() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let mut paths = HashMap::new();
        paths.insert("@/*".to_string(), vec!["src/*".to_string()]);
        let path_alias = PathAliasResolver {
            base_url: None,
            paths,
        };

        let resolver = ImportResolver::with_path_alias(store, path_alias);

        let import = ImportDef {
            id: crate::types::ids::ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::FromImport.as_str(),
                "@/utils",
                Some("formatDate"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::FromImport,
            module: "@/utils".to_string(),
            imported_name: "formatDate".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        let candidates = resolver.candidate_qnames(&import);
        // Should resolve @/utils to src/utils
        assert!(
            candidates.iter().any(|c| c.contains("src/utils")),
            "expected src/utils in candidates, got: {:?}",
            candidates
        );
    }

    #[test]
    fn test_path_alias_file_scoped_lookup() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Create two files with a "compute" symbol — one in the aliased module,
        // one in a different module.
        let lib_file = FileId::generate("src/lib/helper.ts");
        let other_file = FileId::generate("src/other/utils.ts");

        let compute_lib = test_symbol(lib_file, "compute", SymbolKind::Function);
        let compute_other = test_symbol(other_file, "compute", SymbolKind::Function);

        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: lib_file,
                    path: "src/lib/helper.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: crate::types::enums::ParseStatus::Success,
                },
                symbols: vec![compute_lib.clone()],
                ..Default::default()
            })
            .unwrap();
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: other_file,
                    path: "src/other/utils.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "def".to_string(),
                    status: crate::types::enums::ParseStatus::Success,
                },
                symbols: vec![compute_other],
                ..Default::default()
            })
            .unwrap();

        // Path alias: @lib/helper → src/lib/helper
        let mut paths = HashMap::new();
        paths.insert("@lib/*".to_string(), vec!["src/lib/*".to_string()]);
        let path_alias = PathAliasResolver {
            base_url: None,
            paths,
        };
        let resolver = ImportResolver::with_path_alias(store, path_alias);

        // import { compute } from '@lib/helper'
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "@lib/helper",
                Some("compute"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "@lib/helper".to_string(),
            imported_name: "compute".to_string(),
            local_name: Some("compute".to_string()),
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        let results = resolver.resolve_import(&import).unwrap();

        // Should return ONLY the aliased module's compute, not the one from other/utils
        assert_eq!(results.len(), 1, "path alias should narrow to one file");
        assert_eq!(
            results[0].file_id, lib_file,
            "should resolve to the aliased module, not other/utils"
        );
        assert_eq!(results[0].name, "compute");
    }

    #[test]
    fn test_path_alias_falls_back_when_file_not_found() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Only one file — but not the alias target
        let other_file = FileId::generate("src/other/utils.ts");
        let compute_other = test_symbol(other_file, "compute", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: other_file,
                    path: "src/other/utils.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "def".to_string(),
                    status: crate::types::enums::ParseStatus::Success,
                },
                symbols: vec![compute_other.clone()],
                ..Default::default()
            })
            .unwrap();

        let mut paths = HashMap::new();
        paths.insert("@lib/*".to_string(), vec!["src/lib/*".to_string()]);
        let path_alias = PathAliasResolver {
            base_url: None,
            paths,
        };
        let resolver = ImportResolver::with_path_alias(store, path_alias);

        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "@lib/helper",
                Some("compute"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "@lib/helper".to_string(),
            imported_name: "compute".to_string(),
            local_name: Some("compute".to_string()),
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        // @lib/helper → src/lib/helper, but no file at that path exists.
        // Should fall through to global name search and find the other compute.
        let results = resolver.resolve_import(&import).unwrap();
        assert!(!results.is_empty(), "should fall back to global search");
        assert_eq!(results[0].name, "compute");
    }
}
