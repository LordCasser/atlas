//! ScopedSearchService: shared search orchestration with scope parsing, lazy
//! structural triggering, and result assembly.
//!
//! This service encapsulates the search semantics that were previously duplicated
//! across the MCP (`atlas-mcp/src/tools/search.rs`) and TUI
//! (`atlas-cli/src/tui/search_session.rs`) entry points.  Both entry points can
//! wrap this single service instead of independently re-implementing scope
//! resolution, analysis-mode selection, and lazy-structural fallback.
//!
//! # Flow
//!
//! ```text
//!   parse query  →  resolve scope files  →  manifest search
//!                                              │
//!                              ┌─── empty? ────┤
//!                              │                │
//!                         analysis ≥          results found
//!                         Structural?             │
//!                              │                  │
//!                    trigger lazy structural      │
//!                    re-search                    │
//!                              │                  │
//!                              └──── assemble ────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use atlas_engine::{
//!     ScopedSearchService, ScopedSearchRequest, SearchAnalysis,
//! };
//!
//! let svc = ScopedSearchService::new(
//!     store, engine,
//! );
//! let resp = svc.execute(ScopedSearchRequest {
//!     query: "handleRequest".into(),
//!     scope: Some("src".into()),
//!     analysis: SearchAnalysis::Auto,
//!     limit: 20,
//!     ..Default::default()
//! })?;
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use db::Store;
use search::query_parser::parse_query;
use search::scoring::{ScoreWeights, language_preference_bonus};
use types::structs::Precision;
use types::{CapabilityMask, Language, SymbolDef, SymbolKind};

use crate::Engine;

// ── Public request / response types ─────────────────────────────────────────

/// Search analysis mode — controls whether lazy structural extraction is
/// attempted when manifest results are empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchAnalysis {
    /// Manifest only — query existing DB facts, never trigger lazy extraction.
    Manifest,
    /// Structural — always trigger bounded lazy structural if manifest yields
    /// empty results.
    Structural,
    /// Auto — decide based on scope file count: small scope (≤30 files) uses
    /// structural, large scope uses manifest-only.
    Auto,
}

/// Request for a scoped search.
#[derive(Debug, Clone)]
pub struct ScopedSearchRequest {
    /// Raw user query (may contain `lang:`, `name:`, `kind:`, `path:` prefixes).
    pub query: String,
    /// Project-relative scope directory or file pattern (`None` = project root).
    pub scope: Option<String>,
    /// Language filter extracted from query prefix or set explicitly.
    pub language: Option<Language>,
    /// Symbol kind filter.
    pub kind: Option<SymbolKind>,
    /// Analysis depth.
    pub analysis: SearchAnalysis,
    /// Max results to return.
    pub limit: usize,
    /// C/C++ include roots for header resolution during lazy structural.
    pub include_roots: Vec<String>,
}

impl Default for ScopedSearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            scope: None,
            language: None,
            kind: None,
            analysis: SearchAnalysis::Auto,
            limit: 20,
            include_roots: Vec::new(),
        }
    }
}

/// Coverage of the search response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCoverage {
    /// All relevant files were searched.
    Full,
    /// Only a subset could be searched.
    Partial { reason: String },
}

/// Response from a scoped search.
#[derive(Debug, Clone)]
pub struct ScopedSearchResponse {
    /// Search results (symbol matches).
    pub results: Vec<search::SearchResult>,
    /// Total results available (may exceed `results.len()`).
    pub total: usize,
    /// Number of indexed files in the resolved search scope.
    pub scope_file_count: usize,
    /// Coverage level.
    pub coverage: SearchCoverage,
    /// Whether lazy structural was triggered on this request.
    pub triggered_lazy: bool,
    /// Capability mask achieved.
    pub capability_mask: CapabilityMask,
    /// Precision of lazy extraction (if triggered).
    pub precision: Precision,
    /// Warnings for the user.
    pub warnings: Vec<String>,
    /// File ids that should be parsed by background focus warming.
    pub deferred_file_ids: Vec<types::ids::FileId>,
}

// ── Service ─────────────────────────────────────────────────────────────────

/// Shared search orchestration.
///
/// Owns a reference to the database store, the high-level [`Engine`] (for lazy
/// structural access), and the project root (for on-disk source resolution
/// during lazy extraction).
pub struct ScopedSearchService {
    store: Arc<Store>,
    engine: Arc<Engine>,
    project_root: Option<PathBuf>,
}

impl ScopedSearchService {
    /// Create a new service.
    pub fn new(store: Arc<Store>, engine: Arc<Engine>) -> Self {
        Self {
            store,
            engine,
            project_root: None,
        }
    }

    /// Create a new service with a project root for cold-scope inventory seeding.
    pub fn new_with_project_root(
        store: Arc<Store>,
        engine: Arc<Engine>,
        project_root: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            engine,
            project_root,
        }
    }

    /// Execute a scoped search.
    ///
    /// # Flow
    ///
    /// 1. Parse the query to extract term, language filter, kind filter via
    ///    [`parse_query`].  Explicit request-level filters take priority over
    ///    parsed prefix filters.
    /// 2. Determine scope file set and count from the store.
    /// 3. Decide analysis level: [`SearchAnalysis::Manifest`] skips lazy;
    ///    [`SearchAnalysis::Structural`] always tries lazy on empty; [`SearchAnalysis::Auto`]
    ///    uses structural when `scope_file_count ≤ 30`.
    /// 4. Run manifest search using store scoped-symbol methods (FTS5 → name
    ///    exact → LIKE fallback).
    /// 5. If empty results AND analysis ≥ Structural:
    ///    - Collect file IDs in scope.
    ///    - Trigger [`LazyStructuralService::ensure_structural_for_file_ids`].
    ///    - Re-search.
    /// 6. Return [`ScopedSearchResponse`] with results, scores, coverage,
    ///    capability mask, precision tier, and warnings.
    pub fn execute(&self, req: ScopedSearchRequest) -> anyhow::Result<ScopedSearchResponse> {
        // 1. Parse query — extract structured prefixes from the raw input.
        let parsed = parse_query(&req.query);
        let term = if !parsed.freetext.is_empty() {
            parsed.freetext
        } else {
            parsed.name_filter.unwrap_or_default()
        };
        // Request-level filters take priority over parsed prefix filters.
        let language = req.language.or(parsed.language);
        let kind_filter = req.kind.or(parsed.kind_filter);

        // 2. Normalize scope and count files in scope.
        let normalized_scope = req
            .scope
            .as_deref()
            .map(normalize_scope)
            .unwrap_or_default();
        let mut warnings: Vec<String> = Vec::new();

        let mut scope_file_count = if normalized_scope.is_empty() {
            self.store.count_files()?
        } else {
            self.store.count_files_in_scope(&normalized_scope)?
        };
        let mut inventory_backed = false;
        let mut inventory_scope_count = self
            .store
            .count_file_inventory_in_scope(&normalized_scope)
            .unwrap_or(0);
        if scope_file_count == 0 && inventory_scope_count == 0 {
            if let Some(root) = &self.project_root {
                seed_inventory_from_scope(&self.store, root, &normalized_scope)?;
                inventory_scope_count = self
                    .store
                    .count_file_inventory_in_scope(&normalized_scope)
                    .unwrap_or(0);
            }
        }
        if inventory_scope_count > scope_file_count {
            scope_file_count = inventory_scope_count;
            inventory_backed = true;
            warnings.push(
                "Using focus file inventory because no manifest index exists yet".to_string(),
            );
        }

        // No files at all — bail early.
        if scope_file_count == 0 {
            if !normalized_scope.is_empty() {
                warnings.push(format!("Scope '{normalized_scope}' has no indexed files"));
            }
            return Ok(ScopedSearchResponse {
                results: Vec::new(),
                total: 0,
                scope_file_count: 0,
                coverage: SearchCoverage::Partial {
                    reason: "No indexed files in scope".to_string(),
                },
                triggered_lazy: false,
                capability_mask: CapabilityMask::default(),
                precision: Precision::worst(),
                warnings,
                deferred_file_ids: Vec::new(),
            });
        }

        // 3. Decide whether to attempt lazy structural on empty results.
        const AUTO_STRUCTURAL_THRESHOLD: usize = 30;
        let mut should_trigger_lazy = match req.analysis {
            SearchAnalysis::Manifest => false,
            SearchAnalysis::Structural => true,
            SearchAnalysis::Auto => scope_file_count <= AUTO_STRUCTURAL_THRESHOLD,
        };

        let mut precision = Precision::worst();
        let mut capability_mask = CapabilityMask::from_layers(&["manifest"]);

        // If the DB already has full structural data (e.g. after
        // `atlas index --analysis full`), report Exact precision and skip
        // lazy extraction — there is nothing to gain from re-extracting.
        // Run this check regardless of analysis mode so precision is
        // correctly reported even for Manifest-only searches.
        {
            let file_ids = if normalized_scope.is_empty() {
                self.store
                    .list_files()?
                    .into_iter()
                    .take(200)
                    .map(|f| f.file_id)
                    .collect()
            } else {
                self.store.list_file_ids_in_scope(&normalized_scope, 200)?
            };
            if !file_ids.is_empty() {
                let capability = self.store.derive_capability_for_files(&file_ids);
                if capability.has(CapabilityMask::STRUCTURAL) {
                    precision = Precision::best();
                    capability_mask = CapabilityMask::from_layers(&["manifest", "structural"]);
                    if should_trigger_lazy {
                        should_trigger_lazy = false;
                        warnings.push(
                            "Full structural index already present; skipping lazy extraction"
                                .to_string(),
                        );
                    }
                }
            }
        }

        // 4. Manifest search.
        let candidate_multiplier = 4;
        let candidate_limit = (req.limit.saturating_mul(candidate_multiplier)).clamp(50, 1000);

        let mut symbols = search_symbols_scoped(
            &self.store,
            &term,
            &normalized_scope,
            candidate_limit,
            kind_filter,
            language,
            scope_file_count,
        )?;

        let mut triggered_lazy = false;
        let mut lazy_covered_scope = false;
        let mut lazy_truncated_for_latency = false;
        let mut deferred_file_ids = Vec::new();

        // 5. Trigger lazy structural if manifest returned nothing and policy
        //    says we should try. For inventory-backed projects there is no
        //    manifest index yet, so use the query text to locate a bounded
        //    candidate set instead of expanding an arbitrary prefix of a large
        //    scope.
        const COLD_SEARCH_MAX_SYNC_LAZY_FILES: usize = 2;
        if symbols.is_empty() && !term.is_empty() && (should_trigger_lazy || inventory_backed) {
            if inventory_backed && !should_trigger_lazy {
                let ensured = self
                    .engine
                    .lazy_structural()
                    .ensure_structural_for_symbol_in_scope_limited(
                        &term,
                        Some(&normalized_scope),
                        COLD_SEARCH_MAX_SYNC_LAZY_FILES,
                    )?;
                triggered_lazy = ensured.files_built > 0 || ensured.files_cached > 0;
                lazy_covered_scope = scope_file_count <= ensured.files_built + ensured.files_cached
                    && !ensured.budget_exceeded;
                if triggered_lazy {
                    precision = ensured.precision.clone();
                    capability_mask = CapabilityMask::from_layers(&["manifest", "structural"]);
                }
                if ensured.budget_exceeded {
                    warnings.push(
                        "Cold search parsed only a bounded candidate subset; narrow the scope for exact results."
                            .to_string(),
                    );
                }
                deferred_file_ids.extend(ensured.deferred_file_ids.iter().copied());
                symbols = search_symbols_scoped(
                    &self.store,
                    &term,
                    &normalized_scope,
                    candidate_limit,
                    kind_filter,
                    language,
                    scope_file_count,
                )?;
            } else {
                let mut file_ids = if normalized_scope.is_empty() {
                    self.store
                        .list_files()?
                        .into_iter()
                        .map(|f| f.file_id)
                        .collect()
                } else {
                    self.store.list_file_ids_in_scope(&normalized_scope, 100)?
                };
                if file_ids.is_empty() {
                    file_ids = self
                        .store
                        .list_file_inventory_ids_in_scope(&normalized_scope, 100)
                        .unwrap_or_default();
                }

                if !file_ids.is_empty() {
                    let truncated_for_latency = matches!(req.analysis, SearchAnalysis::Auto)
                        && file_ids.len() > COLD_SEARCH_MAX_SYNC_LAZY_FILES;
                    if truncated_for_latency {
                        deferred_file_ids.extend(
                            file_ids
                                .iter()
                                .skip(COLD_SEARCH_MAX_SYNC_LAZY_FILES)
                                .copied(),
                        );
                        file_ids.truncate(COLD_SEARCH_MAX_SYNC_LAZY_FILES);
                        lazy_truncated_for_latency = true;
                    }
                    let requested_files = file_ids.len();
                    let ensured = self
                        .engine
                        .lazy_structural()
                        .ensure_structural_for_file_ids(&file_ids)?;
                    triggered_lazy = true;
                    lazy_covered_scope = requested_files >= scope_file_count
                        && !ensured.budget_exceeded
                        && !truncated_for_latency;
                    precision = ensured.precision.clone();
                    capability_mask = CapabilityMask::from_layers(&["manifest", "structural"]);

                    if truncated_for_latency {
                        warnings.push(
                            "Cold search parsed only a bounded file subset; narrow the scope for exact results."
                                .to_string(),
                        );
                    } else if ensured.budget_exceeded {
                        warnings.push(
                            "Structural parsing hit budget; narrow the scope for exact results."
                                .to_string(),
                        );
                    }

                    // Re-search after fresh structural data.
                    symbols = search_symbols_scoped(
                        &self.store,
                        &term,
                        &normalized_scope,
                        candidate_limit,
                        kind_filter,
                        language,
                        scope_file_count,
                    )?;
                }
                // else: scope exists but no files — nothing to extract.
            }
        }

        let total = symbols.len();
        symbols.truncate(req.limit);

        // Convert SymbolDef → SearchResult with simple name scoring.
        let results: Vec<search::SearchResult> = symbols
            .into_iter()
            .map(|sym| {
                let file_path = self
                    .store
                    .get_file(&sym.file_id)
                    .ok()
                    .flatten()
                    .map(|info| info.path);
                let name_score = simple_score(&term, &sym);
                search::SearchResult {
                    symbol: sym,
                    score: search::scoring::SearchScore {
                        name_score,
                        total: name_score,
                        ..Default::default()
                    },
                    matched_field: String::new(),
                    snippet: None,
                    file_path,
                }
            })
            .collect();

        let coverage = if scope_file_count == 0 {
            SearchCoverage::Partial {
                reason: "No indexed files".to_string(),
            }
        } else if lazy_truncated_for_latency || (inventory_backed && !lazy_covered_scope) {
            SearchCoverage::Partial {
                reason: "Focus inventory is available, but only a bounded subset has structural facts for this query".to_string(),
            }
        } else {
            SearchCoverage::Full
        };

        Ok(ScopedSearchResponse {
            results,
            total,
            scope_file_count,
            coverage,
            triggered_lazy,
            capability_mask,
            precision,
            warnings,
            deferred_file_ids,
        })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Normalize a user-provided scope path.
///
/// Strips `./`, `/` prefixes and `/` suffixes.  `"."` (project root) normalizes
/// to `""` so store methods treat it as "all files".
fn normalize_scope(scope: &str) -> String {
    let s = scope.trim();
    if s == "." {
        return String::new();
    }
    s.trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

/// Simple name-based relevance score (0.0–1.0).
fn simple_score(query: &str, sym: &SymbolDef) -> f64 {
    let q = query.to_lowercase();
    let name = sym.name.to_lowercase();
    let qname = sym.qualified_name.to_lowercase();
    if name == q {
        1.0
    } else if name.starts_with(&q) {
        0.9
    } else if name.contains(&q) {
        0.75
    } else if qname.contains(&q) {
        0.6
    } else {
        0.35
    }
}

/// Run a three-tier symbol search within a scope using store methods.
///
/// Tiers:
///   1. Exact name match (indexed).
///   2. FTS5 full-text.
///   3. LIKE substring fallback (only for small scopes).
fn search_symbols_scoped(
    store: &Store,
    query: &str,
    scope: &str,
    limit: usize,
    kind_filter: Option<SymbolKind>,
    language: Option<Language>,
    scope_file_count: usize,
) -> anyhow::Result<Vec<SymbolDef>> {
    const LIKE_FALLBACK_SCOPE_LIMIT: usize = 32;

    if scope.is_empty() {
        // ── Non-scoped: search entire project ───────────────────────────
        let mut symbols = store.find_symbols_by_name(query)?;
        if let Some(lang) = language {
            symbols.retain(|s| s.language == lang);
        }
        if symbols.is_empty() {
            symbols = store.search_symbols(query)?;
            if let Some(lang) = language {
                symbols.retain(|s| s.language == lang);
            }
        }
        if symbols.is_empty() && query.len() >= 2 {
            symbols = store.search_symbols_by_name_like(
                query,
                language.as_ref(),
                limit,
                kind_filter.as_ref(),
            )?;
        }
        if let Some(kind) = kind_filter {
            symbols.retain(|s| s.kind == kind);
        }
        let preferred_language = if language.is_none() {
            store.dominant_language().ok().flatten()
        } else {
            None
        };
        symbols.sort_by(|a, b| {
            ranked_simple_score(query, b, preferred_language)
                .partial_cmp(&ranked_simple_score(query, a, preferred_language))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.qualified_name.cmp(&b.qualified_name))
        });
        symbols.truncate(limit);
        return Ok(symbols);
    }

    // ── Scoped: filter by directory prefix at SQL level ────────────────
    let mut symbols =
        store.find_symbols_by_name_in_scope(query, scope, limit, kind_filter.as_ref())?;
    if let Some(lang) = language {
        symbols.retain(|s| s.language == lang);
    }
    if symbols.is_empty() {
        symbols =
            store.search_symbols_in_scope_with_limit(query, scope, limit, kind_filter.as_ref())?;
        if let Some(lang) = language {
            symbols.retain(|s| s.language == lang);
        }
    }
    if symbols.is_empty() && query.len() >= 2 && scope_file_count <= LIKE_FALLBACK_SCOPE_LIMIT {
        symbols = store.search_symbols_by_name_like_in_scope(
            query,
            scope,
            language.as_ref(),
            limit,
            kind_filter.as_ref(),
        )?;
    }
    let preferred_language = if language.is_none() {
        store
            .dominant_language_in_scope(scope)
            .ok()
            .flatten()
            .or_else(|| store.dominant_language().ok().flatten())
    } else {
        None
    };
    symbols.sort_by(|a, b| {
        ranked_simple_score(query, b, preferred_language)
            .partial_cmp(&ranked_simple_score(query, a, preferred_language))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    symbols.truncate(limit);
    Ok(symbols)
}

fn ranked_simple_score(query: &str, sym: &SymbolDef, preferred_language: Option<Language>) -> f64 {
    let weights = ScoreWeights::default();
    simple_score(query, sym)
        + language_preference_bonus(preferred_language == Some(sym.language)) * weights.language
}

const COLD_SCOPE_INVENTORY_LIMIT: usize = 100;
const COLD_SCOPE_INVENTORY_TIMEOUT_MS: u64 = 500;

fn seed_inventory_from_scope(
    store: &Store,
    project_root: &Path,
    scope: &str,
) -> anyhow::Result<()> {
    let start_dir = if scope.is_empty() {
        project_root.to_path_buf()
    } else {
        project_root.join(scope)
    };
    if !start_dir.exists() {
        return Ok(());
    }

    let canonical_root = project_root.canonicalize()?;
    let mut stack = vec![start_dir];
    let deadline = Instant::now() + Duration::from_millis(COLD_SCOPE_INVENTORY_TIMEOUT_MS);
    let mut inserted = 0usize;

    while let Some(dir) = stack.pop() {
        if inserted >= COLD_SCOPE_INVENTORY_LIMIT || Instant::now() >= deadline {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if inserted >= COLD_SCOPE_INVENTORY_LIMIT || Instant::now() >= deadline {
                break;
            }
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.')
                    || matches!(
                        name,
                        "target"
                            | "node_modules"
                            | "build"
                            | "dist"
                            | ".cache"
                            | "__pycache__"
                            | "venv"
                            | ".venv"
                    )
                {
                    continue;
                }
                if let Ok(canonical) = path.canonicalize() {
                    if canonical.starts_with(&canonical_root) {
                        stack.push(path);
                    }
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(project_root) else {
                continue;
            };
            let Some(language) = Language::from_path(rel) else {
                continue;
            };
            let rel_path = rel.to_string_lossy().replace('\\', "/");
            let file_id = types::ids::FileId::generate(&rel_path);
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default();

            #[cfg(unix)]
            let (inode, dev) = {
                use std::os::unix::fs::MetadataExt;
                (metadata.ino() as i64, metadata.dev() as i64)
            };
            #[cfg(not(unix))]
            let (inode, dev) = (0i64, 0i64);

            store.insert_file_inventory(
                &file_id,
                &rel_path,
                language.as_str(),
                mtime,
                metadata.len() as i64,
                inode,
                dev,
            )?;
            inserted += 1;
        }
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use db::Store;
    use types::{FileInfo, Language, ParseStatus};

    /// Seed a TypeScript file with a known symbol into the store.
    fn seed_ts_file(store: &Store, path: &str, source: &str) -> types::FileId {
        let file_id = types::FileId::generate(path);
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language: Language::TypeScript,
                content_hash,
                status: ParseStatus::Success,
            })
            .unwrap();

        // Manually insert a symbol so the store has manifest data to search.
        let sid = types::SymbolId::generate(&file_id, "ts", "handleRequest", "function", None);
        let sym = types::SymbolDef {
            id: sid,
            kind: SymbolKind::Function,
            name: "handleRequest".into(),
            qualified_name: "handleRequest".into(),
            symbol_path: vec![],
            file_id,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "manifest".to_string(),
        };
        store.insert_symbols(&[sym]).unwrap();
        file_id
    }

    fn seed_file_with_symbol(
        store: &Store,
        path: &str,
        language: Language,
        name: &str,
        qname: &str,
        kind: SymbolKind,
    ) -> types::FileId {
        let file_id = types::FileId::generate(path);
        store
            .upsert_file(&FileInfo {
                file_id,
                path: path.to_string(),
                language,
                content_hash: blake3::hash(path.as_bytes()).to_hex().to_string(),
                status: ParseStatus::Success,
            })
            .unwrap();
        let sid =
            types::SymbolId::generate(&file_id, language.as_str(), qname, kind.as_str(), None);
        let sym = types::SymbolDef {
            id: sid,
            kind,
            name: name.into(),
            qualified_name: qname.into(),
            symbol_path: vec![],
            file_id,
            language,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "manifest".to_string(),
        };
        store.insert_symbols(&[sym]).unwrap();
        file_id
    }

    fn test_service() -> ScopedSearchService {
        let store = Arc::new(Store::open_in_memory().expect("open in-memory store"));
        store.init_schema().expect("init schema");
        let engine = Arc::new(Engine::from_store(Arc::clone(&store), None));
        ScopedSearchService::new(store, engine)
    }

    // ── execute_manifest_returns_results ───────────────────────────────
    //
    // Verifies that a manifest-only search finds a symbol that was pre-seeded
    // in the store.
    #[test]
    fn execute_manifest_returns_results() {
        let svc = test_service();
        seed_ts_file(&svc.store, "src/handler.ts", "function handleRequest() {}");

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "handleRequest".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(!resp.results.is_empty(), "should find handleRequest");
        assert_eq!(resp.results[0].symbol.name, "handleRequest");
        assert!(
            !resp.triggered_lazy,
            "manifest mode should not trigger lazy"
        );
        assert_eq!(resp.total, 1);
        assert_eq!(resp.scope_file_count, 1);
        assert!(matches!(resp.coverage, SearchCoverage::Full));
    }

    // ── execute_empty_triggers_lazy ────────────────────────────────────
    //
    // Verifies that structural mode attempts lazy extraction when manifest
    // search returns no results.  Since we have no project root (in-memory
    // engine), lazy won't actually build files, but the service should still
    // enter the lazy path and report it.
    #[test]
    fn execute_empty_triggers_lazy() {
        let svc = test_service();

        // Seed a file so scope is non-empty, but with no symbol matching the
        // query — manifest search will return empty.
        seed_ts_file(&svc.store, "src/utils.ts", "function helper() {}");

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Structural,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(resp.results.is_empty(), "no results for nonexistent symbol");
        assert_eq!(resp.scope_file_count, 1);
        // Structural mode should report that lazy was triggered even though
        // extraction may not have produced results (no project root).
        assert!(resp.triggered_lazy, "structural mode should trigger lazy");
    }

    // ── execute_manifest_empty_does_not_trigger_lazy ───────────────────
    //
    // Verifies that manifest mode does NOT trigger lazy when results are empty.
    #[test]
    fn execute_manifest_empty_does_not_trigger_lazy() {
        let svc = test_service();
        seed_ts_file(&svc.store, "src/utils.ts", "function helper() {}");

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(resp.results.is_empty());
        assert_eq!(resp.scope_file_count, 1);
        assert!(
            !resp.triggered_lazy,
            "manifest mode should not trigger lazy"
        );
    }

    // ── execute_auto_skips_lazy_when_structural_present ──────────────
    //
    // Verifies that Auto mode skips lazy extraction when the DB already
    // contains full structural data (e.g. after `atlas index --analysis full`).
    #[test]
    fn execute_auto_skips_lazy_when_structural_present() {
        let svc = test_service();

        // Seed a file with a symbol and mark it as structurally extracted.
        let file_id = seed_ts_file(&svc.store, "src/handler.ts", "function handleRequest() {}");
        let content_hash = svc
            .store
            .get_file(&file_id)
            .expect("get_file")
            .expect("file should exist")
            .content_hash;
        svc.store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                &content_hash,
                "complete",
                CapabilityMask::from_layers(&["manifest", "structural"]),
            )
            .unwrap();

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Auto,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(
            !resp.triggered_lazy,
            "Auto should skip lazy when structural data is already present"
        );
        assert_eq!(resp.scope_file_count, 1);
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.contains("structural index already present")),
            "should warn about existing structural data"
        );
        assert!(
            resp.precision.is_exact(),
            "precision should be Exact when structural data exists"
        );
        assert!(
            resp.capability_mask.has(CapabilityMask::STRUCTURAL),
            "capability_mask should include STRUCTURAL when structural data exists"
        );
    }

    // ── test_manifest_mode_with_structural_existing ─────────────────
    //
    // Verifies that Manifest mode correctly reports Exact precision
    // when the DB already contains structural extraction state (the key
    // bug fix — previously precision remained Unavailable).
    #[test]
    fn test_manifest_mode_with_structural_existing() {
        let svc = test_service();

        // Seed a file with a symbol and mark it as structurally extracted.
        let file_id = seed_ts_file(&svc.store, "src/handler.ts", "function handleRequest() {}");
        let content_hash = svc
            .store
            .get_file(&file_id)
            .expect("get_file")
            .expect("file should exist")
            .content_hash;
        svc.store
            .upsert_file_extraction_state(
                &file_id,
                "structural",
                &content_hash,
                "complete",
                CapabilityMask::from_layers(&["manifest", "structural"]),
            )
            .unwrap();

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(
            !resp.triggered_lazy,
            "Manifest mode should never trigger lazy"
        );
        assert!(
            resp.precision.is_exact(),
            "precision should be Exact when structural data exists"
        );
        assert!(
            resp.capability_mask.has(CapabilityMask::STRUCTURAL),
            "capability_mask should include STRUCTURAL when structural data exists"
        );
    }

    // ── execute_auto_triggers_lazy_when_only_manifest ─────────────────
    //
    // Verifies that Auto mode still triggers lazy extraction when only
    // manifest data exists in the DB.
    #[test]
    fn execute_auto_triggers_lazy_when_only_manifest() {
        let svc = test_service();
        seed_ts_file(&svc.store, "src/utils.ts", "function helper() {}");

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Auto,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(
            resp.triggered_lazy,
            "Auto should trigger lazy when only manifest data exists"
        );
        assert_eq!(resp.scope_file_count, 1);
    }

    #[test]
    fn execute_auto_bounds_cold_lazy_file_count() {
        let svc = test_service();
        seed_ts_file(&svc.store, "src/a.ts", "function a() {}");
        seed_ts_file(&svc.store, "src/b.ts", "function b() {}");
        seed_ts_file(&svc.store, "src/c.ts", "function c() {}");

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "nonexistent".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Auto,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(resp.triggered_lazy, "Auto should still try cold lazy");
        assert!(
            resp.warnings
                .iter()
                .any(|w| w.contains("bounded file subset")),
            "cold Auto search should warn when synchronous parsing is bounded: {:?}",
            resp.warnings
        );
        assert!(matches!(resp.coverage, SearchCoverage::Partial { .. }));
    }

    #[test]
    fn execute_manifest_prefers_scope_dominant_language_for_same_name() {
        let svc = test_service();
        seed_file_with_symbol(
            &svc.store,
            "src/main.rs",
            Language::Rust,
            "App",
            "crate::App",
            SymbolKind::Variable,
        );
        seed_file_with_symbol(
            &svc.store,
            "src/lib.rs",
            Language::Rust,
            "Helper",
            "crate::Helper",
            SymbolKind::Variable,
        );
        seed_file_with_symbol(
            &svc.store,
            "src/app.tsx",
            Language::TypeScript,
            "App",
            "App",
            SymbolKind::Variable,
        );

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "App".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert_eq!(
            resp.results[0].symbol.language,
            Language::Rust,
            "Rust should rank first in a Rust-dominant scope"
        );
        assert!(
            resp.results
                .iter()
                .any(|r| r.symbol.language == Language::TypeScript),
            "soft preference should keep cross-language results visible"
        );
    }

    #[test]
    fn execute_manifest_language_filter_applies_to_exact_matches() {
        let svc = test_service();
        seed_file_with_symbol(
            &svc.store,
            "src/main.rs",
            Language::Rust,
            "App",
            "crate::App",
            SymbolKind::Variable,
        );
        seed_file_with_symbol(
            &svc.store,
            "src/app.tsx",
            Language::TypeScript,
            "App",
            "App",
            SymbolKind::Variable,
        );

        let resp = svc
            .execute(ScopedSearchRequest {
                query: "lang:typescript App".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .expect("execute should succeed");

        assert!(!resp.results.is_empty());
        assert!(
            resp.results
                .iter()
                .all(|r| r.symbol.language == Language::TypeScript),
            "explicit lang filter must filter exact-name results"
        );
    }
}
