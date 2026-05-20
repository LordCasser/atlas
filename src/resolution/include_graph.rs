//! C/C++ include graph resolver.
//!
//! Handles the resolution of `#include` directives:
//! - System includes: `#include <stdio.h>` — resolved against system include paths
//! - Local includes: `#include "helper.h"` — resolved relative to the including file
//! - Include chains: `a.h` → `b.h` → `c.h` — transitive resolution
//!
//! For MVP, we focus on local includes (project files) and skip system includes.
//! System includes are handled by the BuiltinFilter.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::db::Store;
use crate::types::*;

/// Resolves C/C++ `#include` directives to project files.
pub struct IncludeGraph {
    store: Arc<Store>,
    /// Project root directory for resolving relative includes.
    project_root: PathBuf,
}

impl IncludeGraph {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self { store, project_root }
    }

    /// Resolve an include directive to a file in the project.
    ///
    /// For `#include "path.h"` (local include), we search:
    /// 1. Relative to the including file's directory
    /// 2. Relative to the project root
    ///
    /// For `#include <path.h>` (system include), we return `None`
    /// (system headers are handled by BuiltinFilter).
    pub fn resolve_include(&self, import: &ImportDef) -> Option<FileId> {
        // Only handle local includes (#include "...")
        if import.kind != ImportKind::Include || !import.is_relative {
            return None;
        }

        let module_path = &import.module;
        if module_path.is_empty() {
            return None;
        }

        // Strategy 1: Try as project-relative path
        let candidate = self.project_root.join(module_path);
        let relative = candidate
            .strip_prefix(&self.project_root)
            .unwrap_or(&candidate);
        let file_id = FileId::generate(&relative.to_string_lossy());

        // Check if the file exists in the DB
        if self.store.get_file(&file_id).ok().flatten().is_some() {
            return Some(file_id);
        }

        // Strategy 2: Try common C/C++ include patterns
        // header.h → header.c or header.cpp
        if let Some(stem) = Path::new(module_path).file_stem() {
            let stem_str = stem.to_string_lossy();

            // Try .c, .cpp, .cc companions
            for ext in &["c", "cpp", "cc", "cxx"] {
                let companion = format!("{}.{}", stem_str, ext);
                let companion_id = FileId::generate(&companion);
                if self.store.get_file(&companion_id).ok().flatten().is_some() {
                    return Some(companion_id);
                }
            }
        }

        None
    }

    /// Find all files that include a given file (reverse include graph).
    ///
    /// Returns the FileIds of files that have `#include` directives
    /// pointing to the given file.
    pub fn find_includers(&self, file_id: &FileId) -> Vec<FileId> {
        // Get the file path to match against import module paths
        let _file_info = match self.store.get_file(file_id).ok().flatten() {
            Some(f) => f,
            None => return vec![],
        };

        let mut includers = Vec::new();

        // Search through all files' imports for includes matching this file
        // For MVP, we iterate all files. A future optimization could use
        // a dedicated index.
        if let Ok(all_files) = self.store.list_files() {
            for fi in all_files {
                if let Ok(imports) = self.store.find_imports_by_file(&fi.file_id) {
                    for import in &imports {
                        if import.kind == ImportKind::Include {
                            let resolved = self.resolve_include(import);
                            if resolved.as_ref() == Some(file_id) {
                                includers.push(fi.file_id);
                                break; // Only add each file once
                            }
                        }
                    }
                }
            }
        }

        includers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_include_graph_creation() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let _graph = IncludeGraph::new(store, PathBuf::from("/project"));
    }

    #[test]
    fn test_system_include_returns_none() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let graph = IncludeGraph::new(store, PathBuf::from("/project"));

        let import = ImportDef {
            id: crate::types::ids::ImportId::generate(
                &FileId::generate("main.c"),
                ImportKind::Include.as_str(),
                "<stdio.h>",
                Some(""),
                0,
            ),
            file_id: FileId::generate("main.c"),
            kind: ImportKind::Include,
            module: "<stdio.h>".to_string(),
            imported_name: String::new(),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false, // system include
            range: Default::default(),
        };

        assert!(graph.resolve_include(&import).is_none());
    }
}
