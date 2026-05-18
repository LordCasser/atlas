//! Import path resolver.

use crate::types::Language;

pub struct ImportResolver;

impl ImportResolver {
    pub fn new() -> Self {
        Self
    }

    pub fn resolve_import_path(
        &self,
        _import_path: &str,
        _from_file: &str,
        _lang: Language,
    ) -> Option<String> {
        todo!("Phase 7")
    }
}
