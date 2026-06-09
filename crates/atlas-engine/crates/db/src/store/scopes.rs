//! Scopes and imports: insert + query by file.

use std::collections::HashSet;

use rusqlite::params;
use types::*;

use super::Store;
use crate::store_writers::{write_imports, write_scopes};

impl Store {
    // ── Scopes ──────────────────────────────────────────────────────────────

    /// Batch-insert scopes.
    pub fn insert_scopes(&self, scopes: &[ScopeDef]) -> anyhow::Result<()> {
        if scopes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_scopes(tx, scopes))
    }

    /// Find all scopes for a file.
    pub fn find_scopes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ScopeDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT scope_id, file_id, kind, name, scope_path, parent_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM scopes WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            let kind_str: String = row.get(2)?;
            let kind = ScopeKind::from_str(&kind_str).unwrap_or_else(|| {
                tracing::warn!(%kind_str, "Unknown ScopeKind, defaulting to default");
                Default::default()
            });
            Ok(ScopeDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind,
                name: row.get(3)?,
                scope_path: row.get(4)?,
                parent_id: row.get(5)?,
                range: TextRange {
                    start_byte: row.get(6)?,
                    end_byte: row.get(7)?,
                    start_line: row.get(8)?,
                    start_column: row.get(9)?,
                    end_line: row.get(10)?,
                    end_column: row.get(11)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Imports ─────────────────────────────────────────────────────────────

    /// Batch-insert imports.
    pub fn insert_imports(&self, imports: &[ImportDef]) -> anyhow::Result<()> {
        if imports.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_imports(tx, imports))
    }

    /// Find all imports for a file.
    pub fn find_imports_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ImportDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT import_id, file_id, kind, module, imported_name, local_name,
                    is_wildcard, is_relative, alias,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM imports WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            let kind_str: String = row.get(2)?;
            let kind = ImportKind::from_str(&kind_str).unwrap_or_else(|| {
                tracing::warn!(%kind_str, "Unknown ImportKind, defaulting to default");
                Default::default()
            });
            Ok(ImportDef {
                id: row.get(0)?,
                file_id: row.get(1)?,
                kind,
                module: row.get(3)?,
                imported_name: row.get(4)?,
                local_name: row.get(5)?,
                is_wildcard: row.get(6)?,
                is_relative: row.get(7)?,
                alias: row.get(8)?,
                range: TextRange {
                    start_byte: row.get(9)?,
                    end_byte: row.get(10)?,
                    start_line: row.get(11)?,
                    start_column: row.get(12)?,
                    end_line: row.get(13)?,
                    end_column: row.get(14)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find files that import a given file (reverse dependencies / dependents).
    ///
    /// Returns tuples of (importing_file_path, import_module_string).
    /// This is a best-effort O(N) scan over all imports; for large projects
    /// consider building an in-memory dependency index.
    ///
    /// # Resolution strategies
    ///
    /// **Path A** – LIKE substring match on `module`.  Works when the stored
    /// module string already contains the target path as a substring (e.g.
    /// TypeScript imports with full paths, or npm-like package names).
    ///
    /// **Path B** – Relative import resolution.  Handles languages where
    /// imports use bare filenames (`#include "helper.h"`), directory-relative
    /// paths (`./utils` → `src/utils`), or paths missing file extensions.
    /// This path covers both C/C++ `#include` directives and TypeScript /
    /// JavaScript / Python `import` statements.
    ///
    /// Both paths contribute candidates, and duplicates are removed.
    pub fn find_dependents_by_file(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.lock_read();

        // Get the target file's path for matching
        let target_path: String = conn.query_row(
            "SELECT path FROM files WHERE file_id = ?1",
            params![file_id],
            |row| row.get(0),
        )?;

        // Dedup accumulator – both Path A and Path B contribute candidates.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut results: Vec<(String, String)> = Vec::new();
        let mut insert = |importing_path: String, module: String| {
            if seen.insert((importing_path.clone(), module.clone())) {
                results.push((importing_path, module));
            }
        };

        // ── Path A: Standard path-substring LIKE query ──────────────────
        // Works for TypeScript, Python, Java etc. where module stores
        // relative paths like "./foo/bar" or "react".
        {
            let mut stmt = conn.prepare(
                "SELECT f.path, i.module
                 FROM imports i
                 JOIN files f ON f.file_id = i.file_id
                 WHERE i.module LIKE ?1
                 ORDER BY f.path",
            )?;
            let pattern = format!("%{target_path}%");
            let rows: Vec<(String, String)> = stmt
                .query_map(params![pattern], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(
                            ?e,
                            "Dependent import row decode error (LIKE path), skipping"
                        );
                        None
                    }
                })
                .collect();
            for (path, module) in rows {
                insert(path, module);
            }
        }

        // ── Path B: Relative import resolution ──────────────────────────
        // Handles both C/C++ includes AND TS/JS/Python relative imports.
        // Now covers all relative imports (is_relative=1), not just includes.
        let target_basename = if let Some(pos) = target_path.rfind('/') {
            &target_path[pos + 1..]
        } else {
            &target_path
        };

        {
            let mut stmt = conn.prepare(
                "SELECT f.file_id, f.path, i.module, i.kind
                 FROM imports i
                 JOIN files f ON f.file_id = i.file_id
                 WHERE i.is_relative = 1
                 ORDER BY f.path",
            )?;
            let candidate_rows: Vec<(FileId, String, String, String)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, FileId>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .filter_map(|r| match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(?e, "Include-import row decode error, skipping");
                        None
                    }
                })
                .collect();

            for (_importing_fid, importing_path, module, kind) in candidate_rows {
                // Strategy 1: Bare basename match — `helper.h` == `helper.h`
                if module == target_basename {
                    insert(importing_path, module);
                    continue;
                }

                // Strategy 2: Relative path resolved against importing
                // file's directory.  e.g., importing `src/main.c` with
                // `#include "helper.h"` → check `src/helper.h`.
                // Path normalization handles "." and ".." components in
                // TS/JS specifiers like `./utils` or `../foo/bar`.
                if let Some(parent_dir) = std::path::Path::new(&importing_path).parent() {
                    let raw = parent_dir.join(&module);
                    // Manually normalize: pop on ParentDir, skip CurDir.
                    let mut resolved = std::path::PathBuf::new();
                    for c in raw.components() {
                        match c {
                            std::path::Component::ParentDir => {
                                resolved.pop();
                            }
                            std::path::Component::CurDir => {}
                            other => {
                                resolved.push(other.as_os_str());
                            }
                        }
                    }
                    let resolved_str = resolved.to_string_lossy();
                    if resolved_str == target_path {
                        insert(importing_path, module);
                        continue;
                    }

                    // Strategy 3 (non-include kinds only): Extension and
                    // index resolution.  TS/JS imports like `./utils` may
                    // resolve to `./utils/index.ts` or `./utils.ts`.
                    if kind != "include" {
                        // Try appending extensions
                        for ext in [".ts", ".tsx", ".js", ".jsx", ".py"] {
                            let with_ext = format!("{resolved_str}{ext}");
                            if with_ext == target_path {
                                insert(importing_path.clone(), module.clone());
                                break;
                            }
                        }
                        // If no extension match, try index variants
                        for idx_ext in
                            ["index.ts", "index.tsx", "index.js", "index.jsx"]
                        {
                            let with_index = resolved.join(idx_ext);
                            if with_index.to_string_lossy() == target_path {
                                insert(importing_path.clone(), module.clone());
                                break;
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a fresh in-memory store with schema initialized.
    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    /// Insert a file row so FK constraints are satisfied.
    fn insert_test_file(store: &Store, path: &str) -> FileId {
        let fid = FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.to_string(),
                language: Language::TypeScript,
                content_hash: "test".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        fid
    }

    /// Insert a file with a specific language.
    fn insert_test_file_lang(store: &Store, path: &str, lang: Language) -> FileId {
        let fid = FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.to_string(),
                language: lang,
                content_hash: "test".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        fid
    }

    /// Build an ImportDef with minimal fields for dependency tests.
    fn make_import(
        file_id: &FileId,
        kind: ImportKind,
        module: &str,
        is_relative: bool,
    ) -> ImportDef {
        ImportDef {
            id: ImportId::generate(
                file_id,
                kind.as_str(),
                module,
                None,
                0,
            ),
            file_id: *file_id,
            kind,
            module: module.to_string(),
            imported_name: String::new(),
            local_name: None,
            is_wildcard: false,
            is_relative,
            range: TextRange::default(),
            alias: None,
        }
    }

    // ── Tests ────────────────────────────────────────────────────────────

    /// Path A (LIKE) finds dependents when the module string contains
    /// the target file path as a substring.
    #[test]
    fn test_find_dependents_like_match() {
        let store = test_store();

        let target_fid = insert_test_file(&store, "src/github/index.ts");
        let a_fid = insert_test_file(&store, "src/a.ts");
        let b_fid = insert_test_file(&store, "src/b.ts");

        // These module strings contain "src/github/index.ts" as a substring.
        store
            .insert_imports(&[
                make_import(&a_fid, ImportKind::Import, "src/github/index.ts", false),
                make_import(&b_fid, ImportKind::Import, "../src/github/index.ts", true),
            ])
            .unwrap();

        let deps = store
            .find_dependents_by_file(&target_fid)
            .unwrap();

        let paths: Vec<&str> = deps.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(deps.len(), 2);
        assert!(paths.contains(&"src/a.ts"));
        assert!(paths.contains(&"src/b.ts"));
    }

    /// Path B resolves TS/JS relative imports using extension/index fallback.
    #[test]
    fn test_find_dependents_relative_import() {
        let store = test_store();

        let target_fid = insert_test_file(&store, "src/utils/index.ts");
        let a_fid = insert_test_file(&store, "src/a.ts");
        let b_fid = insert_test_file(&store, "src/subdir/b.ts");

        // "./utils" resolves to "src/utils" → index fallback → "src/utils/index.ts"
        // "../utils" resolves to "src/utils" (normalized) → index → "src/utils/index.ts"
        store
            .insert_imports(&[
                make_import(&a_fid, ImportKind::Import, "./utils", true),
                make_import(&b_fid, ImportKind::Import, "../utils", true),
            ])
            .unwrap();

        let deps = store
            .find_dependents_by_file(&target_fid)
            .unwrap();

        let paths: Vec<&str> = deps.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(deps.len(), 2, "expected 2 dependents, got {deps:?}");
        assert!(paths.contains(&"src/a.ts"));
        assert!(paths.contains(&"src/subdir/b.ts"));
    }

    /// Dedup prevents the same (path, module) pair from appearing twice
    /// when matched by both Path A and Path B.
    #[test]
    fn test_find_dependents_no_duplicates() {
        let store = test_store();

        let target_fid = insert_test_file(&store, "helpers.ts");
        let a_fid = insert_test_file(&store, "app.ts");

        // "helpers.ts" is matched by Path A (LIKE) AND Path B (bare basename).
        store
            .insert_imports(&[
                make_import(&a_fid, ImportKind::Import, "helpers.ts", true),
            ])
            .unwrap();

        let deps = store
            .find_dependents_by_file(&target_fid)
            .unwrap();

        assert_eq!(
            deps.len(),
            1,
            "duplicates detected: {deps:?}"
        );
        assert_eq!(deps[0].0, "app.ts");
    }

    /// Path B still resolves C/C++ `#include` directives as before.
    #[test]
    fn test_find_dependents_c_include() {
        let store = test_store();

        let target_fid = insert_test_file_lang(&store, "src/helper.h", Language::C);
        let main_fid = insert_test_file_lang(&store, "src/main.c", Language::C);
        let other_fid = insert_test_file_lang(&store, "src/other.c", Language::C);

        // "helper.h" → bare basename match
        // "../src/helper.h" → directory resolution (normalized)
        store
            .insert_imports(&[
                make_import(&main_fid, ImportKind::Include, "helper.h", true),
                make_import(&other_fid, ImportKind::Include, "../src/helper.h", true),
            ])
            .unwrap();

        let deps = store
            .find_dependents_by_file(&target_fid)
            .unwrap();

        let paths: Vec<&str> = deps.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(deps.len(), 2, "expected 2 dependents, got {deps:?}");
        assert!(paths.contains(&"src/main.c"));
        assert!(paths.contains(&"src/other.c"));
    }
}
