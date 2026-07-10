//! ClosurePlanner — dependency-closure-aware lazy extraction planning.
//!
//! Given a seed file (e.g., `bar.ts` that imports `./foo`), the
//! ClosurePlanner returns a bounded import dependency set. Focus materializes
//! resolution symbols for that set before scoped cross-file resolution.
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
//! Defaults: max_depth=2, max_closure_files=30. Override with
//! [`ClosurePlanner::with_limits`].

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::ids::FileId;

/// A directory to search when resolving angle-bracket includes (`#include <...>`).
///
/// Project-relative path, e.g. `"include"`, `"arch/x86/include"`.
#[derive(Debug, Clone)]
pub struct IncludeRoot {
    /// Project-relative directory path (uses `/` separator on all platforms).
    pub path: String,
}

/// Plans dependency closures for lazy extraction.
pub(crate) struct ClosurePlanner {
    store: Arc<Store>,
    include_roots: Vec<IncludeRoot>,
    max_depth: usize,
    max_closure_files: usize,
}

impl ClosurePlanner {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            include_roots: Vec::new(),
            max_depth: 2,
            max_closure_files: 30,
        }
    }

    /// Override the default BFS depth and file-count limits.
    pub(crate) fn with_limits(mut self, max_depth: usize, max_closure_files: usize) -> Self {
        self.max_depth = max_depth;
        self.max_closure_files = max_closure_files;
        self
    }

    /// Set include roots for angle-bracket include resolution.
    ///
    /// Each root is a project-relative directory to search when resolving
    /// `#include <...>` directives (C/C++).
    pub(crate) fn with_include_roots(mut self, roots: Vec<IncludeRoot>) -> Self {
        self.include_roots = roots;
        self
    }

    /// Compute the bounded import dependencies for a seed file.
    ///
    /// BFS from seed through imports, resolving module strings to file_ids.
    /// Respects [`self.max_depth`] and [`self.max_closure_files`].
    pub(crate) fn plan_dependencies(&self, seed: &FileId) -> Result<Vec<FileId>> {
        let mut visited: HashSet<FileId> = HashSet::new();
        let mut queue: VecDeque<FileId> = VecDeque::new();

        visited.insert(*seed);
        queue.push_back(*seed);

        let mut dependencies = Vec::new();
        for _ in 0..self.max_depth {
            let level_size = queue.len();
            if level_size == 0 {
                break;
            }
            // Collect files at this BFS level
            let mut current_level: Vec<FileId> = Vec::with_capacity(level_size);
            for _ in 0..level_size {
                current_level.push(queue.pop_front().unwrap());
            }

            for file_id in &current_level {
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
                        dependencies.push(target_id);
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

        Ok(dependencies)
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Get the parent directory of a file's project-relative path.
    ///
    /// Returns empty string for root-level files.
    fn get_file_dir(&self, file_id: &FileId) -> Result<String> {
        let file_info = self
            .store
            .get_file(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found for id {file_id:?}"))?;

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

        let planner = ClosurePlanner::new(store).with_include_roots(vec![
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
}
