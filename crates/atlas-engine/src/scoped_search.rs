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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use db::{DiscoveredFile, Store};
use filesync::discovery::{DiscoveryConfig, discover_files_bounded};
use search::query_parser::parse_query;
use search::scoring::{ScoreWeights, language_preference_bonus};
use types::structs::AnswerQuality;
use types::{FactCoverage, Language, ReferenceKind, SymbolDef, SymbolKind};

use crate::LazyStructuralService;

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
    pub capability_mask: FactCoverage,
    /// AnswerQuality of lazy extraction (if triggered).
    pub quality: AnswerQuality,
    /// Warnings for the user.
    pub warnings: Vec<String>,
    /// File ids that should be parsed by background focus warming.
    pub deferred_file_ids: Vec<types::ids::FileId>,
}

// ── Service ─────────────────────────────────────────────────────────────────

/// Shared search orchestration.
///
/// Uses Focus materialize structural ensure (not a private Engine stack).
pub struct ScopedSearchService {
    store: Arc<Store>,
    structural: LazyStructuralService,
    project_root: Option<PathBuf>,
}

impl ScopedSearchService {
    /// Create a new service with Focus structural materialize.
    pub fn new(store: Arc<Store>, structural: LazyStructuralService) -> Self {
        Self {
            store,
            structural,
            project_root: None,
        }
    }

    /// Create a new service with a project root for cold-scope inventory seeding.
    pub fn new_with_project_root(
        store: Arc<Store>,
        structural: LazyStructuralService,
        project_root: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            structural,
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
    /// 4. Run scoped fact search: exact `@Decorator` queries use Decoration
    ///    references; symbol queries use FTS5 → exact name → LIKE fallback.
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
        let decorator_name = decorator_query_name(&term);

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
        let mut inventory_discovery_complete = true;
        let mut inventory_scope_count = self
            .store
            .count_file_inventory_in_scope(&normalized_scope)
            .unwrap_or(0);
        if scope_file_count == 0 && inventory_scope_count == 0 {
            if let Some(root) = &self.project_root {
                inventory_discovery_complete =
                    seed_file_inventory_from_scope(&self.store, root, &normalized_scope)?;
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
                "Using focus file inventory for files not yet present in indexed file facts"
                    .to_string(),
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
                capability_mask: FactCoverage::default(),
                quality: AnswerQuality::worst(),
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

        let mut quality = AnswerQuality::worst();
        let mut capability_mask = FactCoverage::from_layers(&["manifest"]);
        let mut scope_has_structural_coverage = false;

        // If the DB already has full structural data (e.g. after
        // `atlas index --analysis full`), report Exact precision and skip
        // lazy extraction — there is nothing to gain from re-extracting.
        // Run this check regardless of analysis mode so precision is
        // correctly reported even for Manifest-only searches.
        if !inventory_backed
            && self.store.scope_has_fresh_complete_fact(
                &normalized_scope,
                FactCoverage::from_bits(FactCoverage::STRUCTURAL),
            )?
        {
            scope_has_structural_coverage = true;
            quality = AnswerQuality::best();
            capability_mask = FactCoverage::from_layers(&["manifest", "structural"]);
            if should_trigger_lazy {
                should_trigger_lazy = false;
                warnings.push(
                    "Full structural index already present; skipping lazy extraction".to_string(),
                );
            }
        }

        // 4. Manifest search.
        let candidate_multiplier = 4;
        let candidate_limit = (req.limit.saturating_mul(candidate_multiplier)).clamp(50, 1000);

        let run_search = || {
            if let Some(name) = decorator_name {
                search_decorated_symbols(
                    &self.store,
                    name,
                    &normalized_scope,
                    kind_filter,
                    language,
                )
            } else {
                search_symbols_scoped(
                    &self.store,
                    &term,
                    &normalized_scope,
                    candidate_limit,
                    kind_filter,
                    language,
                    scope_file_count,
                )
            }
        };

        let mut symbols = run_search()?;

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
        let decorator_needs_refinement = decorator_name.is_some() && !scope_has_structural_coverage;
        if (symbols.is_empty()
            || decorator_needs_refinement
            || inventory_backed
            || !scope_has_structural_coverage)
            && !term.is_empty()
            && (should_trigger_lazy || inventory_backed)
        {
            if inventory_backed && !should_trigger_lazy {
                let ensured = self
                    .structural
                    .ensure_structural_for_symbol_in_scope_limited(
                        &term,
                        Some(&normalized_scope),
                        COLD_SEARCH_MAX_SYNC_LAZY_FILES,
                    )?;
                triggered_lazy = ensured.files_built > 0 || ensured.files_cached > 0;
                lazy_covered_scope = scope_file_count <= ensured.files_built + ensured.files_cached
                    && !ensured.budget_exceeded;
                if triggered_lazy {
                    quality = ensured.quality.clone();
                    capability_mask = FactCoverage::from_layers(&["manifest", "structural"]);
                }
                if ensured.budget_exceeded {
                    warnings.push(
                        "Cold search parsed only a bounded candidate subset; narrow the scope for exact results."
                            .to_string(),
                    );
                }
                deferred_file_ids.extend(ensured.deferred_file_ids.iter().copied());
                symbols = run_search()?;
            } else {
                let mut file_ids = if inventory_backed {
                    self.store
                        .list_file_inventory_ids_in_scope(&normalized_scope, 100)
                        .unwrap_or_default()
                } else if normalized_scope.is_empty() {
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
                    let ensured = self.structural.ensure_structural_for_file_ids(&file_ids)?;
                    triggered_lazy = true;
                    lazy_covered_scope = requested_files >= scope_file_count
                        && !ensured.budget_exceeded
                        && !truncated_for_latency;
                    quality = ensured.quality.clone();
                    capability_mask = FactCoverage::from_layers(&["manifest", "structural"]);

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
                    symbols = run_search()?;
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
                let name_score = if decorator_name.is_some() {
                    1.0
                } else {
                    simple_score(&term, &sym)
                };
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
        } else if lazy_truncated_for_latency
            || (inventory_backed && (!inventory_discovery_complete || !lazy_covered_scope))
        {
            SearchCoverage::Partial {
                reason: "Focus inventory is available, but only a bounded subset has structural facts for this query".to_string(),
            }
        } else if decorator_name.is_some() && !scope_has_structural_coverage && !lazy_covered_scope
        {
            SearchCoverage::Partial {
                reason: "Decorator search requires structural facts for every file in scope"
                    .to_string(),
            }
        } else if !matches!(req.analysis, SearchAnalysis::Manifest)
            && !scope_has_structural_coverage
            && !lazy_covered_scope
        {
            SearchCoverage::Partial {
                reason: "Structural facts are incomplete for part of the search scope".to_string(),
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
            quality,
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

fn decorator_query_name(query: &str) -> Option<&str> {
    let name = query.strip_prefix('@')?;
    let mut bytes = name.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
    {
        return None;
    }
    Some(name)
}

fn search_decorated_symbols(
    store: &Store,
    decorator_name: &str,
    scope: &str,
    kind_filter: Option<SymbolKind>,
    language: Option<Language>,
) -> anyhow::Result<Vec<SymbolDef>> {
    let references = store.find_references_by_name_and_kind_in_scope(
        decorator_name,
        ReferenceKind::Decoration,
        scope,
    )?;
    if references.is_empty() {
        return Ok(Vec::new());
    }

    let file_ids = references
        .iter()
        .map(|reference| reference.file_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut symbols_by_file: HashMap<_, Vec<_>> = HashMap::new();
    const FILE_QUERY_BATCH: usize = 500;
    for batch in file_ids.chunks(FILE_QUERY_BATCH) {
        for symbol in store.find_symbols_by_files(batch)? {
            symbols_by_file
                .entry(symbol.file_id)
                .or_default()
                .push(symbol);
        }
    }

    let mut seen = HashSet::new();
    let mut symbols = Vec::new();
    for reference in references {
        let Some(file_symbols) = symbols_by_file.get(&reference.file_id) else {
            continue;
        };
        let target = file_symbols
            .iter()
            .filter(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Class
                        | SymbolKind::Struct
                        | SymbolKind::Function
                        | SymbolKind::Method
                        | SymbolKind::Field
                        | SymbolKind::Property
                ) && symbol.range.start_byte <= reference.range.start_byte
                    && symbol.range.end_byte >= reference.range.end_byte
                    && kind_filter.is_none_or(|kind| symbol.kind == kind)
                    && language.is_none_or(|lang| symbol.language == lang)
            })
            .min_by_key(|symbol| symbol.range.byte_len());
        if let Some(symbol) = target.filter(|symbol| seen.insert(symbol.id)) {
            symbols.push(symbol.clone());
        }
    }

    let mut file_paths = HashMap::new();
    for file_id in file_ids {
        let path = store
            .get_file(&file_id)?
            .map(|file| file.path)
            .unwrap_or_default();
        file_paths.insert(file_id, path);
    }
    symbols.sort_by(|left, right| {
        file_paths
            .get(&left.file_id)
            .cmp(&file_paths.get(&right.file_id))
            .then(left.range.start_line.cmp(&right.range.start_line))
            .then(left.qualified_name.cmp(&right.qualified_name))
    });
    Ok(symbols)
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

const COLD_SCOPE_INVENTORY_LIMIT: usize = 2_000;
const COLD_SCOPE_INVENTORY_TIMEOUT_MS: u64 = 1_500;

/// Populate Tier-0 file inventory for a project-relative scope.
///
/// This performs cheap discovery only: no parsing, no graph writes, and no
/// `files` table upsert. `files` remains the materialized-facts catalog, while
/// `file_inventory` is the cold-start manifest for Focus entry points.
pub fn seed_file_inventory_from_scope(
    store: &Store,
    project_root: &Path,
    scope: &str,
) -> anyhow::Result<bool> {
    let normalized_scope = scope
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/');
    let include_patterns = if normalized_scope.is_empty() || normalized_scope == "." {
        Vec::new()
    } else {
        vec![
            normalized_scope.to_string(),
            format!("{normalized_scope}/**"),
        ]
    };
    let config = DiscoveryConfig {
        include_patterns,
        exclude_patterns: Vec::new(),
    };
    let (files, complete) = discover_files_bounded(
        project_root,
        &config,
        COLD_SCOPE_INVENTORY_LIMIT,
        Duration::from_millis(COLD_SCOPE_INVENTORY_TIMEOUT_MS),
    )?;

    for rel in files {
        let Some(language) = Language::from_path(&rel) else {
            continue;
        };
        let abs_path = project_root.join(&rel);
        let metadata = match std::fs::symlink_metadata(&abs_path) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
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

        store.insert_file_inventory(&DiscoveredFile {
            file_id,
            path: rel_path,
            language,
            mtime,
            size: metadata.len() as i64,
            inode,
            dev,
        })?;
    }
    Ok(complete)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FocusMaterialize;
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

    fn seed_decorated_struct(store: &Store, path: &str, name: &str, decorator: &str) {
        let source = format!("@{decorator}\nstruct {name} {{\n  build() {{}}\n}}\n");
        let frontend = extraction::create_frontend(Language::ArkTS).unwrap();
        let facts = extraction::extract_file_with_mode(
            &frontend,
            types::FileId::generate(path),
            Path::new(path),
            &source,
            "decorator-search-test",
            extraction::ExtractionMode::Full,
            &(),
        )
        .unwrap();
        store.insert_file_facts(&facts).unwrap();
    }

    fn test_service() -> ScopedSearchService {
        let store = Arc::new(Store::open_in_memory().expect("open in-memory store"));
        store.init_schema().expect("init schema");
        let m = FocusMaterialize::open(Arc::clone(&store), None);
        ScopedSearchService::new(store, m.structural().clone())
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
                FactCoverage::from_layers(&["manifest", "structural"]),
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
            resp.quality.is_exact(),
            "precision should be Exact when structural data exists"
        );
        assert!(
            resp.capability_mask.has(FactCoverage::STRUCTURAL),
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
                FactCoverage::from_layers(&["manifest", "structural"]),
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
            resp.quality.is_exact(),
            "precision should be Exact when structural data exists"
        );
        assert!(
            resp.capability_mask.has(FactCoverage::STRUCTURAL),
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

    #[test]
    fn execute_decorator_query_owns_scope_filters_limits_and_totals() {
        let svc = test_service();
        seed_decorated_struct(&svc.store, "src/Widget.ets", "Widget", "Component");
        seed_decorated_struct(&svc.store, "src-other/Other.ets", "Other", "Component");
        seed_file_with_symbol(
            &svc.store,
            "src/component.ts",
            Language::TypeScript,
            "Component",
            "Component",
            SymbolKind::Function,
        );

        let response = svc
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(response.total, 1);
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].symbol.name, "Widget");
        assert_eq!(response.results[0].score.name_score, 1.0);

        let root = svc
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some(".".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(root.total, 2);
        assert_eq!(
            root.results
                .iter()
                .map(|result| result.symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Other", "Widget"]
        );

        let limited = svc
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Manifest,
                limit: 0,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(limited.total, 1);
        assert!(limited.results.is_empty());

        let filtered = svc
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                kind: Some(SymbolKind::Method),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered.total, 0);
        assert!(filtered.results.is_empty());

        let language_filtered = svc
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                language: Some(Language::TypeScript),
                analysis: SearchAnalysis::Manifest,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(language_filtered.total, 0);
        assert!(language_filtered.results.is_empty());
    }

    #[test]
    fn execute_decorator_query_refines_manifest_file_to_structural() {
        let project = tempfile::tempdir().unwrap();
        let source_dir = project.path().join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = "@Component\nstruct Widget {\n  build() {}\n}\n";
        std::fs::write(source_dir.join("Widget.ets"), source).unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let file_id = types::FileId::generate("src/Widget.ets");
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/Widget.ets".to_string(),
                language: Language::ArkTS,
                content_hash: content_hash.clone(),
                status: ParseStatus::Partial,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                "manifest",
                &content_hash,
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            )
            .unwrap();

        let materialize =
            FocusMaterialize::open(Arc::clone(&store), Some(project.path().to_path_buf()));
        let service = ScopedSearchService::new_with_project_root(
            store,
            materialize.structural().clone(),
            Some(project.path().to_path_buf()),
        );
        let response = service
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Structural,
                limit: 10,
                ..Default::default()
            })
            .unwrap();

        assert!(response.triggered_lazy);
        assert_eq!(response.total, 1);
        assert_eq!(response.results[0].symbol.name, "Widget");
        assert_eq!(response.results[0].symbol.kind, SymbolKind::Struct);
    }

    #[test]
    fn execute_decorator_query_refines_mixed_manifest_and_structural_scope() {
        let project = tempfile::tempdir().unwrap();
        let source_dir = project.path().join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let first_source = "@Component\nstruct First {\n  build() {}\n}\n";
        let second_source = "@Component\nstruct Second {\n  build() {}\n}\n";
        std::fs::write(source_dir.join("First.ets"), first_source).unwrap();
        std::fs::write(source_dir.join("Second.ets"), second_source).unwrap();

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        seed_decorated_struct(&store, "src/First.ets", "First", "Component");

        let second_id = types::FileId::generate("src/Second.ets");
        let second_hash = blake3::hash(second_source.as_bytes()).to_hex().to_string();
        store
            .upsert_file(&FileInfo {
                file_id: second_id,
                path: "src/Second.ets".to_string(),
                language: Language::ArkTS,
                content_hash: second_hash.clone(),
                status: ParseStatus::Partial,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &second_id,
                "manifest",
                &second_hash,
                "complete",
                FactCoverage::from_layers(&["manifest"]),
            )
            .unwrap();

        let materialize =
            FocusMaterialize::open(Arc::clone(&store), Some(project.path().to_path_buf()));
        let service = ScopedSearchService::new_with_project_root(
            store,
            materialize.structural().clone(),
            Some(project.path().to_path_buf()),
        );
        let response = service
            .execute(ScopedSearchRequest {
                query: "@Component".into(),
                scope: Some("src".into()),
                analysis: SearchAnalysis::Structural,
                limit: 10,
                ..Default::default()
            })
            .unwrap();

        assert!(response.triggered_lazy);
        assert_eq!(response.total, 2);
        assert_eq!(
            response
                .results
                .iter()
                .map(|result| result.symbol.name.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
        assert!(matches!(response.coverage, SearchCoverage::Full));
    }
}
