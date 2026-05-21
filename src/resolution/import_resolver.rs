//! Import path resolver — maps import statements to candidate symbols.
//!
//! P2: Enhanced with PathAliasResolver (tsconfig.json paths) and
//! ExportResolver (re-export/barrel chains).

use crate::db::Store;
use crate::types::*;

use super::path_alias::PathAliasResolver;

/// Resolves import paths to potential symbols.
///
/// Resolution strategy:
/// 1. If PathAliasResolver is configured, resolve the import path first
/// 2. Generate candidate qualified names from the (possibly rewritten) import
/// 3. Look up candidates by qualified name
/// 4. Fallback: search by imported name
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
}
