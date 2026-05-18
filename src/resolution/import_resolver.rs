//! Import path resolver — maps import statements to candidate symbols.

use crate::db::Store;
use crate::types::*;

/// Resolves import paths to potential symbols.
pub struct ImportResolver {
    store: std::sync::Arc<Store>,
}

impl ImportResolver {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self { store }
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
            let fallback_name = import
                .local_name
                .clone()
                .or_else(|| {
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
    fn candidate_qnames(&self, import: &ImportDef) -> Vec<String> {
        let mut candidates = Vec::new();

        let module = &import.module;
        let name = &import.imported_name;
        let local: &str = import.local_name.as_deref().unwrap_or("");

        match import.kind {
            ImportKind::Import | ImportKind::Package | ImportKind::Use => {
                let n: &str = if !local.is_empty() { local } else { name.as_str() };
                if !n.is_empty() {
                    candidates.push(n.to_string());
                }
            }
            ImportKind::FromImport => {
                if !module.is_empty() && !name.is_empty() {
                    candidates.push(format!("{}.{}", module, name));
                }
                if !name.is_empty() {
                    candidates.push(name.clone());
                }
            }
            ImportKind::Include => {
                if !module.is_empty() {
                    let stem = module
                        .rsplit('/')
                        .next()
                        .and_then(|s| s.rsplit('.').next())
                        .unwrap_or(module.as_str());
                    candidates.push(stem.to_string());
                }
            }
        }

        candidates
    }
}
