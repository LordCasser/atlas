//! Resolution context: in-memory indexes for fast symbol/scopes/imports lookup.
//!
//! Loads file-level data from the store once and builds hash-based indexes
//! so resolution strategies can query without hitting SQLite every step.
//!
//! P4: `GlobalSymbolIndex` loads all project symbols into memory once,
//! replacing per-reference FTS5 queries with in-memory exact + fuzzy matching.

use std::collections::HashMap;

use atlas_db::Store;
use atlas_types::*;

/// Global in-memory index of all project symbols (P4: avoids per-reference DB queries).
///
/// Built once at the start of resolution. Provides O(1) exact lookups by
/// name/ID, plus bounded fuzzy fallback using Levenshtein distance.
#[derive(Debug, Clone)]
pub struct GlobalSymbolIndex {
    /// All symbols in the project.
    symbols: Vec<SymbolDef>,
    /// name (lowercase) → Vec<SymbolDef> (exact name match).
    by_name: HashMap<String, Vec<SymbolDef>>,
    /// SymbolId → SymbolDef.
    by_id: HashMap<SymbolId, SymbolDef>,
}

impl GlobalSymbolIndex {
    /// Build the global index from all symbols in the store.
    pub fn build(store: &Store) -> anyhow::Result<Self> {
        let symbols = store.load_all_symbols()?;
        let mut by_name: HashMap<String, Vec<SymbolDef>> = HashMap::new();
        let mut by_id: HashMap<SymbolId, SymbolDef> = HashMap::new();

        for sym in &symbols {
            by_id.insert(sym.id, sym.clone());
            let key = sym.name.to_lowercase();
            by_name.entry(key).or_default().push(sym.clone());
        }

        Ok(Self {
            symbols,
            by_name,
            by_id,
        })
    }

    /// Find symbols by exact name (case-insensitive).
    pub fn find_by_name(&self, name: &str) -> Vec<SymbolDef> {
        self.by_name
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Find a symbol by ID.
    pub fn get(&self, id: &SymbolId) -> Option<&SymbolDef> {
        self.by_id.get(id)
    }

    /// Bounded fuzzy search (Levenshtein, max 20 results).
    /// Returns candidates for downstream scoring.
    pub fn fuzzy_search(&self, name: &str, max_distance: usize) -> Vec<SymbolDef> {
        let lower = name.to_lowercase();
        let mut candidates: Vec<(usize, SymbolDef)> = self
            .symbols
            .iter()
            .filter_map(|s| {
                let d = atlas_types::levenshtein(&lower, &s.name.to_lowercase());
                if d <= max_distance { Some((d, s.clone())) } else { None }
            })
            .collect();
        candidates.sort_by_key(|(d, _)| *d);
        candidates.truncate(20);
        candidates.into_iter().map(|(_, s)| s).collect()
    }

    /// Total number of symbols indexed.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

/// In-memory resolution context for a single file.
#[derive(Debug, Clone)]
pub struct ResolutionContext {
    /// File being resolved.
    pub file: FileInfo,

    /// All symbols in this file.
    pub symbols: Vec<SymbolDef>,

    /// All scopes in this file.
    pub scopes: Vec<ScopeDef>,

    /// All imports in this file.
    pub imports: Vec<ImportDef>,

    // --- Indexes ---
    /// ScopeId → Vec<SymbolDef> (symbols in scope, including children).
    pub symbols_by_scope: HashMap<ScopeId, Vec<SymbolDef>>,

    /// SymbolId → SymbolDef (direct lookup).
    pub symbols_by_id: HashMap<SymbolId, SymbolDef>,

    /// qualified_name → SymbolDef (exact qname match).
    pub symbols_by_qname: HashMap<String, SymbolDef>,

    /// ScopeId → ScopeDef.
    pub scopes_by_id: HashMap<ScopeId, ScopeDef>,

    /// ScopeId → parent ScopeId (for scope-tree walking).
    pub scope_parents: HashMap<ScopeId, ScopeId>,
}

impl ResolutionContext {
    /// Build resolution context for a file.
    pub fn build(store: &Store, file_id: FileId) -> anyhow::Result<Self> {
        let file = store
            .get_file(&file_id)?
            .ok_or_else(|| anyhow::anyhow!("File not found in store"))?;

        let symbols = store.find_symbols_by_file(&file_id)?;
        let scopes = store.find_scopes_by_file(&file_id)?;
        let imports = store.find_imports_by_file(&file_id)?;

        // Build indexes
        let mut symbols_by_scope: HashMap<ScopeId, Vec<SymbolDef>> = HashMap::new();
        let mut symbols_by_id: HashMap<SymbolId, SymbolDef> = HashMap::new();
        let mut symbols_by_qname: HashMap<String, SymbolDef> = HashMap::new();
        let mut scopes_by_id: HashMap<ScopeId, ScopeDef> = HashMap::new();
        let mut scope_parents: HashMap<ScopeId, ScopeId> = HashMap::new();

        for s in &scopes {
            scopes_by_id.insert(s.id, s.clone());
            if let Some(pid) = s.parent_id {
                scope_parents.insert(s.id, pid);
            }
        }

        for sym in &symbols {
            // Group by scope
            if let Some(sid) = sym.scope_id {
                symbols_by_scope.entry(sid).or_default().push(sym.clone());
            }
            // Index by ID
            symbols_by_id.insert(sym.id, sym.clone());
            // Index by qualified name
            if !sym.qualified_name.is_empty() {
                symbols_by_qname.insert(sym.qualified_name.clone(), sym.clone());
            }
        }

        Ok(Self {
            file,
            symbols,
            scopes,
            imports,
            symbols_by_scope,
            symbols_by_id,
            symbols_by_qname,
            scopes_by_id,
            scope_parents,
        })
    }

    // --- Convenience lookups ---

    /// Find symbols directly in the given scope (NOT including children).
    pub fn symbols_in_scope(&self, scope_id: ScopeId) -> &[SymbolDef] {
        match self.symbols_by_scope.get(&scope_id) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Walk the scope chain upward, searching for a symbol by name.
    pub fn lookup_scoped(&self, scope_id: ScopeId, name: &str) -> Option<&SymbolDef> {
        let mut current = Some(scope_id);
        while let Some(sid) = current {
            // Search in this scope
            if let Some(syms) = self.symbols_by_scope.get(&sid) {
                for s in syms {
                    if s.name == name {
                        return Some(s);
                    }
                }
            }
            // Move to parent
            current = self.scope_parents.get(&sid).copied();
        }
        None
    }

    /// Find symbols in the whole file by name (linear scan, use for same-file strategy).
    pub fn find_in_file_by_name(&self, name: &str) -> Vec<SymbolDef> {
        self.symbols
            .iter()
            .filter(|s| s.name == name)
            .cloned()
            .collect()
    }

    /// Exact qualified-name lookup in indexes.
    pub fn find_by_qname(&self, qname: &str) -> Option<&SymbolDef> {
        self.symbols_by_qname.get(qname)
    }
}
