//! ClosurePlanner — dependency-closure-aware lazy extraction planning.
//!
//! Given a seed file (e.g., `bar.ts` that imports `./foo`), the
//! ClosurePlanner expands the build set to include dependency files
//! BEFORE the seed is built, ensuring symbols from dependencies are
//! resolvable during cross-file reference resolution.
//!
//! # Design
//!
//! - BFS from seed through imports table, resolving module strings to
//!   file_ids via directory-join normalization.
//! - Bare imports (`is_relative = false`) are treated as external
//!   and returned as `None`.
//! - Does NOT depend on GraphEngine — uses only the imports table
//!   and files table from the store, which are stable during lazy
//!   building.
//!
//! # Limits
//!
//! Defaults: max_depth=2, max_closure_files=64. Override with
//! [`ClosurePlanner::with_limits`].

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use regex::Regex;
use types::ids::{FileId, ImportId};
use types::structs::ImportDef;
use types::{ImportKind, layer, status};

/// A directory to search when resolving angle-bracket includes (`#include <...>`).
///
/// Project-relative path, e.g. `"include"`, `"arch/x86/include"`.
#[derive(Debug, Clone)]
pub struct IncludeRoot {
    /// Project-relative directory path (uses `/` separator on all platforms).
    pub path: String,
}

/// Snapshot of a seed file's dependency graph.
#[derive(Debug, Clone)]
pub struct DependencyClosure {
    pub seed_file: FileId,
    /// Depth 1: files the seed directly imports.
    pub direct_deps: Vec<FileId>,
    /// Depth 2+: files that dependencies import.
    pub transitive_deps: Vec<FileId>,
    /// Dependencies that are missing a structural layer.
    pub missing_structural: Vec<FileId>,
    /// Dependencies that are missing a resolution_symbols layer
    /// (and not already covered by missing_structural).
    pub missing_resolution_symbols: Vec<FileId>,
    /// Dependencies that are missing even a manifest layer.
    pub missing_manifest: Vec<FileId>,
    /// Deepest BFS level reached in this closure.
    pub max_depth_reached: usize,
    /// Total number of unique files in the closure (incl. seed).
    pub total_files: usize,
}

/// Ordered build list for lazy extraction: dependencies first, seed last.
#[derive(Debug)]
pub struct PrioritizedWorkset {
    pub order: Vec<FileId>,
}

/// Plans dependency closures for lazy extraction.
pub struct ClosurePlanner {
    store: Arc<Store>,
    project_root: Option<std::path::PathBuf>,
    include_roots: Vec<IncludeRoot>,
    max_depth: usize,
    max_closure_files: usize,
}

impl ClosurePlanner {
    pub fn new(store: Arc<Store>, project_root: Option<std::path::PathBuf>) -> Self {
        Self {
            store,
            project_root,
            include_roots: Vec::new(),
            max_depth: 2,
            max_closure_files: 64,
        }
    }

    /// Override the default BFS depth and file-count limits.
    pub fn with_limits(mut self, max_depth: usize, max_closure_files: usize) -> Self {
        self.max_depth = max_depth;
        self.max_closure_files = max_closure_files;
        self
    }

    /// Set include roots for angle-bracket include resolution.
    ///
    /// Each root is a project-relative directory to search when resolving
    /// `#include <...>` directives (C/C++).
    pub fn with_include_roots(mut self, roots: Vec<IncludeRoot>) -> Self {
        self.include_roots = roots;
        self
    }

    /// Compute the dependency closure for a seed file.
    ///
    /// BFS from seed through imports, resolving module strings to file_ids.
    /// Respects [`self.max_depth`] and [`self.max_closure_files`].
    pub fn plan_closure(&self, seed: &FileId) -> Result<DependencyClosure> {
        let mut visited: HashSet<FileId> = HashSet::new();
        let mut queue: VecDeque<FileId> = VecDeque::new();

        visited.insert(*seed);
        queue.push_back(*seed);

        let mut direct_deps: Vec<FileId> = Vec::new();
        let mut transitive_deps: Vec<FileId> = Vec::new();
        let mut missing_structural: Vec<FileId> = Vec::new();
        let mut missing_resolution_symbols: Vec<FileId> = Vec::new();
        let mut missing_manifest: Vec<FileId> = Vec::new();
        let mut max_depth_reached: usize = 0;

        // Classify the seed file itself
        self.classify_file(
            seed,
            &mut missing_structural,
            &mut missing_resolution_symbols,
            &mut missing_manifest,
        );

        for depth in 0..self.max_depth {
            let level_size = queue.len();
            if level_size == 0 {
                break;
            }
            max_depth_reached = depth;

            // Collect files at this BFS level
            let mut current_level: Vec<FileId> = Vec::with_capacity(level_size);
            for _ in 0..level_size {
                current_level.push(queue.pop_front().unwrap());
            }

            for file_id in &current_level {
                // Classify dep files at this depth
                if depth > 0 {
                    self.classify_file(
                        file_id,
                        &mut missing_structural,
                        &mut missing_resolution_symbols,
                        &mut missing_manifest,
                    );
                }

                let importing_file_dir = match self.get_file_dir(file_id) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!("ClosurePlanner: skipping file {:?}: {:#}", file_id, e);
                        continue;
                    }
                };

                let imports = match self.store.find_imports_by_file(file_id) {
                    Ok(imports) => imports,
                    Err(e) => {
                        tracing::debug!(
                            "ClosurePlanner: cannot read imports for {:?}: {:#}",
                            file_id,
                            e
                        );
                        continue;
                    }
                };

                for import in &imports {
                    // Bail if we've hit the file-count limit
                    if visited.len() >= self.max_closure_files {
                        break;
                    }

                    let target_id = match self.resolve_import_target(
                        &importing_file_dir,
                        &import.module,
                        import.is_relative,
                    ) {
                        Ok(Some(id)) => id,
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::debug!(
                                "ClosurePlanner: resolution failed for {:?} import {:?}: {:#}",
                                file_id,
                                import.module,
                                e
                            );
                            continue;
                        }
                    };

                    if !visited.contains(&target_id) {
                        visited.insert(target_id);
                        queue.push_back(target_id);

                        if depth == 0 {
                            direct_deps.push(target_id);
                        } else {
                            transitive_deps.push(target_id);
                        }
                    }
                }

                if visited.len() >= self.max_closure_files {
                    break;
                }
            }

            if visited.len() >= self.max_closure_files {
                break;
            }
        }

        // Collect files still in queue as unprocessed dependencies
        let _unprocessed: Vec<FileId> = queue.into_iter().collect();

        Ok(DependencyClosure {
            seed_file: *seed,
            direct_deps,
            transitive_deps,
            missing_structural,
            missing_resolution_symbols,
            missing_manifest,
            max_depth_reached,
            total_files: visited.len(),
        })
    }

    /// Build a prioritized workset from a closure.
    ///
    /// Order: direct deps first, then transitive deps, seed file last.
    /// Dependencies come BEFORE the seed, so resolution sees stable symbols.
    pub fn prioritize(&self, closure: &DependencyClosure) -> PrioritizedWorkset {
        let mut order: Vec<FileId> = Vec::new();
        let mut seen: HashSet<FileId> = HashSet::new();

        for dep in closure
            .direct_deps
            .iter()
            .chain(closure.transitive_deps.iter())
        {
            if seen.insert(*dep) {
                order.push(*dep);
            }
        }

        // Seed file last — so resolution sees its dependencies' symbols
        if seen.insert(closure.seed_file) {
            order.push(closure.seed_file);
        }

        PrioritizedWorkset { order }
    }

    /// Convenience: plan + prioritize in one call.
    pub fn plan_for_seed(&self, seed: &FileId) -> Result<PrioritizedWorkset> {
        // Bootstrap: if seed is manifest-only and has no imports in DB,
        // scan the source file directly for import statements.
        let _ = self.bootstrap_imports_from_source(seed);

        // Discover same-name companion files (e.g., "foo.c" ↔ "foo.h").
        let siblings = self.discover_sibling_files(seed)?;

        let mut closure = self.plan_closure(seed)?;

        // Inject discovered sibling files as additional direct deps, so
        // they are built before the seed during lazy extraction.
        for sibling_id in &siblings {
            if !closure.direct_deps.contains(sibling_id)
                && !closure.transitive_deps.contains(sibling_id)
                && *sibling_id != closure.seed_file
            {
                closure.direct_deps.push(*sibling_id);
                closure.total_files += 1;
            }
        }

        Ok(self.prioritize(&closure))
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Classify a file for missing layers.
    fn classify_file(
        &self,
        file_id: &FileId,
        missing_structural: &mut Vec<FileId>,
        missing_resolution_symbols: &mut Vec<FileId>,
        missing_manifest: &mut Vec<FileId>,
    ) {
        if !self.has_complete_layer(file_id, layer::STRUCTURAL) {
            missing_structural.push(*file_id);
        }
        // resolution_symbols: structural is a superset, so only track when
        // neither structural nor resolution_symbols is complete.
        if !self.has_complete_layer(file_id, layer::STRUCTURAL)
            && !self.has_complete_layer(file_id, layer::RESOLUTION_SYMBOLS)
        {
            missing_resolution_symbols.push(*file_id);
        }
        if !self.has_complete_layer(file_id, layer::MANIFEST) {
            missing_manifest.push(*file_id);
        }
    }

    /// Check whether a file has a complete layer matching its current
    /// content hash.
    fn has_complete_layer(&self, file_id: &FileId, layer_name: &str) -> bool {
        let file_info = match self.store.get_file(file_id) {
            Ok(Some(fi)) => fi,
            _ => return false,
        };

        match self.store.get_file_index_layer(file_id, layer_name) {
            Ok(Some((s, hash))) => s == status::COMPLETE && hash == file_info.content_hash,
            _ => false,
        }
    }

    /// Get the parent directory of a file's project-relative path.
    ///
    /// Returns empty string for root-level files.
    fn get_file_dir(&self, file_id: &FileId) -> Result<String> {
        let file_info = self
            .store
            .get_file(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found for id {:?}", file_id))?;

        let dir = std::path::Path::new(&file_info.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(dir)
    }

    /// Resolve an import module string to a target file_id.
    ///
    /// For relative imports: combines the importing file's directory with
    /// the module path, normalises (resolves `.` and `..`), generates a
    /// [`FileId`], and checks the files table for existence.
    ///
    /// For non-relative imports (angle-bracket `#include <...>`): searches
    /// the configured include roots via [`resolve_angle_include`].
    fn resolve_import_target(
        &self,
        importing_file_dir: &str,
        module: &str,
        is_relative: bool,
    ) -> Result<Option<FileId>> {
        if is_relative {
            let candidate = std::path::Path::new(importing_file_dir).join(module);
            let normalized = normalize_path(&candidate);

            if normalized.is_empty() {
                return Ok(None);
            }

            let file_id = FileId::generate(&normalized);
            match self.store.get_file(&file_id)? {
                Some(_) => Ok(Some(file_id)),
                None => Ok(None),
            }
        } else {
            // Try include roots for angle-bracket includes
            self.resolve_angle_include(module)
        }
    }

    // Private helper needed for fn resolve_import_target - dead_code warning
    #[allow(dead_code)]
    fn _project_root(&self) -> Option<&std::path::Path> {
        self.project_root.as_deref()
    }

    /// Resolve an angle-bracket include module string by searching the
    /// configured include roots.
    ///
    /// For each root directory, joins the module path, normalises, and
    /// checks the files table for existence. Returns the first match.
    fn resolve_angle_include(&self, module: &str) -> Result<Option<FileId>> {
        for root in &self.include_roots {
            let candidate = std::path::Path::new(&root.path).join(module);
            let normalized = normalize_path(&candidate);
            if normalized.is_empty() {
                continue;
            }
            let file_id = FileId::generate(&normalized);
            if self.store.get_file(&file_id)?.is_some() {
                return Ok(Some(file_id));
            }
        }
        Ok(None)
    }

    /// Discover same-name companion files (e.g., "foo.c" ↔ "foo.h").
    ///
    /// When processing a seed `.c` file, discovers a same-name `.h` in the
    /// same directory; when processing a `.h` file, discovers a same-name
    /// `.c`.  Other extensions (`.cc`, `.cpp`, `.cxx`, `.hpp`, `.hxx`) are
    /// also handled.
    fn discover_sibling_files(&self, file_id: &FileId) -> Result<Vec<FileId>> {
        let file_info = match self.store.get_file(file_id)? {
            Some(fi) => fi,
            None => return Ok(vec![]),
        };
        let path = std::path::Path::new(&file_info.path);
        let parent = match path.parent() {
            Some(p) => p,
            None => return Ok(vec![]),
        };
        let stem = match path.file_stem() {
            Some(s) => s.to_string_lossy().to_string(),
            None => return Ok(vec![]),
        };
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();

        // Determine the companion extension
        let companion_ext = if ext == "c" || ext == "cc" || ext == "cpp" || ext == "cxx" {
            "h"
        } else if ext == "h" || ext == "hpp" || ext == "hxx" {
            "c"
        } else {
            return Ok(vec![]);
        };

        // Check if companion file exists in DB
        let companion_path = parent.join(format!("{}.{}", stem, companion_ext));
        let normalized = normalize_path(&companion_path);
        let companion_id = FileId::generate(&normalized);
        match self.store.get_file(&companion_id)? {
            Some(_) => Ok(vec![companion_id]),
            None => Ok(vec![]),
        }
    }

    /// Bootstrap imports for a seed file by scanning source directly.
    ///
    /// This is used when the seed file is manifest-only and has no imports
    /// in the database.  Scans for C #include and C++ include patterns
    /// directly from source text.
    ///
    /// Errors are logged but not propagated — the caller falls back to
    /// DB-only import lookup.
    fn bootstrap_imports_from_source(&self, file_id: &FileId) -> Result<()> {
        // Only bootstrap if no imports exist yet
        match self.store.find_imports_by_file(file_id) {
            Ok(imports) if !imports.is_empty() => return Ok(()),
            Err(_) => { /* proceed with bootstrap */ }
            _ => { /* proceed with bootstrap */ }
        }

        let file_info = self
            .store
            .get_file(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found: {:?}", file_id))?;

        let resolved_path = self.resolve_source_path(&file_info.path);
        let source = match std::fs::read_to_string(&resolved_path) {
            Ok(s) => s,
            Err(_) => return Ok(()), // File not on disk — skip
        };

        // Scan for include/import patterns
        let imports = scan_c_includes(file_id, &file_info.path, &source);

        // Write discovered imports to DB
        if !imports.is_empty() {
            self.store.insert_imports(&imports)?;
        }
        Ok(())
    }

    fn resolve_source_path(&self, relative: &str) -> std::path::PathBuf {
        match &self.project_root {
            Some(root) => root.join(relative),
            None => std::path::PathBuf::from(relative),
        }
    }
}

// ── C include scanner ───────────────────────────────────────────────────────

/// Scan C source for `#include` directives.
///
/// Lightweight regex-based scanner for `#include "..."` and `#include <...>`
/// patterns.  Used as a fallback when the manifest index lacks import rows.
///
/// Distinguishes local from system includes per the C standard:
/// - `#include "..."` → always relative (searches including file's directory first)
/// - `#include <...>` → never relative (system/library include paths)
pub(crate) fn scan_c_includes(file_id: &FileId, _file_path: &str, source: &str) -> Vec<ImportDef> {
    // Separate patterns for quoted (local) vs angle (system) includes
    let quote_re = Regex::new(r##"#include\s+"([^"]+)""##).unwrap();
    let angle_re = Regex::new(r#"#include\s+<([^>]+)>"#).unwrap();
    let mut imports = Vec::new();

    // Quoted includes: always relative (C standard §6.10.2)
    for (cap_idx, cap) in quote_re.captures_iter(source).enumerate() {
        let module = cap[1].to_string();
        let start_byte = cap.get(0).map_or(cap_idx as u32, |m| m.start() as u32);
        let import_id = ImportId::generate(file_id, "include", &module, None, start_byte);
        imports.push(ImportDef {
            id: import_id,
            file_id: *file_id,
            kind: ImportKind::Include,
            module,
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: true,
            range: Default::default(),
        });
    }

    // Angle includes: never relative (system/external headers)
    for (cap_idx, cap) in angle_re.captures_iter(source).enumerate() {
        let module = cap[1].to_string();
        let start_byte = cap.get(0).map_or(cap_idx as u32, |m| m.start() as u32);
        let import_id = ImportId::generate(file_id, "include", &module, None, start_byte);
        imports.push(ImportDef {
            id: import_id,
            file_id: *file_id,
            kind: ImportKind::Include,
            module,
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: Default::default(),
        });
    }

    imports
}

// ── Path normalisation (no filesystem access) ─────────────────────────────

/// Normalise a path by resolving `.` and `..` components without
/// touching the filesystem.
///
/// - `CurDir` components are dropped.
/// - `ParentDir` components pop the preceding component.
/// - Windows separators are *not* handled — the DB uses `/` on all
///   platforms and this crate targets Unix hosts.
fn normalize_path(path: &std::path::Path) -> String {
    let mut components: Vec<&std::ffi::OsStr> = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => { /* skip */ }
            std::path::Component::ParentDir => {
                components.pop();
            }
            other => components.push(other.as_os_str()),
        }
    }

    let mut result = String::new();
    for (i, c) in components.iter().enumerate() {
        if i > 0 {
            result.push('/');
        }
        result.push_str(&c.to_string_lossy());
    }
    result
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::{Language, ParseStatus};
    use types::structs::FileInfo;

    #[test]
    fn normalize_path_drops_dot() {
        let p = std::path::Path::new("src/./main.rs");
        assert_eq!(normalize_path(p), "src/main.rs");
    }

    #[test]
    fn normalize_path_resolves_parent_dir() {
        let p = std::path::Path::new("src/subdir/../main.rs");
        assert_eq!(normalize_path(p), "src/main.rs");
    }

    #[test]
    fn normalize_path_multiple_parent_dir() {
        let p = std::path::Path::new("a/b/c/../../d");
        assert_eq!(normalize_path(p), "a/d");
    }

    #[test]
    fn normalize_path_no_components_returns_empty() {
        let p = std::path::Path::new(".");
        assert_eq!(normalize_path(p), "");
    }

    #[test]
    fn normalize_path_root_parent_is_dropped() {
        let p = std::path::Path::new("../foo");
        assert_eq!(normalize_path(p), "foo");
    }

    // ── Integration tests (ignored — require multi-file DB setup) ──────

    // ── Bootstrap scanner tests ─────────────────────────────────────

    #[test]
    fn bootstrap_scanner_quote_vs_angle() {
        // Verify that scan_c_includes correctly distinguishes local
        // `#include "..."` (is_relative=true) from system `<...>`
        // (is_relative=false).
        use types::ids::FileId;

        let file_id = FileId::generate("src/main.c");
        let source = concat!(
            "#include \"util.h\"\n",
            "#include <stdio.h>\n",
            "#include \"dir/helper.h\"\n",
            "#include <stdlib.h>\n",
            "int main() { return 0; }\n",
        );

        let imports = scan_c_includes(&file_id, "src/main.c", source);

        assert_eq!(imports.len(), 4, "should discover all 4 includes");

        // Quoted includes are always relative
        let util = imports.iter().find(|i| i.module == "util.h").unwrap();
        assert!(util.is_relative, "#include \"util.h\" must be relative");

        let helper = imports.iter().find(|i| i.module == "dir/helper.h").unwrap();
        assert!(
            helper.is_relative,
            "#include \"dir/helper.h\" must be relative"
        );

        // Angle includes are never relative
        let stdio = imports.iter().find(|i| i.module == "stdio.h").unwrap();
        assert!(
            !stdio.is_relative,
            "#include <stdio.h> must NOT be relative"
        );

        let stdlib = imports.iter().find(|i| i.module == "stdlib.h").unwrap();
        assert!(
            !stdlib.is_relative,
            "#include <stdlib.h> must NOT be relative"
        );
    }

    #[test]
    #[ignore = "Phase 2: needs multi-file DB setup with imports"]
    fn closure_plan_discovers_direct_deps() {
        // Setup: bar.ts imports ./foo. Verify direct_deps contains foo.
    }

    #[test]
    #[ignore = "Phase 2: needs multi-file DB setup with imports"]
    fn closure_plan_discovers_transitive_deps() {
        // Setup: bar.ts → ./foo → ./baz. Verify transitive_deps contains baz.
    }

    #[test]
    #[ignore = "Phase 2: needs multi-file DB setup with imports"]
    fn bare_imports_are_external() {
        // Setup: bar.ts imports "react" (is_relative=false).
        // Verify resolve_import_target returns None.
    }

    #[test]
    #[ignore = "Phase 2: needs multi-file DB setup with imports"]
    fn prioritize_deps_before_seed() {
        // Setup: seed imports dep. Verify workset.order has dep before seed.
    }

    // ── Include root resolution tests ─────────────────────────────────

    #[test]
    fn test_resolve_angle_include_with_roots() {
        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        // Register files at include/linux/fs.h and arch/x86/include/asm/foo.h
        let fs_h_id = FileId::generate("include/linux/fs.h");
        let foo_h_id = FileId::generate("arch/x86/include/asm/foo.h");

        store
            .upsert_file(&FileInfo {
                file_id: fs_h_id,
                path: "include/linux/fs.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: foo_h_id,
                path: "arch/x86/include/asm/foo.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let planner = ClosurePlanner::new(store, None).with_include_roots(vec![
            IncludeRoot {
                path: "include".to_string(),
            },
            IncludeRoot {
                path: "arch/x86/include".to_string(),
            },
        ]);

        // Resolve angle-bracket includes through include roots
        let fs_h = planner.resolve_angle_include("linux/fs.h").unwrap();
        assert!(
            fs_h.is_some(),
            "linux/fs.h should resolve via include/ root"
        );
        assert_eq!(fs_h.unwrap(), fs_h_id);

        let foo_h = planner.resolve_angle_include("asm/foo.h").unwrap();
        assert!(
            foo_h.is_some(),
            "asm/foo.h should resolve via arch/x86/include/ root"
        );
        assert_eq!(foo_h.unwrap(), foo_h_id);

        // A non-existent module should return None
        let missing = planner.resolve_angle_include("nonexistent/baz.h").unwrap();
        assert!(missing.is_none(), "unknown module should return None");
    }

    // ── Sibling discovery tests ───────────────────────────────────────

    #[test]
    fn test_sibling_discovery_c_to_h() {
        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let main_c_id = FileId::generate("kernel/main.c");
        let main_h_id = FileId::generate("kernel/main.h");

        store
            .upsert_file(&FileInfo {
                file_id: main_c_id,
                path: "kernel/main.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: main_h_id,
                path: "kernel/main.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let planner = ClosurePlanner::new(store, None);

        // From main.c, discover main.h
        let siblings = planner.discover_sibling_files(&main_c_id).unwrap();
        assert_eq!(siblings.len(), 1, "should discover one sibling file");
        assert_eq!(siblings[0], main_h_id, "should discover main.h from main.c");
    }

    #[test]
    fn test_sibling_discovery_h_to_c() {
        let store = std::sync::Arc::new({
            let s = Store::open_in_memory().unwrap();
            s.init_schema().unwrap();
            s
        });

        let main_c_id = FileId::generate("kernel/main.c");
        let main_h_id = FileId::generate("kernel/main.h");

        store
            .upsert_file(&FileInfo {
                file_id: main_c_id,
                path: "kernel/main.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: main_h_id,
                path: "kernel/main.h".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        let planner = ClosurePlanner::new(store, None);

        // From main.h, discover main.c
        let siblings = planner.discover_sibling_files(&main_h_id).unwrap();
        assert_eq!(siblings.len(), 1, "should discover one sibling file");
        assert_eq!(siblings[0], main_c_id, "should discover main.c from main.h");
    }
}
