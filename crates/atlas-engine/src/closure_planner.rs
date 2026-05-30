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
use types::ids::FileId;
use types::{layer, status};

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
    max_depth: usize,
    max_closure_files: usize,
}

impl ClosurePlanner {
    pub fn new(
        store: Arc<Store>,
        project_root: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            store,
            project_root,
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
                        tracing::debug!(
                            "ClosurePlanner: skipping file {:?}: {:#}",
                            file_id,
                            e
                        );
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

        for dep in closure.direct_deps.iter().chain(closure.transitive_deps.iter()) {
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
        let closure = self.plan_closure(seed)?;
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
    /// Bare imports (`is_relative = false`, e.g. "react") return `None`
    /// because they refer to external dependencies not in the index.
    fn resolve_import_target(
        &self,
        importing_file_dir: &str,
        module: &str,
        is_relative: bool,
    ) -> Result<Option<FileId>> {
        if !is_relative {
            return Ok(None);
        }

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
    }

    // Private helper needed for fn resolve_import_target - dead_code warning
    #[allow(dead_code)]
    fn _project_root(&self) -> Option<&std::path::Path> {
        self.project_root.as_deref()
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
}
