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
    /// Pre-computed lower-case names for each symbol (same index as `symbols`).
    /// Avoids millions of .to_lowercase() calls during fuzzy_search.
    lower_names: Vec<String>,
    /// name (lowercase) → Vec<SymbolDef> (exact name match).
    by_name: HashMap<String, Vec<SymbolDef>>,
    /// SymbolId → SymbolDef.
    by_id: HashMap<SymbolId, SymbolDef>,
    /// FileId → parent directory path (without trailing '/'). Built once from
    /// the store's file table to enable directory-proximity scoring.
    file_parent_dir: HashMap<FileId, String>,
    /// Files under explicit test/spec directories. Project-wide heuristic
    /// fallback must not connect production references to these symbols.
    test_file_ids: HashSet<FileId>,
    /// Parent directory → indices into `symbols` (ascending) for symbols
    /// declared in files under that directory. Lets proximity-scoped fuzzy
    /// search visit only nearby symbols instead of scanning the whole
    /// project — see docs/performance.md Methodology §16.
    dir_symbol_ix: HashMap<String, Vec<u32>>,

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
        let symbols = store.get_all_symbols()?;
        Self::build_from_symbols(&symbols, store)
    }

    /// Build the global index from a pre-loaded slice of symbols.
    ///
    /// Avoids a duplicate `get_all_symbols()` call when the caller already has
    /// the symbol list (e.g. shared between resolution and graph building).
    /// The store is still needed for the file → parent directory map.
    pub fn build_from_symbols(symbols: &[SymbolDef], store: &Store) -> anyhow::Result<Self> {
        let mut by_name: HashMap<String, Vec<SymbolDef>> = HashMap::new();
        let mut by_id: HashMap<SymbolId, SymbolDef> = HashMap::new();
        let mut lower_names: Vec<String> = Vec::with_capacity(symbols.len());

        for sym in symbols {
            by_id.insert(sym.id, sym.clone());
            let key = sym.name.to_lowercase();
            lower_names.push(key.clone());
            by_name.entry(key).or_default().push(sym.clone());
        }

        // Build file_id → parent directory map from the store's file table.
        let mut file_parent_dir: HashMap<FileId, String> = HashMap::new();
        let mut test_file_ids = HashSet::new();
        if let Ok(files) = store.list_files() {
            for f in &files {
                if let Some(parent) = Path::new(&f.path).parent() {
                    file_parent_dir.insert(f.file_id, parent.to_string_lossy().to_string());
                }
                if is_explicit_test_path(&f.path) {
                    test_file_ids.insert(f.file_id);
                }
            }
        }

        // Group symbol indices by their file's parent directory. Indices are
        // pushed in ascending order, which proximity search relies on to keep
        // its tie-breaking identical to a full linear scan.
        let mut dir_symbol_ix: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, sym) in symbols.iter().enumerate() {
            if let Some(dir) = file_parent_dir.get(&sym.file_id)
                && let Ok(ix) = u32::try_from(i)
            {
                dir_symbol_ix.entry(dir.clone()).or_default().push(ix);
            }
        }

        Ok(Self {
            symbols: symbols.to_vec(),
            lower_names,
            by_name,
            by_id,
            file_parent_dir,
            test_file_ids,
            dir_symbol_ix,
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

    /// Resolve an exact project-wide name match without cloning/sorting the
    /// candidate list. Exact-case matches win over case-insensitive matches;
    /// within the same confidence class, directory proximity breaks ties.
    pub fn find_exact_name_target(
        &self,
        name: &str,
        file_id: Option<FileId>,
    ) -> Option<ResolvedTarget> {
        let candidates = self.by_name.get(&name.to_lowercase())?;
        let ref_parent = file_id.and_then(|fid| self.file_parent_dir.get(&fid));
        let mut best_exact: Option<(usize, usize, &SymbolDef)> = None;
        let mut best_case_insensitive: Option<(usize, usize, &SymbolDef)> = None;

        for (order, sym) in candidates.iter().enumerate() {
            if !self.is_allowed_global_candidate(file_id, sym.file_id) {
                continue;
            }
            let tier = match file_id {
                Some(_) => proximity_tier(ref_parent, self.file_parent_dir.get(&sym.file_id)),
                None => 0,
            };

            if sym.name == name {
                if best_exact.is_none_or(|(best_tier, best_order, _)| {
                    tier < best_tier || (tier == best_tier && order < best_order)
                }) {
                    best_exact = Some((tier, order, sym));
                }
            } else if sym.name.eq_ignore_ascii_case(name)
                && best_case_insensitive.is_none_or(|(best_tier, best_order, _)| {
                    tier < best_tier || (tier == best_tier && order < best_order)
                })
            {
                best_case_insensitive = Some((tier, order, sym));
            }
        }

        if let Some((_tier, _order, sym)) = best_exact {
            return Some(ResolvedTarget {
                symbol_id: sym.id,
                confidence: Confidence::certain(),
                strategy: ResolutionStrategy::NameOnly,
                provenance: Provenance::Heuristic,
            });
        }

        best_case_insensitive.map(|(_tier, _order, sym)| ResolvedTarget {
            symbol_id: sym.id,
            confidence: Confidence::new(0.9),
            strategy: ResolutionStrategy::NameOnly,
            provenance: Provenance::Heuristic,
        })
    }

    /// Same as [`find_exact_name_target`], but prioritizes candidates within
    /// `preferred_file_ids`. When the preferred set is empty this delegates to
    /// the original method. Otherwise it scans preferred files first; if an
    /// exact match is found there the global scan is skipped entirely.
    pub fn find_exact_name_target_in_scope(
        &self,
        name: &str,
        file_id: Option<FileId>,
        preferred_file_ids: &HashSet<FileId>,
    ) -> Option<ResolvedTarget> {
        let candidates = self.by_name.get(&name.to_lowercase())?;

        // Import scope is a correctness signal, not a large-candidate
        // optimization. Even two same-named symbols can belong to unrelated
        // modules, so never bypass a non-empty preferred set.
        if preferred_file_ids.is_empty() {
            return self.find_exact_name_target(name, file_id);
        }

        let ref_parent = file_id.and_then(|fid| self.file_parent_dir.get(&fid));

        // Pass 1: only scan candidates that live in a preferred file.
        let mut best_exact_in_scope: Option<(usize, usize, &SymbolDef)> = None;
        for (order, sym) in candidates.iter().enumerate() {
            if !preferred_file_ids.contains(&sym.file_id) {
                continue;
            }
            if !self.is_allowed_global_candidate(file_id, sym.file_id) {
                continue;
            }
            let tier = match file_id {
                Some(_) => proximity_tier(ref_parent, self.file_parent_dir.get(&sym.file_id)),
                None => 0,
            };
            if sym.name == name
                && best_exact_in_scope.is_none_or(|(best_tier, best_order, _)| {
                    tier < best_tier || (tier == best_tier && order < best_order)
                })
            {
                best_exact_in_scope = Some((tier, order, sym));
            }
        }

        // Exact match found within preferred files → return immediately,
        // bypassing the full O(candidates) global scan.
        if let Some((_tier, _order, sym)) = best_exact_in_scope {
            return Some(ResolvedTarget {
                symbol_id: sym.id,
                confidence: Confidence::certain(),
                strategy: ResolutionStrategy::NameOnly,
                provenance: Provenance::Heuristic,
            });
        }

        // Pass 2: no match in preferred files — fall back to the global scan.
        self.find_exact_name_target(name, file_id)
    }

    fn is_allowed_global_candidate(
        &self,
        source_file_id: Option<FileId>,
        candidate_file_id: FileId,
    ) -> bool {
        source_file_id.is_none_or(|source| {
            self.test_file_ids.contains(&source) || !self.test_file_ids.contains(&candidate_file_id)
        })
    }

    /// Find symbols by exact name, sorted by directory proximity to the
    /// reference's file. Candidates from the same directory tree as `file_id`
    /// are placed first. This reduces cross-module false matches in
    /// Strategy 6 project-wide name search.
    ///
    /// Tier 0 = same parent directory; tier 1 = same top-level directory;
    /// tier 2 = unrelated.
    pub fn find_by_name_proximity(&self, name: &str, file_id: FileId) -> Vec<SymbolDef> {
        // ── Cache check ──
        let lower = name.to_lowercase();
        let cache_key = (lower, file_id);
        if let Ok(cache) = self.proximity_cache.lock()
            && let Some(cached) = cache.get(&cache_key)
        {
            return cached.clone();
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
        if let Ok(cache) = self.fuzzy_cache.lock()
            && let Some(cached) = cache.get(&key)
        {
            return cached.clone();
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
            .zip(self.lower_names.iter())
            .filter(|(_s, s_lower)| {
                let s_len = s_lower.len();
                if s_len < min_len || s_len > max_len {
                    return false;
                }
                if !trigrams.is_empty() && s_len >= 3 {
                    let has_common = s_lower
                        .as_bytes()
                        .windows(3)
                        .any(|w| trigrams.contains(std::str::from_utf8(w).unwrap_or("")));
                    if !has_common {
                        return false;
                    }
                }
                true
            })
            .filter_map(|(s, s_lower)| {
                types::levenshtein_bounded(&lower, s_lower, max_distance).map(|d| (d, s.clone()))
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

    /// Proximity-scoped fuzzy search: restrict to same-directory symbols first,
    /// fall back to global fuzzy_search only when no proximity match is found.
    pub fn fuzzy_search_proximity(
        &self,
        name: &str,
        max_distance: usize,
        file_id: FileId,
    ) -> Vec<SymbolDef> {
        // A reference in a file with no parent directory can never satisfy the
        // proximity predicate, so skip straight to the global search.
        let Some(ref_parent) = self.file_parent_dir.get(&file_id) else {
            return self.fuzzy_search(name, max_distance);
        };
        let lower = name.to_lowercase();
        let name_len = lower.len();
        let min_len = name_len.saturating_sub(max_distance);
        let max_len = name_len.saturating_add(max_distance);

        let trigrams: HashSet<&[u8]> = if name_len >= 3 {
            lower
                .as_bytes()
                .windows(3)
                .filter(|w| std::str::from_utf8(w).is_ok())
                .collect()
        } else {
            HashSet::new()
        };

        // Directory proximity rejects ~98% of the project, so resolve it first
        // via the pre-built directory index instead of scanning every symbol.
        // Indices are re-sorted so the candidate order matches a full linear
        // scan, keeping `sort_by_key`'s stable tie-breaking unchanged.
        let mut nearby: Vec<u32> = Vec::new();
        for (dir, ixs) in &self.dir_symbol_ix {
            if ref_parent == dir || ref_parent.starts_with(dir) || dir.starts_with(ref_parent) {
                nearby.extend_from_slice(ixs);
            }
        }
        nearby.sort_unstable();

        let mut candidates: Vec<(usize, SymbolDef)> = nearby
            .into_iter()
            .map(|i| i as usize)
            .filter(|i| {
                let s_len = self.lower_names[*i].len();
                s_len >= min_len && s_len <= max_len
            })
            .filter(|i| {
                if trigrams.is_empty() {
                    return true;
                }
                self.lower_names[*i]
                    .as_bytes()
                    .windows(3)
                    .any(|w| trigrams.contains(w))
            })
            .filter_map(|i| {
                types::levenshtein_bounded(&lower, &self.lower_names[i], max_distance)
                    .map(|d| (d, self.symbols[i].clone()))
            })
            .collect();

        if candidates.is_empty() {
            return self.fuzzy_search(name, max_distance);
        }

        candidates.sort_by_key(|(d, _)| *d);
        candidates.truncate(20);
        candidates.into_iter().map(|(_, s)| s).collect()
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

pub(crate) fn is_explicit_test_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "test" | "tests" | "testing" | "selftests" | "spec" | "__tests__"
            )
        })
    })
}

/// Compute a proximity score for candidate sorting during name search.
///
/// 0 = same parent directory (strong signal: same module/package).
/// 1 = same top-level directory component (weak signal: same subsystem).
/// 2 = unrelated directory trees (minimum signal).
pub(crate) fn proximity_tier(ref_parent: Option<&String>, sym_parent: Option<&String>) -> usize {
    match (ref_parent, sym_parent) {
        (Some(r), Some(s)) => {
            if r == s {
                return 0;
            }
            let r_top = Path::new(r).components().next().map(|c| c.as_os_str());
            let s_top = Path::new(s).components().next().map(|c| c.as_os_str());
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
    /// Import name (imported_name or local_name alias) → indices into `imports`.
    /// Enables O(1) import filtering in Strategy 5 instead of iterating all
    /// imports per reference.
    pub imports_by_name: HashMap<String, Vec<usize>>,
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
                symbols_by_scope
                    .entry(sid)
                    .or_default()
                    .push(Arc::clone(&arc));
            }
            // Index by ID
            symbols_by_id.insert(arc.id, Arc::clone(&arc));
            // Index by qualified name
            if !arc.qualified_name.is_empty() {
                symbols_by_qname.insert(arc.qualified_name.clone(), Arc::clone(&arc));
            }
        }

        // Pre-index imports by name for O(1) Strategy 5 filtering.
        let mut imports_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, import) in imports.iter().enumerate() {
            if !import.imported_name.is_empty() {
                imports_by_name
                    .entry(import.imported_name.clone())
                    .or_default()
                    .push(i);
            }
            if let Some(ref alias) = import.local_name {
                imports_by_name.entry(alias.clone()).or_default().push(i);
            }
        }

        Ok(Self {
            file,
            symbols,
            scopes,
            imports,
            imports_by_name,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_symbol(name: &str, file_path: &str) -> SymbolDef {
        let file_id = FileId::generate(file_path);
        let range = TextRange {
            start_byte: 0,
            end_byte: name.len() as u32,
            start_line: 1,
            start_column: 0,
            end_line: 1,
            end_column: name.len() as u32,
        };
        SymbolDef {
            id: SymbolId::generate(
                &file_id,
                Language::TypeScript.as_str(),
                name,
                SymbolKind::Function.as_str(),
                None,
            ),
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            symbol_path: vec![name.to_string()],
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
            namespace_path: Vec::new(),
            layer: types::layer::STRUCTURAL.to_string(),
        }
    }

    fn test_index(
        symbols: Vec<SymbolDef>,
        file_parent_dir: impl IntoIterator<Item = (FileId, &'static str)>,
    ) -> GlobalSymbolIndex {
        let mut by_name: HashMap<String, Vec<SymbolDef>> = HashMap::new();
        let mut by_id: HashMap<SymbolId, SymbolDef> = HashMap::new();
        let mut lower_names = Vec::with_capacity(symbols.len());
        let file_parent_dir: HashMap<FileId, String> = file_parent_dir
            .into_iter()
            .map(|(file_id, parent)| (file_id, parent.to_string()))
            .collect();
        let test_file_ids = file_parent_dir
            .iter()
            .filter_map(|(file_id, parent)| is_explicit_test_path(parent).then_some(*file_id))
            .collect();

        let mut dir_symbol_ix: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, sym) in symbols.iter().enumerate() {
            by_id.insert(sym.id, sym.clone());
            let key = sym.name.to_lowercase();
            lower_names.push(key.clone());
            by_name.entry(key).or_default().push(sym.clone());
            if let Some(dir) = file_parent_dir.get(&sym.file_id)
                && let Ok(ix) = u32::try_from(i)
            {
                dir_symbol_ix.entry(dir.clone()).or_default().push(ix);
            }
        }

        GlobalSymbolIndex {
            symbols,
            lower_names,
            by_name,
            by_id,
            file_parent_dir,
            test_file_ids,
            dir_symbol_ix,
            fuzzy_cache: Mutex::new(HashMap::new()),
            proximity_cache: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn exact_name_target_prefers_exact_case_before_proximity() {
        let far_exact = test_symbol("run", "packages/a/src/run.ts");
        let near_case_insensitive = test_symbol("Run", "packages/b/src/run.ts");
        let ref_file = FileId::generate("packages/b/src/caller.ts");
        let index = test_index(
            vec![far_exact.clone(), near_case_insensitive.clone()],
            [
                (far_exact.file_id, "packages/a/src"),
                (near_case_insensitive.file_id, "packages/b/src"),
                (ref_file, "packages/b/src"),
            ],
        );

        let matched = index.find_exact_name_target("run", Some(ref_file)).unwrap();

        assert_eq!(matched.symbol_id, far_exact.id);
        assert_eq!(matched.confidence, Confidence::certain());
    }

    #[test]
    fn exact_name_target_uses_proximity_within_same_confidence() {
        let far = test_symbol("run", "packages/a/src/run.ts");
        let near = test_symbol("run", "packages/b/src/run.ts");
        let ref_file = FileId::generate("packages/b/src/caller.ts");
        let index = test_index(
            vec![far.clone(), near.clone()],
            [
                (far.file_id, "packages/a/src"),
                (near.file_id, "packages/b/src"),
                (ref_file, "packages/b/src"),
            ],
        );

        let matched = index.find_exact_name_target("run", Some(ref_file)).unwrap();

        assert_eq!(matched.symbol_id, near.id);
        assert_eq!(matched.confidence, Confidence::certain());
    }

    #[test]
    fn exact_name_target_honors_small_preferred_scope() {
        let imported = test_symbol("run", "include/api/run.ts");
        let nearby = test_symbol("run", "src/local/run.ts");
        let ref_file = FileId::generate("src/local/caller.ts");
        let index = test_index(
            vec![nearby, imported.clone()],
            [
                (imported.file_id, "include/api"),
                (FileId::generate("src/local/run.ts"), "src/local"),
                (ref_file, "src/local"),
            ],
        );

        let matched = index
            .find_exact_name_target_in_scope(
                "run",
                Some(ref_file),
                &HashSet::from([imported.file_id]),
            )
            .unwrap();

        assert_eq!(matched.symbol_id, imported.id);
    }

    #[test]
    fn exact_name_target_does_not_connect_production_to_test_symbol() {
        let test_symbol = test_symbol("preempt_disable", "tools/testing/preempt_lock.c");
        let production_file = FileId::generate("kernel/sched/core.c");
        let index = test_index(
            vec![test_symbol],
            [
                (production_file, "kernel/sched"),
                (
                    FileId::generate("tools/testing/preempt_lock.c"),
                    "tools/testing",
                ),
            ],
        );

        assert!(
            index
                .find_exact_name_target("preempt_disable", Some(production_file))
                .is_none()
        );
    }

    #[test]
    fn exact_name_target_allows_test_to_test_symbol() {
        let target = test_symbol("helper", "tests/helper.ts");
        let test_caller = FileId::generate("tests/caller.ts");
        let index = test_index(
            vec![target.clone()],
            [(target.file_id, "tests"), (test_caller, "tests")],
        );

        let matched = index
            .find_exact_name_target("helper", Some(test_caller))
            .unwrap();
        assert_eq!(matched.symbol_id, target.id);
    }
}
