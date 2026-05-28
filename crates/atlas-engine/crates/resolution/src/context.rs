//! Resolution context: in-memory indexes for fast symbol/scopes/imports lookup.
//!
//! Loads file-level data from the store once and builds hash-based indexes
//! so resolution strategies can query without hitting SQLite every step.
//!
//! P4: `GlobalSymbolIndex` loads all project symbols into memory once,
//! replacing per-reference FTS5 queries with in-memory exact + fuzzy matching.
//!
//! P7: File-proximity scoring added to `find_by_name_proximity` so that
//! candidates from the same directory tree as the reference file are preferred
//! during project-wide name search, reducing cross-module noise in fuzzy fallback.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use db::Store;
use types::*;

/// Global in-memory index of all project symbols (P4: avoids per-reference DB queries).
///
/// Built once at the start of resolution. Provides O(1) exact lookups by
/// name/ID, plus bounded fuzzy fallback using Levenshtein distance.
///
/// P7: Tracks file parent directories so name lookups can score candidates
/// by directory-tree proximity, reducing false matches across unrelated modules.
#[derive(Debug)]
pub struct GlobalSymbolIndex {
    /// All symbols in the project.
    symbols: Vec<SymbolDef>,
    /// name (lowercase) → Vec<SymbolDef> (exact name match).
    by_name: HashMap<String, Vec<SymbolDef>>,
    /// SymbolId → SymbolDef.
    by_id: HashMap<SymbolId, SymbolDef>,
    /// FileId → parent directory path (without trailing '/'). Built once from
    /// the store's file table to enable directory-proximity scoring.
    file_parent_dir: HashMap<FileId, String>,

    // ── Per-session caches ──────────────────────────────────────────────
    /// Cached fuzzy-search results keyed by (lower_name, max_distance).
    /// Avoids O(N) Levenshtein scan when the same unresolved name hits
    /// Strategy 6 across multiple files.
    fuzzy_cache: Mutex<HashMap<(String, usize), Vec<SymbolDef>>>,
    /// Cached proximity results keyed by (lower_name, file_id).
    /// Avoids re-sorting candidates into tiers when the same (name, file)
    /// pair appears repeatedly.
    proximity_cache: Mutex<HashMap<(String, FileId), Vec<SymbolDef>>>,
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

        // Build file_id → parent directory map from the store's file table.
        let mut file_parent_dir: HashMap<FileId, String> = HashMap::new();
        if let Ok(files) = store.list_files() {
            for f in &files {
                if let Some(parent) = Path::new(&f.path).parent() {
                    file_parent_dir
                        .insert(f.file_id, parent.to_string_lossy().to_string());
                }
            }
        }

        Ok(Self {
            symbols,
            by_name,
            by_id,
            file_parent_dir,
            fuzzy_cache: Mutex::new(HashMap::new()),
            proximity_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Find symbols by exact name (case-insensitive).
    pub fn find_by_name(&self, name: &str) -> Vec<SymbolDef> {
        self.by_name
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Find symbols by exact name, sorted by directory proximity to the
    /// reference's file. Candidates from the same directory tree as `file_id`
    /// are placed first. This reduces cross-module false matches in
    /// Strategy 6 project-wide name search.
    ///
    /// Tier 0 = same parent directory; tier 1 = same top-level directory;
    /// tier 2 = unrelated.
    pub fn find_by_name_proximity(
        &self,
        name: &str,
        file_id: FileId,
    ) -> Vec<SymbolDef> {
        // ── Cache check ──
        let lower = name.to_lowercase();
        let cache_key = (lower, file_id);
        if let Ok(cache) = self.proximity_cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        let candidates = self.find_by_name(name);
        if candidates.len() <= 1 {
            // Cache the trivial result too.
            if let Ok(mut cache) = self.proximity_cache.lock() {
                cache.insert(cache_key, candidates.clone());
            }
            return candidates;
        }

        let ref_parent = self.file_parent_dir.get(&file_id);

        let mut tier0: Vec<SymbolDef> = Vec::new();
        let mut tier1: Vec<SymbolDef> = Vec::new();
        let mut tier2: Vec<SymbolDef> = Vec::new();

        for sym in candidates {
            let sym_parent = self.file_parent_dir.get(&sym.file_id);
            let tier = proximity_tier(ref_parent, sym_parent);
            match tier {
                0 => tier0.push(sym),
                1 => tier1.push(sym),
                _ => tier2.push(sym),
            }
        }

        let mut result = Vec::with_capacity(tier0.len() + tier1.len() + tier2.len());
        result.extend(tier0);
        result.extend(tier1);
        result.extend(tier2);

        if let Ok(mut cache) = self.proximity_cache.lock() {
            cache.insert(cache_key, result.clone());
        }
        result
    }

    /// Bounded fuzzy search (Levenshtein, max 20 results).
    ///
    /// Uses length pruning and trigram pre-filtering to avoid O(N×name)
    /// Levenshtein scan over all project symbols.  Results are cached
    /// per (lower_name, max_distance) so repeated queries (e.g., the same
    /// unresolved name in Strategy 6 across many files) return instantly.
    pub fn fuzzy_search(&self, name: &str, max_distance: usize) -> Vec<SymbolDef> {
        let lower = name.to_lowercase();
        let key = (lower.clone(), max_distance);

        // ── Cache check ──
        if let Ok(cache) = self.fuzzy_cache.lock() {
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let name_len = lower.len();

        // ── Length pruning ──
        // Levenshtein distance ≥ |len(a) - len(b)|, so skip candidates
        // whose length differs by more than max_distance.
        let min_len = name_len.saturating_sub(max_distance);
        let max_len = name_len.saturating_add(max_distance);

        // ── Trigram pre-filter ──
        // Only compute Levenshtein for candidates that share at least one
        // trigram with the query.  This reduces the candidate set from
        // O(N) to O(~50) on a typical 5k symbol project.
        let trigrams: HashSet<&str> = if name_len >= 3 {
            lower
                .as_bytes()
                .windows(3)
                .filter_map(|w| std::str::from_utf8(w).ok())
                .collect()
        } else {
            HashSet::new()
        };

        let mut candidates: Vec<(usize, SymbolDef)> = self
            .symbols
            .iter()
            .filter(|s| {
                let s_name = s.name.to_lowercase();
                let s_len = s_name.len();
                // Fast length check
                if s_len < min_len || s_len > max_len {
                    return false;
                }
                // Trigram check (skip for short names)
                if !trigrams.is_empty() && s_len >= 3 {
                    let has_common = s_name
                        .as_bytes()
                        .windows(3)
                        .any(|w| trigrams.contains(std::str::from_utf8(w).unwrap_or("")));
                    if !has_common {
                        return false;
                    }
                }
                true
            })
            .filter_map(|s| {
                let d = types::levenshtein(&lower, &s.name.to_lowercase());
                if d <= max_distance {
                    Some((d, s.clone()))
                } else {
                    None
                }
            })
            .collect();

        candidates.sort_by_key(|(d, _)| *d);
        candidates.truncate(20);
        let result: Vec<SymbolDef> = candidates.into_iter().map(|(_, s)| s).collect();

        // Cache for subsequent queries of the same (name, distance).
        if let Ok(mut cache) = self.fuzzy_cache.lock() {
            cache.insert(key, result.clone());
        }
        result
    }

    /// Find a symbol by ID.
    pub fn get(&self, id: &SymbolId) -> Option<&SymbolDef> {
        self.by_id.get(id)
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

/// Compute a proximity score for candidate sorting during name search.
///
/// 0 = same parent directory (strong signal: same module/package).
/// 1 = same top-level directory component (weak signal: same subsystem).
/// 2 = unrelated directory trees (minimum signal).
fn proximity_tier(ref_parent: Option<&String>, sym_parent: Option<&String>) -> usize {
    match (ref_parent, sym_parent) {
        (Some(r), Some(s)) => {
            if r == s {
                return 0;
            }
            let r_top = Path::new(r)
                .components()
                .next()
                .map(|c| c.as_os_str());
            let s_top = Path::new(s)
                .components()
                .next()
                .map(|c| c.as_os_str());
            if r_top.is_some() && r_top == s_top {
                return 1;
            }
            2
        }
        _ => 2,
    }
}

/// In-memory resolution context for a single file.
///
/// All symbol indexes store `Arc<SymbolDef>` so the same symbol can be
/// indexed by scope, ID, and qualified name with zero data copies —
/// only atomic reference-count increments.
#[derive(Debug)]
pub struct ResolutionContext {
    /// File being resolved.
    pub file: FileInfo,

    /// All symbols in this file (owned for Clone + into_iter consumers).
    pub symbols: Vec<Arc<SymbolDef>>,

    /// All scopes in this file.
    pub scopes: Vec<ScopeDef>,

    /// All imports in this file.
    pub imports: Vec<ImportDef>,

    // --- Indexes ---
    /// ScopeId → symbols in scope (including children).
    pub symbols_by_scope: HashMap<ScopeId, Vec<Arc<SymbolDef>>>,

    /// SymbolId → SymbolDef (direct lookup).
    pub symbols_by_id: HashMap<SymbolId, Arc<SymbolDef>>,

    /// qualified_name → SymbolDef (exact qname match).
    pub symbols_by_qname: HashMap<String, Arc<SymbolDef>>,

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

        let symbols: Vec<Arc<SymbolDef>> = store
            .find_symbols_by_file(&file_id)?
            .into_iter()
            .map(Arc::new)
            .collect();
        let scopes = store.find_scopes_by_file(&file_id)?;
        let imports = store.find_imports_by_file(&file_id)?;

        // Build indexes — Symbols are stored as Arc so that the same symbol
        // can appear in multiple indexes with zero data copies (only refcount).
        let mut symbols_by_scope: HashMap<ScopeId, Vec<Arc<SymbolDef>>> = HashMap::new();
        let mut symbols_by_id: HashMap<SymbolId, Arc<SymbolDef>> = HashMap::new();
        let mut symbols_by_qname: HashMap<String, Arc<SymbolDef>> = HashMap::new();
        let mut scopes_by_id: HashMap<ScopeId, ScopeDef> = HashMap::new();
        let mut scope_parents: HashMap<ScopeId, ScopeId> = HashMap::new();

        for s in &scopes {
            scopes_by_id.insert(s.id, s.clone());
            if let Some(pid) = s.parent_id {
                scope_parents.insert(s.id, pid);
            }
        }

        for sym in &symbols {
            let arc = Arc::clone(sym);
            // Group by scope
            if let Some(sid) = arc.scope_id {
                symbols_by_scope.entry(sid).or_default().push(Arc::clone(&arc));
            }
            // Index by ID
            symbols_by_id.insert(arc.id, Arc::clone(&arc));
            // Index by qualified name
            if !arc.qualified_name.is_empty() {
                symbols_by_qname.insert(arc.qualified_name.clone(), Arc::clone(&arc));
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
    pub fn symbols_in_scope(&self, scope_id: ScopeId) -> &[Arc<SymbolDef>] {
        match self.symbols_by_scope.get(&scope_id) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Walk the scope chain upward, searching for a symbol by name.
    pub fn lookup_scoped(&self, scope_id: ScopeId, name: &str) -> Option<&SymbolDef> {
        let mut current = Some(scope_id);
        while let Some(sid) = current {
            if let Some(syms) = self.symbols_by_scope.get(&sid) {
                for s in syms {
                    if s.name == name {
                        return Some(&**s);
                    }
                }
            }
            current = self.scope_parents.get(&sid).copied();
        }
        None
    }

    /// Find symbols in the whole file by name (linear scan, use for same-file strategy).
    pub fn find_in_file_by_name(&self, name: &str) -> Vec<SymbolDef> {
        self.symbols
            .iter()
            .filter(|s| s.name == name)
            .map(|s| (**s).clone())
            .collect()
    }

    /// Exact qualified-name lookup in indexes.
    pub fn find_by_qname(&self, qname: &str) -> Option<&SymbolDef> {
        self.symbols_by_qname.get(qname).map(|v| &**v)
    }
}
