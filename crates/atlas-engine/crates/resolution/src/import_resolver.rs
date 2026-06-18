//! Import path resolver — maps import statements to candidate symbols.
//!
//! P2: Enhanced with PathAliasResolver (tsconfig.json paths).
//!
//! Resolution strategy:
//! 1. Path-alias-scoped file lookup (takes priority when path alias resolves)
//! 2. Generate candidate qualified names from the (possibly rewritten) import
//! 3. Look up candidates by qualified name
//! 4. Fallback: search by imported name

use std::collections::{HashMap, HashSet};

use db::Store;
use types::*;

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
    /// QName → symbols cache. Empty vec = known miss.
    /// Scoped to this resolver instance (fresh per ResolutionSession::build).
    qname_cache: std::sync::Mutex<HashMap<String, Vec<SymbolDef>>>,
    /// Reexport chain cache. Key: (caller_file_id, import_module, target_name).
    reexport_cache: std::sync::Mutex<HashMap<(FileId, String, String), Vec<SymbolDef>>>,
    /// Module-path resolution cache. Key: (module_path, target_name).
    module_path_cache: std::sync::Mutex<HashMap<(String, String), Vec<SymbolDef>>>,
}

impl ImportResolver {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self {
            store,
            path_alias: PathAliasResolver::empty(),
            qname_cache: std::sync::Mutex::new(HashMap::new()),
            reexport_cache: std::sync::Mutex::new(HashMap::new()),
            module_path_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Create an ImportResolver with a configured path alias resolver.
    pub fn with_path_alias(store: std::sync::Arc<Store>, path_alias: PathAliasResolver) -> Self {
        Self {
            store,
            path_alias,
            qname_cache: std::sync::Mutex::new(HashMap::new()),
            reexport_cache: std::sync::Mutex::new(HashMap::new()),
            module_path_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Resolve an import definition into candidate symbols.
    pub fn resolve_import(&self, import: &ImportDef) -> anyhow::Result<Vec<SymbolDef>> {
        let _span = tracing::debug_span!(target: "atlas_resolve",
            "resolution.import_resolve",
            module = %import.module,
            name = %import.imported_name,
        )
        .entered();
        let _timer = std::time::Instant::now();
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
                // Use imported_name (the actual symbol name in the source module)
                // rather than local_name (which may be an alias like "hello" for "greet").
                let target_name = if import.imported_name.is_empty() {
                    import.local_name.as_deref().unwrap_or("")
                } else {
                    import.imported_name.as_str()
                };
                if !target_name.is_empty() {
                    let file_results = self.resolve_by_module_path(&resolved_module, target_name);
                    if !file_results.is_empty() {
                        return Ok(file_results);
                    }
                }
            }
        }

        // ── Candidate qualified names + global fallback ──
        let candidate_names = self.candidate_qnames(import);

        let mut results = Vec::new();
        let db_start = std::time::Instant::now();
        for qname in &candidate_names {
            // Instance-scoped cache: Mutex-held for HashMap ops only (microseconds).
            // Fresh per ResolutionSession::build(), avoiding stale cross-session entries.
            let cached = self.qname_cache.lock().unwrap().get(qname).cloned();
            if let Some(cached_syms) = cached {
                if cached_syms.is_empty() {
                    continue; // negative entry: known miss, skip DB
                }
                results.extend(cached_syms);
                continue;
            }

            if let Ok(syms) = self.store.find_symbols_by_qname(qname) {
                self.qname_cache
                    .lock()
                    .unwrap()
                    .insert(qname.clone(), syms.clone());
                results.extend(syms);
            }
            // DB errors not cached — transient failures should be retried
        }
        let db_elapsed = db_start.elapsed();

        if results.is_empty() {
            // Fallback: prefer imported_name (actual symbol name) over local_name (alias).
            let fallback_name = if import.imported_name.is_empty() {
                import.local_name.clone()
            } else {
                Some(import.imported_name.clone())
            };

            let mut fallback_used = false;
            if let Some(ref name) = fallback_name {
                // Strategy A: module-path-aware lookup (constrains to import target)
                if !import.module.is_empty() {
                    let by_module = self.resolve_by_module_path(&import.module, name);
                    if !by_module.is_empty() {
                        results.extend(by_module);
                    }
                }
                // Strategy B: global fallback as last resort
                if results.is_empty() {
                    results.extend(self.store.search_symbols(name)?);
                    fallback_used = true;
                }
            }

            let total = _timer.elapsed();
            tracing::debug!(
                target: "atlas::resolution",
                "import resolve slow path: module='{}', name='{}', qname_candidates={}, \
                 qname_db_lookup={:?}, fallback_used={}, total={:?}",
                import.module,
                import.imported_name,
                candidate_names.len(),
                db_elapsed,
                fallback_used,
                total,
            );
        } else if db_elapsed > std::time::Duration::from_millis(2) {
            let total = _timer.elapsed();
            tracing::debug!(
                target: "atlas::resolution",
                "import resolve: module='{}', name='{}', qname_candidates={}, \
                 qname_db_lookup={:?}, results={}, total={:?}",
                import.module,
                import.imported_name,
                candidate_names.len(),
                db_elapsed,
                results.len(),
                total,
            );
        }

        Ok(results)
    }

    /// Resolve through barrel re-export chains.
    ///
    /// When an import resolves to a file that has ExportFrom facts
    /// (i.e. it's a barrel file with `export * from './lib'`), this method
    /// follows the re-export chain to find the original symbol definition.
    pub fn resolve_through_reexports(
        &self,
        import: &ImportDef,
        candidates: Vec<SymbolDef>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        // Only follow re-exports when the import target name is known
        if import.kind == ImportKind::ExportFrom {
            return Ok(candidates);
        }
        // Use imported_name (actual symbol name) — local_name may be an alias.
        let target_name = if import.imported_name.is_empty() {
            import.local_name.as_deref().unwrap_or("")
        } else {
            import.imported_name.as_str()
        };
        if target_name.is_empty() {
            return Ok(candidates);
        }

        // ── Reexport chain cache ──
        // The same (caller file, import module, target name) recurs across
        // many references in the same file.  Each miss costs multiple recursive
        // DB queries (find_imports_by_file + resolve_relative_module +
        // find_symbols_by_file) in follow_reexport_chain.
        let cache_key = (
            import.file_id,
            import.module.clone(),
            target_name.to_string(),
        );
        if let Some(cached) = self.reexport_cache.lock().unwrap().get(&cache_key).cloned() {
            return Ok(cached);
        }

        let mut resolved: Vec<SymbolDef> = Vec::new();
        let mut visited: std::collections::HashSet<FileId> = std::collections::HashSet::new();

        for sym in &candidates {
            visited.insert(sym.file_id);
            if let Some(chain_sym) =
                self.follow_reexport_chain(&sym.file_id, target_name, &mut visited, 0)
            {
                resolved.push(chain_sym);
            } else {
                resolved.push(sym.clone());
            }
        }
        // Cache result for subsequent references with same (file, module, name)
        let cache_key = (
            import.file_id,
            import.module.clone(),
            target_name.to_string(),
        );
        self.reexport_cache
            .lock()
            .unwrap()
            .insert(cache_key, resolved.clone());
        Ok(resolved)
    }

    /// Recursively follow re-export chains from a barrel file to find the
    /// actual symbol definition.  Returns None if the chain dead-ends.
    fn follow_reexport_chain(
        &self,
        file_id: &FileId,
        name: &str,
        visited: &mut std::collections::HashSet<FileId>,
        depth: u32,
    ) -> Option<SymbolDef> {
        if depth > 10 {
            return None;
        }

        // Get ExportFrom facts for this barrel file
        let exports = self.store.find_imports_by_file(file_id).ok()?;
        let reexports: Vec<_> = exports
            .iter()
            .filter(|i| i.kind == ImportKind::ExportFrom)
            .collect();
        if reexports.is_empty() {
            return None;
        }

        for reexport in &reexports {
            let module_path = &reexport.module;
            if module_path.is_empty() {
                continue;
            }

            // Resolve relative module path to target files
            let target_files = self
                .store
                .resolve_relative_module(file_id, module_path)
                .ok()
                .unwrap_or_default();

            for tf in &target_files {
                if !visited.insert(tf.file_id) {
                    continue; // cycle guard
                }

                // Search for the symbol in the target file
                let symbols = self.store.find_symbols_by_file(&tf.file_id).ok()?;
                if let Some(sym) = symbols.iter().find(|s| s.name == name) {
                    return Some(sym.clone());
                }

                // Recurse: target file might itself be a barrel
                if let Some(chain_sym) =
                    self.follow_reexport_chain(&tf.file_id, name, visited, depth + 1)
                {
                    return Some(chain_sym);
                }
            }
        }
        None
    }

    /// Look up a symbol by name in files whose DB path matches `resolved_module`.
    ///
    /// Uses a SQL `LIKE` query directly on the files table instead of loading
    /// all files and doing O(n) client-side `starts_with` scans.
    ///
    /// Results are cached via the instance-scoped module_path_cache (same pattern as
    /// qname_cache and reexport_cache) — the same (module, name) pair repeats
    /// across many references in a monorepo.
    fn resolve_by_module_path(&self, resolved_module: &str, target_name: &str) -> Vec<SymbolDef> {
        let cache_key = (resolved_module.to_string(), target_name.to_string());

        // Check instance-scoped cache first
        let cached = self
            .module_path_cache
            .lock()
            .unwrap()
            .get(&cache_key)
            .cloned();
        if let Some(cached_result) = cached {
            return cached_result;
        }

        let files = match self.store.find_files_by_path_prefix(resolved_module) {
            Ok(files) => files,
            Err(_) => {
                // Cache negative result
                self.module_path_cache
                    .lock()
                    .unwrap()
                    .insert(cache_key, Vec::new());
                return Vec::new();
            }
        };

        let mut results = Vec::new();
        for file in &files {
            if let Ok(symbols) = self.store.find_symbols_by_file(&file.file_id) {
                results.extend(symbols.into_iter().filter(|s| s.name == target_name));
            }
        }

        // Cache the result (empty vec = known miss)
        self.module_path_cache
            .lock()
            .unwrap()
            .insert(cache_key, results.clone());

        results
    }

    /// Collect all project FileIds reachable through a file's import statements.
    ///
    /// For each import, resolves the module path using the same infrastructure as
    /// [`resolve_import`]: path_alias expansion, relative path resolution (via
    /// `store.find_files_by_path_prefix` which handles the DB-level lookup).
    /// Returns only FileIds — no symbol-level resolution.
    ///
    /// This is designed for S6 candidate-set reduction: for a file F with imports,
    /// S6 can prioritise symbols from these reachable files before falling back
    /// to the global index.
    pub fn collect_imported_file_ids(&self, imports: &[ImportDef]) -> HashSet<FileId> {
        let mut file_ids = HashSet::new();
        for import in imports {
            if import.module.is_empty() {
                continue;
            }
            // Resolve the module path using the same alias infrastructure as
            // resolve_import. For path-aliased imports (e.g. @lib/builder), this
            // expands the alias. Non-aliased modules pass through unchanged.
            let resolved_module = if self.path_alias.has_aliases() {
                self.path_alias
                    .resolve(&import.module)
                    .unwrap_or_else(|| import.module.clone())
            } else {
                import.module.clone()
            };
            // Look up files matching the resolved module path prefix.
            if let Ok(files) = self.store.find_files_by_path_prefix(&resolved_module) {
                for f in files {
                    file_ids.insert(f.file_id);
                }
            }
        }
        file_ids
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
                // imported_name is the actual name in the source module.
                // local_name is the alias used in the importing file (may differ).
                // We MUST look up the imported_name, not the alias.
                if !name.is_empty() {
                    candidates.push(name.clone());
                }
                if !local.is_empty() {
                    candidates.push(local.to_string());
                }
            }
            ImportKind::FromImport => {
                if !resolved_module.is_empty() && !name.is_empty() {
                    candidates.push(format!("{resolved_module}.{name}"));
                }
                // Also try with the original module path
                if module != &resolved_module && !module.is_empty() && !name.is_empty() {
                    candidates.push(format!("{module}.{name}"));
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
            // ExportFrom behaves like Import for candidate QName generation:
            // the re-exported name is looked up in the importing file's scope.
            ImportKind::ExportFrom => {
                if !name.is_empty() {
                    candidates.push(name.to_string());
                }
                if !local.is_empty() {
                    candidates.push(local.to_string());
                }
            }
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
use std::collections::{HashMap, HashSet};

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
            id: SymbolId::generate(&file_id, "typescript", name, kind.as_str(), None::<&str>),
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
            layer: "structural".to_string(),
        }
    }

    #[test]
    fn test_candidate_qnames_from_import() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let resolver = ImportResolver::new(store);

        let import = ImportDef {
            id: types::ids::ImportId::generate(
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
            id: types::ids::ImportId::generate(
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
            "expected src/utils in candidates, got: {candidates:?}"
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
                    status: types::enums::ParseStatus::Success,
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
                    status: types::enums::ParseStatus::Success,
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
                    status: types::enums::ParseStatus::Success,
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

    // ── QName cache tests ──

    #[test]
    fn qname_cache_hit_returns_cached_result() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Insert a symbol whose qualified name will be a candidate
        let file_id = FileId::generate("lib.ts");
        let sym = test_symbol(file_id, "foo", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "lib.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![sym],
                ..Default::default()
            })
            .unwrap();

        let resolver = ImportResolver::new(store);

        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "lib",
                Some("foo"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "lib".to_string(),
            imported_name: "foo".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        // First call: cache miss → DB query
        let results1 = resolver.resolve_import(&import).unwrap();
        // Second call: cache hit → no DB query, same result
        let results2 = resolver.resolve_import(&import).unwrap();

        assert_eq!(results1, results2);
        assert!(!results1.is_empty());
    }

    #[test]
    fn qname_cache_stores_negative_entry() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let resolver = ImportResolver::new(store);

        // Import a name that doesn't exist in the DB
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "missing",
                Some("no_such_symbol"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "missing".to_string(),
            imported_name: "no_such_symbol".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        // Both calls should return empty (second from negative cache)
        let results1 = resolver.resolve_import(&import).unwrap();
        let results2 = resolver.resolve_import(&import).unwrap();

        assert!(results1.is_empty(), "first call should have no results");
        assert!(
            results2.is_empty(),
            "second call should reuse negative cache entry"
        );
    }

    #[test]
    fn qname_cache_isolation() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Insert only "foo", not "bar"
        let file_id = FileId::generate("lib.ts");
        let sym_foo = test_symbol(file_id, "foo", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "lib.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![sym_foo],
                ..Default::default()
            })
            .unwrap();

        let resolver = ImportResolver::new(store);

        let make_import = |name: &str, seq: u32| ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "lib",
                Some(name),
                seq,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "lib".to_string(),
            imported_name: name.to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        // Prime cache with "foo"
        let results_foo = resolver.resolve_import(&make_import("foo", 0)).unwrap();
        assert!(!results_foo.is_empty());

        // Query "bar" — different QName, should NOT use foo's cache entry
        let results_bar = resolver.resolve_import(&make_import("bar", 1)).unwrap();
        assert!(
            results_bar.is_empty(),
            "bar should not match foo's cached result"
        );
    }

    /// Cache is scoped to the ImportResolver instance, not global.
    ///
    /// Verifies that a negative cache hit in one resolver does not poison
    /// a fresh resolver against the same store after a symbol is inserted.
    #[test]
    fn cache_scoped_to_resolver_instance() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Resolver A: query a QName that doesn't exist yet
        let resolver_a = ImportResolver::new(store.clone());
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "lib",
                Some("new_symbol"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "lib".to_string(),
            imported_name: "new_symbol".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };
        let results_a = resolver_a.resolve_import(&import).unwrap();
        assert!(
            results_a.is_empty(),
            "resolver A: expected empty (symbol not yet in store)"
        );

        // Insert the symbol into the store after resolver A's miss
        let file_id = FileId::generate("lib.ts");
        let sym = test_symbol(file_id, "new_symbol", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "lib.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![sym],
                ..Default::default()
            })
            .unwrap();

        // Resolver B (fresh, same store): should find the symbol
        let resolver_b = ImportResolver::new(store.clone());
        let results_b = resolver_b.resolve_import(&import).unwrap();
        assert!(
            !results_b.is_empty(),
            "resolver B: expected to find 'new_symbol' — cache is instance-scoped, not global"
        );
        assert_eq!(results_b[0].name, "new_symbol");
    }

    /// Within a single resolver instance, repeated lookups return cached results.
    #[test]
    fn within_instance_cache_hit_works() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let file_id = FileId::generate("lib.ts");
        let sym = test_symbol(file_id, "cached_sym", SymbolKind::Function);
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "lib.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "def".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![sym],
                ..Default::default()
            })
            .unwrap();

        let resolver = ImportResolver::new(store);
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "lib",
                Some("cached_sym"),
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "lib".to_string(),
            imported_name: "cached_sym".to_string(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        // First call: cache miss → DB query
        let r1 = resolver.resolve_import(&import).unwrap();
        assert!(!r1.is_empty());
        // Second call: cache hit → same result without DB query
        let r2 = resolver.resolve_import(&import).unwrap();
        assert_eq!(r1, r2);
    }

    // ── collect_imported_file_ids tests ───────────────────────────────────

    #[test]
    fn collect_imported_file_ids_empty_imports_returns_empty() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let resolver = ImportResolver::new(store);

        let result = resolver.collect_imported_file_ids(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn collect_imported_file_ids_resolves_path_alias() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Register a file reachable via the aliased path
        let lib_file = FileId::generate("src/lib/builder.ts");
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: lib_file,
                    path: "src/lib/builder.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![],
                ..Default::default()
            })
            .unwrap();

        // Path alias: @lib/* → src/lib/*
        let mut paths = HashMap::new();
        paths.insert("@lib/*".to_string(), vec!["src/lib/*".to_string()]);
        let path_alias = PathAliasResolver {
            base_url: None,
            paths,
        };
        let resolver = ImportResolver::with_path_alias(store, path_alias);

        // import from '@lib/builder' — should resolve via alias
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "@lib/builder",
                None,
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "@lib/builder".to_string(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        let file_ids = resolver.collect_imported_file_ids(&[import]);
        assert!(
            file_ids.contains(&lib_file),
            "aliased import should resolve to lib_file"
        );
    }

    #[test]
    fn collect_imported_file_ids_normal_module_path() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        // Register a file reachable by a normal path prefix
        let utils_file = FileId::generate("lib/utils.ts");
        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: utils_file,
                    path: "lib/utils.ts".to_string(),
                    language: Language::TypeScript,
                    content_hash: "abc".to_string(),
                    status: types::enums::ParseStatus::Success,
                },
                symbols: vec![],
                ..Default::default()
            })
            .unwrap();

        let resolver = ImportResolver::new(store);

        // import from 'lib/utils' — prefix matches the stored path
        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "lib/utils",
                None,
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: "lib/utils".to_string(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        let file_ids = resolver.collect_imported_file_ids(&[import]);
        assert!(
            file_ids.contains(&utils_file),
            "normal module path should resolve to utils_file"
        );
    }

    #[test]
    fn collect_imported_file_ids_skips_empty_modules() {
        let store = std::sync::Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let resolver = ImportResolver::new(store);

        let import = ImportDef {
            id: ImportId::generate(
                &FileId::generate("main.ts"),
                ImportKind::Import.as_str(),
                "",
                None,
                0,
            ),
            file_id: FileId::generate("main.ts"),
            kind: ImportKind::Import,
            module: String::new(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        };

        let result = resolver.collect_imported_file_ids(&[import]);
        assert!(result.is_empty());
    }
}
