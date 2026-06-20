//! Lazy Structural Service — on-demand structural extraction.
//!
//! When a query requests a symbol that only exists at the manifest layer
//! (top-level symbols from `atlas index --analysis manifest`), this service
//! automatically triggers a full structural extraction for the owning file,
//! followed by incremental reference resolution and graph building.
//!
//! # Components
//!
//! ```text
//! Query (search/context/trace)
//!     │
//!     v
//! LazyStructuralService
//!     ├─ CandidateProvider  (pluggable: FTS5 + ripgrep by default)
//!     └─ StructuralLoader   (re-extract + re-resolve + rebuild edges)
//! ```
//!
//! # Design constraints
//!
//! - Does NOT call `resolve_all()` or `GraphBuilder::build_all()` — uses
//!   incremental `resolve_for_files` / `build_for_files` instead.
//! - Leverages file-level extraction state for cache decisions.
//! - Respects the same `ExtractionMode::Structural` pipeline as `atlas index`.
//! - [`CandidateProvider`] is a trait — swap implementations for different
//!   discovery strategies (e.g. compile_commands.json, ctags, custom heuristics).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use db::Store;
use extraction::{
    CancelCheck, ExtractionMode, create_frontend, extract_file_with_mode,
    extract_file_with_mode_cancellable,
};
use types::ids::FileId;
use types::structs::Precision;
use types::{FileInfo, Language, ParseStatus, layer, status};

/// Maximum candidate files to consider for lazy structural loading.
const MAX_CANDIDATE_FILES: usize = 10;

/// Maximum time to spend asking ripgrep for lazy candidates.
///
/// Large repositories such as the Linux kernel can contain tens of thousands
/// of source files. Candidate discovery must be bounded independently from
/// extraction so a broad query never waits for ripgrep to scan the full tree.
const RG_CANDIDATE_TIMEOUT_MS: u64 = 1_500;

/// Wall-clock guard for a single lazy structural invocation (milliseconds).
///
/// ⚠ Loop-continuation guard only — does NOT interrupt in-flight extraction.
///    True hard timeout requires extraction worker isolation (future work).
///    The request-level budget in `LazyBudget` provides the real constraint.
pub(crate) const LAZY_STRUCTURAL_LOOP_GUARD_MS: u64 = 5_000;

/// Maximum file size (bytes) for lazy structural extraction.
///
/// Files exceeding this limit are soft-rejected with a diagnostic message
/// recommending `atlas index` for full indexing instead.
const LAZY_STRUCTURAL_MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// CandidateProvider trait
// ---------------------------------------------------------------------------

/// Pluggable strategy for discovering which files need structural extraction.
///
/// The default implementation queries the `symbols` table (FTS5) and falls
/// back to `ripgrep` when no indexed symbols match.  Alternative providers
/// could use `compile_commands.json`, ctags indexes, or custom heuristics.
pub trait CandidateProvider: Send + Sync {
    /// Find files that likely contain a definition or reference to `name`.
    fn candidates_for_symbol(&self, name: &str) -> Result<Vec<FileId>>;

    /// Find files matching the given path pattern.
    ///
    /// Default: treats `path` as a literal file path, generating a single
    /// [`FileId`] from it.  Providers that index by path can override this.
    ///
    /// The default implementation uses deterministic [`FileId::generate`];
    /// it does not consult a [`Store`] because the trait does not require one.
    /// Implementors that hold a store should resolve through
    /// `Store::resolve_file_id` to return the canonical FileId.
    fn candidates_for_path(&self, path: &str) -> Result<Vec<FileId>> {
        Ok(vec![FileId::generate(path)])
    }
}

/// Default candidate provider: FTS5 on symbols table + ripgrep fallback.
pub struct DefaultCandidateProvider {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
}

impl DefaultCandidateProvider {
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        Self {
            store,
            project_root,
        }
    }
}

impl CandidateProvider for DefaultCandidateProvider {
    fn candidates_for_symbol(&self, name: &str) -> Result<Vec<FileId>> {
        // 1. Try FTS5 on symbols
        let candidates = self.candidates_from_symbols(name)?;
        if !candidates.is_empty() {
            return Ok(candidates);
        }
        // 2. Fallback: bounded ripgrep
        self.candidates_from_ripgrep(name, None)
    }

    fn candidates_for_path(&self, path: &str) -> Result<Vec<FileId>> {
        match &self.project_root {
            Some(root) => match self.store.resolve_file_id(root, path) {
                Ok(Some(file_id)) => Ok(vec![file_id]),
                _ => self.store.find_file_inventory_by_path(path).map(|row| {
                    row.and_then(|r| file_id_from_inventory_bytes(&r.file_id))
                        .map(|file_id| vec![file_id])
                        .unwrap_or_default()
                }),
            },
            None => Ok(vec![FileId::generate(path)]),
        }
    }
}

impl DefaultCandidateProvider {
    fn candidates_from_symbols(&self, name: &str) -> Result<Vec<FileId>> {
        let symbols = self.store.find_symbols_by_name(name)?;
        let mut seen = std::collections::HashSet::new();
        let mut file_ids = Vec::new();
        for sym in symbols.iter().take(MAX_CANDIDATE_FILES) {
            if seen.insert(sym.file_id) {
                file_ids.push(sym.file_id);
            }
        }
        Ok(file_ids)
    }

    fn candidates_from_ripgrep(&self, name: &str, scope: Option<&str>) -> Result<Vec<FileId>> {
        let project_root = match &self.project_root {
            Some(r) => r.clone(),
            None => return Ok(Vec::new()),
        };

        let mut paths = run_rg_candidate_paths(&project_root, name, scope, true)?;
        if paths.is_empty() {
            paths = run_rg_candidate_paths(&project_root, name, scope, false)?;
        }

        let mut file_ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for line in paths {
            if line.is_empty() {
                continue;
            }
            if let Some(file_id) =
                resolve_or_inventory_candidate(&self.store, &project_root, &line)?
            {
                if seen.insert(file_id) {
                    file_ids.push(file_id);
                }
            }
        }
        Ok(file_ids)
    }
}

fn file_id_from_inventory_bytes(bytes: &[u8]) -> Option<FileId> {
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(FileId::from_bytes(arr))
}

// ---------------------------------------------------------------------------
// LazyStructuralService
// ---------------------------------------------------------------------------

/// Outcome of a single-file structural reindex.
pub(crate) enum ReindexOutcome {
    /// Extraction and DB write completed successfully.
    Built,
    /// Extraction was cancelled (budget exhausted). No DB write occurred.
    Cancelled,
}

/// Outcome of a lazy structural ensure invocation.
#[derive(Debug, Clone)]
pub struct EnsureStructuralResult {
    pub files_built: usize,
    pub files_cached: usize,
    pub budget_exceeded: bool,
    /// FileIds that were actually built (not cached).
    /// Used by delta graph refresh to scope the in-memory graph rebuild.
    pub built_file_ids: Vec<FileId>,
    /// FileIds that were found to already have up-to-date extraction (cached).
    pub cached_file_ids: Vec<FileId>,
    /// Precision reflecting data quality after this lazy operation.
    pub precision: Precision,
    /// Files that are being built by another job (ClaimResult::AlreadyBuilding).
    pub files_pending: usize,
    /// IDs of extraction jobs that are currently in-flight (AlreadyBuilding).
    pub pending_job_ids: Vec<String>,
    /// Candidate files intentionally left for background focus warming.
    pub deferred_file_ids: Vec<FileId>,
}

/// Entry point for query-driven lazy structural extraction.
///
/// Holds a [`CandidateProvider`] for file discovery and a [`Store`] for
/// cache checks and re-extraction.  By default uses [`DefaultCandidateProvider`].
pub struct LazyStructuralService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    pub(crate) candidate_provider: Box<dyn CandidateProvider>,
}

impl LazyStructuralService {
    /// Create a service with the default candidate provider.
    pub fn new(store: Arc<Store>, project_root: Option<PathBuf>) -> Self {
        let provider = DefaultCandidateProvider::new(store.clone(), project_root.clone());
        Self {
            store,
            project_root,
            candidate_provider: Box::new(provider),
        }
    }

    /// Create a service with a custom candidate provider (for testing only).
    #[cfg(test)]
    pub fn with_provider(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        provider: Box<dyn CandidateProvider>,
    ) -> Self {
        Self {
            store,
            project_root,
            candidate_provider: provider,
        }
    }

    /// Ensure the file containing `symbol_name` has full structural facts.
    pub fn ensure_structural_for_symbol(&self, name: &str) -> Result<EnsureStructuralResult> {
        let candidates = self.candidate_provider.candidates_for_symbol(name)?;
        if candidates.is_empty() {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: false,
                built_file_ids: vec![],
                cached_file_ids: vec![],
                precision: Precision::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: vec![],
            });
        }
        self.ensure_structural_for_files(&candidates, None)
    }

    /// Ensure files matching `symbol_name` inside a project-relative scope.
    ///
    /// Scoped search uses this when only the focus inventory exists. It keeps
    /// extraction tied to the user's requested hot area instead of parsing an
    /// arbitrary prefix of a large directory.
    pub fn ensure_structural_for_symbol_in_scope(
        &self,
        name: &str,
        scope: Option<&str>,
    ) -> Result<EnsureStructuralResult> {
        let candidates = self.candidates_for_symbol_in_scope(name, scope)?;
        if candidates.is_empty() {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: false,
                built_file_ids: vec![],
                cached_file_ids: vec![],
                precision: Precision::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: vec![],
            });
        }
        self.ensure_structural_for_files(&candidates, None)
    }

    /// Ensure a bounded number of candidate files matching `symbol_name`.
    ///
    /// This is used by latency-sensitive frontends such as MCP search. Candidate
    /// discovery is already bounded, but parsing every cold candidate on the
    /// first query can still exceed an AI client's tool timeout. Limiting the
    /// synchronous parse count lets the first response return partial results
    /// quickly while still warming the most likely files.
    pub fn ensure_structural_for_symbol_in_scope_limited(
        &self,
        name: &str,
        scope: Option<&str>,
        max_files: usize,
    ) -> Result<EnsureStructuralResult> {
        let mut candidates = self.candidates_for_symbol_in_scope(name, scope)?;
        if candidates.is_empty() || max_files == 0 {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: !candidates.is_empty(),
                built_file_ids: vec![],
                cached_file_ids: vec![],
                precision: Precision::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: candidates,
            });
        }

        let truncated = candidates.len() > max_files;
        let deferred_file_ids = if truncated {
            candidates[max_files..].to_vec()
        } else {
            Vec::new()
        };
        candidates.truncate(max_files);
        let mut result = self.ensure_structural_for_files(&candidates, None)?;
        result.budget_exceeded |= truncated;
        result.deferred_file_ids = deferred_file_ids;
        result.precision = crate::precision::structural_precision(
            result.files_built,
            result.files_cached,
            result.budget_exceeded,
        );
        Ok(result)
    }

    /// Ensure a specific file has full structural facts.
    pub fn ensure_structural_for_file(
        &self,
        file_id: &FileId,
        token: Option<&dyn CancelCheck>,
    ) -> Result<EnsureStructuralResult> {
        self.ensure_structural_for_files(&[*file_id], token)
    }

    /// Ensure structural facts for a file owned by a focus closure. The
    /// closure engine performs scoped resolution and graph materialization,
    /// so this path must not also invoke the repo-wide incremental resolver.
    pub fn ensure_structural_for_file_in_closure(
        &self,
        file_id: &FileId,
        token: Option<&dyn CancelCheck>,
    ) -> Result<EnsureStructuralResult> {
        self.ensure_structural_for_files_impl(&[*file_id], token, false)
    }

    /// Ensure a bounded set of files has structural facts.
    ///
    /// Query frontends use this when the user has narrowed work to a specific
    /// directory. The service still applies its wall-clock budget and cache
    /// checks, so callers can safely pass the files in scope up to their own
    /// policy limit.
    pub fn ensure_structural_for_file_ids(
        &self,
        file_ids: &[FileId],
    ) -> Result<EnsureStructuralResult> {
        self.ensure_structural_for_files(file_ids, None)
    }

    /// Check whether a file already has a complete structural layer.
    pub fn has_structural_layer(&self, file_id: &FileId) -> Result<bool> {
        let file = self.store.get_file(file_id)?;
        let current_hash = file.as_ref().map(|f| &f.content_hash);
        let Some(current_hash) = current_hash else {
            return Ok(false);
        };
        let layer = self
            .store
            .get_file_extraction_state(file_id, layer::STRUCTURAL)?;
        let fresh_complete =
            layer.is_some_and(|(s, hash)| s == status::COMPLETE && hash == *current_hash);
        if !fresh_complete {
            return Ok(false);
        }
        if self
            .store
            .file_has_non_callable_call_reference_sources(file_id)?
        {
            tracing::warn!(
                %file_id,
                "structural layer has stale call ownership; scheduling lazy rebuild"
            );
            return Ok(false);
        }
        Ok(true)
    }

    /// Check whether a file already has a complete resolution_symbols layer
    /// (or better — structural counts as superset).
    pub fn has_resolution_symbols_layer(&self, file_id: &FileId) -> Result<bool> {
        if self.has_structural_layer(file_id).unwrap_or(false) {
            return Ok(true);
        }
        let file_info = match self.store.get_file(file_id)? {
            Some(fi) => fi,
            None => return Ok(false),
        };
        match self
            .store
            .get_file_extraction_state(file_id, layer::RESOLUTION_SYMBOLS)
        {
            Ok(Some((s, hash))) => Ok(s == status::COMPLETE && hash == file_info.content_hash),
            _ => Ok(false),
        }
    }

    /// Ensure a file has at least resolution_symbols layer (not full structural).
    /// Used for import dependencies that only need to serve as resolution targets.
    pub fn ensure_resolution_symbols_for_file(
        &self,
        file_id: &FileId,
    ) -> Result<EnsureStructuralResult> {
        self.ensure_resolution_symbols_for_file_ids(&[*file_id])
    }

    /// Ensure resolution_symbols for multiple files.
    pub fn ensure_resolution_symbols_for_file_ids(
        &self,
        file_ids: &[FileId],
    ) -> Result<EnsureStructuralResult> {
        let start = std::time::Instant::now();
        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            cached_file_ids: vec![],
            precision: Precision::worst(),
            files_pending: 0,
            pending_job_ids: vec![],
            deferred_file_ids: vec![],
        };

        for file_id in file_ids {
            if start.elapsed().as_millis() > LAZY_STRUCTURAL_LOOP_GUARD_MS as u128 {
                result.budget_exceeded = true;
                break;
            }
            if self.has_resolution_symbols_layer(file_id)? {
                result.files_cached += 1;
                result.cached_file_ids.push(*file_id);
                continue;
            }
            match self.reindex_file_resolution_symbols(file_id) {
                Ok(()) => {
                    result.files_built += 1;
                    result.built_file_ids.push(*file_id);
                }
                Err(e) => {
                    tracing::warn!("Lazy resolution_symbols failed for {:?}: {:#}", file_id, e);
                }
            }
        }

        if !result.built_file_ids.is_empty() {
            self.incremental_resolve_and_build(&result.built_file_ids)?;
        }

        result.precision = crate::precision::structural_precision(
            result.files_built,
            result.files_cached,
            result.budget_exceeded,
        );

        Ok(result)
    }

    /// Re-extract a single file with ResolutionSymbols mode.
    fn reindex_file_resolution_symbols(&self, file_id: &FileId) -> Result<()> {
        let file_info = self.file_info_for_lazy(file_id)?;
        let frontend = create_frontend(file_info.language).ok_or_else(|| {
            anyhow::anyhow!("frontend not available for {:?}", file_info.language)
        })?;
        let resolved_path = self.resolve_file_path(&file_info.path);
        // Security: ensure path is within project root.
        if let Some(root) = &self.project_root {
            let canonical_root = root.canonicalize().with_context(|| {
                format!("failed to canonicalize project root {}", root.display())
            })?;
            let canonical_file = resolved_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {}", resolved_path.display()))?;
            anyhow::ensure!(
                canonical_file.starts_with(&canonical_root),
                "path traversal detected: {} is outside project root {}",
                canonical_file.display(),
                canonical_root.display()
            );
        }
        let source = std::fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read {}", resolved_path.display()))?;
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let mut facts = extract_file_with_mode(
            &frontend,
            *file_id,
            std::path::Path::new(&file_info.path),
            &source,
            &content_hash,
            ExtractionMode::ResolutionSymbols,
        )?;

        // Post-extraction: enrich with kernel-specific semantics
        // Linux augmentation for resolution_symbols:
        // EXPORT_SYMBOL → sym.exported=true (persisted via write_symbols) ✅
        // initcall edges, syscall diagnostics → silently dropped
        //   (raw_edges table not written by upsert_resolution_symbols)
        let aug = crate::linux_augment::LinuxAugmenter::augment(&mut facts, &source);
        if aug.symbols_exported > 0 || aug.initcall_edges > 0 || aug.syscall_detected > 0 {
            tracing::info!(
                "Linux augment: {} exports, {} initcall edges, {} syscalls for {}",
                aug.symbols_exported,
                aug.initcall_edges,
                aug.syscall_detected,
                file_info.path,
            );
        }

        // Non-destructive upsert: writes symbols, scopes, and imports
        // without destroying existing structural data or invalidating
        // cross-file resolved references.  Safe when structural layer
        // already exists on this file.
        self.store.upsert_resolution_symbols(file_id, &facts)?;

        tracing::info!(
            "Lazy resolution_symbols: {} ({} symbols)",
            file_info.path,
            facts.symbol_count()
        );
        Ok(())
    }

    // ── Internal ────────────────────────────────────────────────────────

    fn ensure_structural_for_files(
        &self,
        file_ids: &[FileId],
        token: Option<&dyn CancelCheck>,
    ) -> Result<EnsureStructuralResult> {
        self.ensure_structural_for_files_impl(file_ids, token, true)
    }

    fn ensure_structural_for_files_impl(
        &self,
        file_ids: &[FileId],
        token: Option<&dyn CancelCheck>,
        build_global_graph: bool,
    ) -> Result<EnsureStructuralResult> {
        let start = std::time::Instant::now();
        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            cached_file_ids: vec![],
            precision: Precision::worst(),
            files_pending: 0,
            pending_job_ids: vec![],
            deferred_file_ids: vec![],
        };

        for file_id in file_ids {
            if start.elapsed().as_millis() > LAZY_STRUCTURAL_LOOP_GUARD_MS as u128 {
                result.budget_exceeded = true;
                break;
            }
            if self.has_structural_layer(file_id)? {
                result.files_cached += 1;
                result.cached_file_ids.push(*file_id);
                continue;
            }
            match self.reindex_file_structural(file_id, token) {
                Ok(ReindexOutcome::Built) => {
                    result.files_built += 1;
                    result.built_file_ids.push(*file_id);
                }
                Ok(ReindexOutcome::Cancelled) => {
                    result.budget_exceeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!("Lazy structural failed for {:?}: {:#}", file_id, e);
                }
            }
        }

        if build_global_graph && !result.built_file_ids.is_empty() {
            self.incremental_resolve_and_build(&result.built_file_ids)?;
        }

        // Compute precision tier from build results
        result.precision = crate::precision::structural_precision(
            result.files_built,
            result.files_cached,
            result.budget_exceeded,
        );

        Ok(result)
    }

    /// Re-extract a single file with Structural mode.
    ///
    /// Uses `Store::replace_file_facts` to atomically delete old data and
    /// insert new facts in a single transaction, preventing concurrent
    /// readers from seeing the file in a partially-deleted state.
    /// Callers do not need a separate [`FileLock`] for MCP single-threaded
    /// operation; concurrent CLI `atlas index` runs should coordinate
    /// via the store's exclusive lock.
    fn reindex_file_structural(
        &self,
        file_id: &FileId,
        token: Option<&dyn CancelCheck>,
    ) -> Result<ReindexOutcome> {
        let file_info = self.file_info_for_lazy(file_id)?;

        let frontend = create_frontend(file_info.language).ok_or_else(|| {
            anyhow::anyhow!("frontend not available for {:?}", file_info.language)
        })?;
        let resolved_path = self.resolve_file_path(&file_info.path);
        // Security: ensure path is within project root.
        if let Some(root) = &self.project_root {
            let canonical_root = root.canonicalize().with_context(|| {
                format!("failed to canonicalize project root {}", root.display())
            })?;
            let canonical_file = resolved_path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {}", resolved_path.display()))?;
            anyhow::ensure!(
                canonical_file.starts_with(&canonical_root),
                "path traversal detected: {} is outside project root {}",
                canonical_file.display(),
                canonical_root.display()
            );
        }
        let source = std::fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read {}", resolved_path.display()))?;
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        // Soft-reject oversized files: lazy structural extraction is designed
        // for typical source files. Files > 2 MB risk OOM or per-file timeout
        // and should be indexed via `atlas index` instead.
        if source.len() > LAZY_STRUCTURAL_MAX_FILE_BYTES {
            return Err(anyhow::anyhow!(
                "file exceeds lazy structural size limit ({} bytes > {} bytes); use `atlas index` for full indexing",
                source.len(),
                LAZY_STRUCTURAL_MAX_FILE_BYTES
            ));
        }

        // CP5: check cancellation before extraction.
        if let Some(t) = token {
            if t.is_cancelled() {
                return Ok(ReindexOutcome::Cancelled);
            }
        }

        // Extract BEFORE any destructive invalidation — if extraction fails,
        // no destructive operations have been performed.
        let mut facts = if let Some(t) = token {
            extract_file_with_mode_cancellable(
                &frontend,
                *file_id,
                std::path::Path::new(&file_info.path),
                &source,
                &content_hash,
                ExtractionMode::Structural,
                t,
            )?
        } else {
            extract_file_with_mode(
                &frontend,
                *file_id,
                std::path::Path::new(&file_info.path),
                &source,
                &content_hash,
                ExtractionMode::Structural,
            )?
        };

        // Post-extraction: enrich with kernel-specific semantics
        let aug = crate::linux_augment::LinuxAugmenter::augment(&mut facts, &source);
        if aug.symbols_exported > 0 || aug.initcall_edges > 0 || aug.syscall_detected > 0 {
            tracing::info!(
                "Linux augment: {} exports, {} initcall edges, {} syscalls for {}",
                aug.symbols_exported,
                aug.initcall_edges,
                aug.syscall_detected,
                file_info.path,
            );
        }

        // CP6: check cancellation before DB write — most critical checkpoint;
        // prevents completed extraction from writing to DB when budget exhausted.
        if let Some(t) = token {
            if t.is_cancelled() {
                return Ok(ReindexOutcome::Cancelled);
            }
        }

        // Invalidate cross-file references, delete outgoing edges, and
        // atomically replace file facts — all in a single transaction so
        // a partial failure cannot leave references in a destroyed state.
        self.store
            .replace_file_facts_with_invalidation(file_id, &facts)?;

        tracing::info!(
            "Lazy structural: {} ({} symbols, {} refs)",
            file_info.path,
            facts.symbol_count(),
            facts.reference_count()
        );
        Ok(ReindexOutcome::Built)
    }

    fn incremental_resolve_and_build(&self, file_ids: &[FileId]) -> Result<()> {
        let mut resolver = resolution::ReferenceResolver::new(self.store.clone());
        let (resolved_refs, _stats) = resolver.resolve_for_files(file_ids)?;
        let builder = graph::GraphBuilder::new(self.store.clone());
        let _build_stats = builder.build_for_files(&resolved_refs, file_ids);
        Ok(())
    }

    fn resolve_file_path(&self, relative: &str) -> PathBuf {
        let path = PathBuf::from(relative);
        if path.is_absolute() {
            return path;
        }
        match &self.project_root {
            Some(root) => root.join(relative),
            None => path,
        }
    }

    fn project_relative_path(&self, raw_path: &str) -> Result<String> {
        let normalized = raw_path.replace('\\', "/");
        let path = std::path::Path::new(&normalized);
        if !path.is_absolute() {
            return Ok(normalized
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string());
        }

        let Some(root) = &self.project_root else {
            return Ok(normalized);
        };
        if let Ok(rel) = path.strip_prefix(root) {
            return Ok(rel.to_string_lossy().replace('\\', "/"));
        }

        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize project root {}", root.display()))?;
        let canonical_path = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        let rel = canonical_path
            .strip_prefix(&canonical_root)
            .with_context(|| {
                format!(
                    "lazy candidate path {} is outside project root {}",
                    path.display(),
                    root.display()
                )
            })?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    }

    fn file_info_for_lazy(&self, file_id: &FileId) -> Result<FileInfo> {
        if let Some(file_info) = self.store.get_file(file_id)? {
            let path = self.project_relative_path(&file_info.path)?;
            return Ok(FileInfo { path, ..file_info });
        }

        let row = self
            .store
            .find_file_inventory_by_id(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found in files or inventory: {file_id:?}"))?;
        let language = Language::from_str(&row.language)
            .or_else(|| Language::from_path(std::path::Path::new(&row.path)))
            .unwrap_or_default();
        let path = self.project_relative_path(&row.path)?;
        Ok(FileInfo {
            file_id: *file_id,
            path,
            language,
            content_hash: row.content_hash.unwrap_or_default(),
            status: ParseStatus::Success,
        })
    }

    fn candidates_for_symbol_in_scope(
        &self,
        name: &str,
        scope: Option<&str>,
    ) -> Result<Vec<FileId>> {
        let normalized_scope = scope
            .map(|s| {
                s.trim()
                    .trim_start_matches("./")
                    .trim_start_matches('/')
                    .trim_end_matches('/')
                    .replace('\\', "/")
            })
            .unwrap_or_default();
        if normalized_scope.is_empty() || normalized_scope == "." {
            return self.candidate_provider.candidates_for_symbol(name);
        }

        let project_root = match &self.project_root {
            Some(root) => root,
            None => return Ok(Vec::new()),
        };

        let mut paths =
            run_rg_candidate_paths(project_root, name, Some(normalized_scope.as_str()), true)?;
        if paths.is_empty() {
            paths =
                run_rg_candidate_paths(project_root, name, Some(normalized_scope.as_str()), false)?;
        }

        let mut seen = std::collections::HashSet::new();
        let mut file_ids = Vec::new();
        for rel in paths {
            if rel.is_empty() {
                continue;
            }
            let file_id = resolve_or_inventory_candidate(&self.store, project_root, &rel)?;
            if let Some(file_id) = file_id {
                if seen.insert(file_id) {
                    file_ids.push(file_id);
                }
            }
        }
        Ok(file_ids)
    }
}

fn resolve_or_inventory_candidate(
    store: &Store,
    project_root: &Path,
    rel_path: &str,
) -> Result<Option<FileId>> {
    if let Some(file_id) = store.resolve_file_id(project_root, rel_path)? {
        return Ok(Some(file_id));
    }
    if let Some(row) = store.find_file_inventory_by_path(rel_path)? {
        return Ok(file_id_from_inventory_bytes(&row.file_id));
    }
    insert_inventory_candidate(store, project_root, rel_path)
}

fn insert_inventory_candidate(
    store: &Store,
    project_root: &Path,
    rel_path: &str,
) -> Result<Option<FileId>> {
    if rel_path.is_empty() || Path::new(rel_path).is_absolute() {
        return Ok(None);
    }

    let abs_path = project_root.join(rel_path);
    let canonical_root = project_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize project root {}",
            project_root.display()
        )
    })?;
    let canonical_file = match abs_path.canonicalize() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !canonical_file.starts_with(&canonical_root) {
        return Ok(None);
    }

    let metadata = match std::fs::metadata(&canonical_file) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(None),
    };
    let language = Language::from_path(Path::new(rel_path)).unwrap_or_else(|| Language::default());
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

    let file_id = FileId::generate(rel_path);
    store.insert_file_inventory(
        &file_id,
        rel_path,
        language.as_str(),
        mtime,
        metadata.len() as i64,
        inode,
        dev,
    )?;
    Ok(Some(file_id))
}

fn run_rg_candidate_paths(
    project_root: &Path,
    name: &str,
    scope: Option<&str>,
    definition_like: bool,
) -> Result<Vec<String>> {
    let mut cmd = std::process::Command::new("rg");
    cmd.args(["--files-with-matches", "--no-heading", "--max-count=1"]);
    for glob in [
        "*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.rs", "*.go", "*.java", "*.py", "*.ts",
        "*.tsx", "*.js", "*.jsx", "*.php", "*.rb", "*.kt", "*.kts",
    ] {
        cmd.arg("--type-add").arg(format!("atlassrc:{glob}"));
    }
    cmd.args(["--type", "atlassrc"]);
    cmd.args(["--glob", "!.git/**"]);
    if project_root.join(".atlasignore").exists() {
        cmd.args(["--ignore-file", ".atlasignore"]);
    }

    let pattern;
    if definition_like {
        pattern = format!(r"\b{}\s*\(", regex::escape(name));
        cmd.arg(&pattern);
    } else {
        cmd.args(["--word-regexp", "--fixed-strings"]);
        cmd.arg(name);
    }
    if let Some(scope) = scope.filter(|s| !s.is_empty() && *s != ".") {
        cmd.arg(scope);
    }
    cmd.current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return Ok(Vec::new()),
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return Ok(Vec::new()),
    };

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_millis(RG_CANDIDATE_TIMEOUT_MS);
    let mut paths = Vec::new();
    loop {
        if paths.len() >= MAX_CANDIDATE_FILES {
            let _ = child.kill();
            break;
        }
        if child.try_wait()?.is_some() {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            break;
        }
        let wait = std::cmp::min(deadline - now, Duration::from_millis(25));
        match rx.recv_timeout(wait) {
            Ok(Ok(line)) => {
                let line = line.trim();
                if !line.is_empty() {
                    paths.push(line.to_string());
                }
            }
            Ok(Err(_)) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if child.try_wait()?.is_some() {
                    break;
                }
            }
        }
    }

    let _ = child.wait();
    drain_rg_candidate_lines(&rx, &mut paths);
    paths.truncate(MAX_CANDIDATE_FILES);
    Ok(paths)
}

fn drain_rg_candidate_lines(
    rx: &std::sync::mpsc::Receiver<std::io::Result<String>>,
    paths: &mut Vec<String>,
) {
    let drain_deadline = Instant::now() + Duration::from_millis(100);
    while paths.len() < MAX_CANDIDATE_FILES {
        match rx.try_recv() {
            Ok(Ok(line)) => {
                let line = line.trim();
                if !line.is_empty() {
                    paths.push(line.to_string());
                }
            }
            Ok(Err(_)) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if Instant::now() >= drain_deadline {
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(Ok(line)) => {
                        let line = line.trim();
                        if !line.is_empty() {
                            paths.push(line.to_string());
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Self-healing structural rebuild (free function for dependency inversion)
// ---------------------------------------------------------------------------

/// Rebuild the structural layer for a single file.
///
/// This is a free function (not a method) so it can be injected as a callback
/// into `LazyDataflowService` for transparent self-healing without creating
/// a circular dependency between the `lazy` and `atlas-engine` crates.
pub(crate) fn rebuild_structural_for_file(
    store: &Store,
    project_root: Option<&std::path::Path>,
    file_id: &FileId,
) -> anyhow::Result<()> {
    // 1. Get file info from store (or inventory fallback)
    let file_info = match store.get_file(file_id)? {
        Some(fi) => fi,
        None => {
            let row = store.find_file_inventory_by_id(file_id)?.ok_or_else(|| {
                anyhow::anyhow!("file not found in files or inventory: {file_id:?}")
            })?;
            let language = types::Language::from_str(&row.language)
                .or_else(|| types::Language::from_path(std::path::Path::new(&row.path)))
                .unwrap_or_default();
            types::FileInfo {
                file_id: *file_id,
                path: row.path,
                language,
                content_hash: row.content_hash.unwrap_or_default(),
                status: types::ParseStatus::Success,
            }
        }
    };

    // 2. Create frontend
    let frontend = create_frontend(file_info.language)
        .ok_or_else(|| anyhow::anyhow!("frontend not available for {:?}", file_info.language))?;

    // 3. Resolve path
    let resolved_path = if let Some(root) = project_root {
        root.join(&file_info.path)
    } else {
        std::path::PathBuf::from(&file_info.path)
    };

    // 4. Security check (path traversal)
    if let Some(root) = project_root {
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize project root {}", root.display()))?;
        let canonical_file = resolved_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", resolved_path.display()))?;
        anyhow::ensure!(
            canonical_file.starts_with(&canonical_root),
            "path traversal detected: {} is outside project root {}",
            canonical_file.display(),
            canonical_root.display()
        );
    }

    // 5. Read source
    let source = std::fs::read_to_string(&resolved_path)
        .with_context(|| format!("failed to read {}", resolved_path.display()))?;

    // Soft-reject oversized files
    if source.len() > LAZY_STRUCTURAL_MAX_FILE_BYTES {
        return Err(anyhow::anyhow!(
            "file exceeds lazy structural size limit ({} bytes > {} bytes); use `atlas index` for full indexing",
            source.len(),
            LAZY_STRUCTURAL_MAX_FILE_BYTES
        ));
    }

    // 6. Compute hash
    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    // 7. Extract structural layer
    let mut facts = extract_file_with_mode(
        &frontend,
        *file_id,
        std::path::Path::new(&file_info.path),
        &source,
        &content_hash,
        ExtractionMode::Structural,
    )?;

    // Post-extraction: enrich with kernel-specific semantics
    let aug = crate::linux_augment::LinuxAugmenter::augment(&mut facts, &source);
    if aug.symbols_exported > 0 || aug.initcall_edges > 0 || aug.syscall_detected > 0 {
        tracing::info!(
            "Linux augment (rebuild): {} exports, {} initcall edges, {} syscalls for {}",
            aug.symbols_exported,
            aug.initcall_edges,
            aug.syscall_detected,
            file_info.path,
        );
    }

    // 8. Write to store (atomic replacement)
    store.replace_file_facts_with_invalidation(file_id, &facts)?;

    tracing::info!(file=%file_info.path, "self-healing structural rebuild complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use types::structs::CapabilityMask;

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    #[test]
    fn test_has_structural_layer_empty_db() {
        let store = test_store();
        let svc = LazyStructuralService::new(store, None);
        let fid = FileId::generate("test.rs");
        assert!(!svc.has_structural_layer(&fid).unwrap());
    }

    #[test]
    fn test_candidate_provider_default_path() {
        // Default path provider generates a single FileId from the path string
        let store = test_store();
        let provider = DefaultCandidateProvider::new(store, None);
        let candidates = provider.candidates_for_path("src/main.rs").unwrap();
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_has_resolution_symbols_layer_empty_db() {
        let store = test_store();
        let svc = LazyStructuralService::new(store, None);
        let fid = FileId::generate("test.ts");
        assert!(!svc.has_resolution_symbols_layer(&fid).unwrap());
    }

    #[test]
    fn test_has_resolution_symbols_layer_when_structural_exists() {
        use types::Language;

        let store = test_store();
        let svc = LazyStructuralService::new(store.clone(), None);

        // Manually insert a file with structural layer
        let fid = FileId::generate("test.ts");
        let file_info = types::structs::FileInfo {
            file_id: fid,
            path: "test.ts".to_string(),
            language: Language::TypeScript,
            content_hash: "abc123".to_string(),
            status: types::enums::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();
        store
            .upsert_file_extraction_state(
                &fid,
                layer::STRUCTURAL,
                "abc123",
                status::COMPLETE,
                CapabilityMask::default(),
            )
            .unwrap();

        // When structural layer exists, has_resolution_symbols_layer should return true
        assert!(svc.has_resolution_symbols_layer(&fid).unwrap());
    }

    #[test]
    fn test_has_structural_layer_rejects_non_callable_call_owner() {
        use types::{
            FileFacts, ReferenceId, ReferenceKind, ReferenceUse, SymbolDef, SymbolId, SymbolKind,
            TextRange,
        };

        let store = test_store();
        let svc = LazyStructuralService::new(store.clone(), None);
        let fid = FileId::generate("net/ipv4/tcp_ipv4.c");
        let hash = "abc123";
        let enum_range = TextRange {
            start_byte: 20,
            end_byte: 30,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 10,
        };
        let function_range = TextRange {
            start_byte: 0,
            end_byte: 100,
            ..Default::default()
        };
        let call_range = TextRange {
            start_byte: 40,
            end_byte: 50,
            ..Default::default()
        };
        let enum_id = SymbolId::generate(
            &fid,
            Language::C.as_str(),
            "tcp_tw_status",
            SymbolKind::Enum.as_str(),
            None,
        );
        let enum_symbol = SymbolDef {
            id: enum_id,
            kind: SymbolKind::Enum,
            name: "tcp_tw_status".to_string(),
            qualified_name: "tcp_tw_status".to_string(),
            symbol_path: vec!["tcp_tw_status".to_string()],
            file_id: fid,
            language: Language::C,
            range: enum_range,
            name_range: enum_range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: layer::STRUCTURAL.to_string(),
        };
        let function_id = SymbolId::generate(
            &fid,
            Language::C.as_str(),
            "tcp_v4_rcv",
            SymbolKind::Function.as_str(),
            None,
        );
        let function_symbol = SymbolDef {
            id: function_id,
            kind: SymbolKind::Function,
            name: "tcp_v4_rcv".to_string(),
            qualified_name: "tcp_v4_rcv".to_string(),
            symbol_path: vec!["tcp_v4_rcv".to_string()],
            file_id: fid,
            language: Language::C,
            range: function_range,
            name_range: function_range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: layer::STRUCTURAL.to_string(),
        };
        let call_text = "tcp_filter".to_string();
        let call_ref = ReferenceUse {
            id: ReferenceId::generate(
                &fid,
                Some(&enum_id),
                call_range.start_byte,
                call_range.end_byte,
                &call_text,
                ReferenceKind::Call,
            ),
            file_id: fid,
            source_symbol: Some(enum_id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: call_text.clone(),
            name: call_text,
            receiver: None,
            arity: None,
            range: call_range,
            binding_id: None,
            resolved: None,
        };

        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id: fid,
                    path: "net/ipv4/tcp_ipv4.c".to_string(),
                    language: Language::C,
                    content_hash: hash.to_string(),
                    status: ParseStatus::Success,
                },
                symbols: vec![enum_symbol, function_symbol],
                references: vec![call_ref],
                ..Default::default()
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &fid,
                layer::STRUCTURAL,
                hash,
                status::COMPLETE,
                CapabilityMask::default(),
            )
            .unwrap();

        assert!(!svc.has_structural_layer(&fid).unwrap());
    }

    #[test]
    fn default_provider_resolves_path_through_store() {
        use types::Language;

        let store = test_store();
        // project_root is only used for absolute-path fallback in resolve_file_id;
        // a dummy path suffices since the test uses exact path match.
        let root = std::path::Path::new(".").to_path_buf();

        // Insert a file into the store with a known path
        let fid = FileId::generate("lib/helper.rs");
        let file_info = types::structs::FileInfo {
            file_id: fid,
            path: "lib/helper.rs".to_string(),
            language: Language::Rust,
            content_hash: "hash1".to_string(),
            status: types::enums::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let provider = DefaultCandidateProvider::new(store, Some(root));
        let candidates = provider.candidates_for_path("lib/helper.rs").unwrap();
        assert_eq!(candidates.len(), 1, "should find exactly one candidate");
        assert_eq!(
            candidates[0], fid,
            "should return the store's canonical FileId, not a re-generated one"
        );
    }

    #[test]
    fn structural_ensure_does_not_treat_inventory_hit_as_extracted() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "run.py";
        let source = "def helper():\n    pass\n\n\ndef main():\n    helper()\n";
        let full_path = root.path().join(path);
        std::fs::write(&full_path, source).unwrap();

        let fid = FileId::generate(path);
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let metadata = std::fs::metadata(&full_path).unwrap();
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

        store
            .insert_file_inventory(
                &fid,
                path,
                Language::Python.as_str(),
                mtime,
                metadata.len() as i64,
                inode,
                dev,
            )
            .unwrap();
        store.set_file_fingerprint(&fid, &content_hash).unwrap();
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.to_string(),
                language: Language::Python,
                content_hash,
                status: ParseStatus::Success,
            })
            .unwrap();

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        assert!(!svc.has_structural_layer(&fid).unwrap());

        let result = svc.ensure_structural_for_file(&fid, None).unwrap();

        assert_eq!(result.files_built, 1);
        assert!(svc.has_structural_layer(&fid).unwrap());
        let refs = store.find_references_by_file(&fid).unwrap();
        assert!(
            refs.iter().any(|r| r.name == "helper"),
            "structural ensure must parse references when no structural extraction_state exists"
        );
    }

    // ── ripgrep candidate provider tests ──────────────────────────────────

    /// Check whether `rg` is available on this system.
    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Counter for unique temp directories across test invocations.
    /// Avoids collisions when tests run in the same process.
    fn next_test_counter() -> usize {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// Helper: create a temp project dir, insert files into the store,
    /// and return (store, project_root, provider).
    fn setup_ripgrep_test(
        files: &[(&str, &str)], // (path, content)
        atlasignore: Option<&str>,
    ) -> (Arc<Store>, PathBuf, DefaultCandidateProvider) {
        let store = test_store();
        let root = std::env::temp_dir().join(format!(
            "atlas_rg_test_{}_{}",
            std::process::id(),
            next_test_counter(),
        ));
        std::fs::create_dir_all(&root).unwrap();

        if let Some(content) = atlasignore {
            std::fs::write(root.join(".atlasignore"), content).unwrap();
        }

        for (path, content) in files {
            let full = root.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();

            // Insert into store so resolve_file_id works
            let fid = FileId::generate(path);
            let file_info = types::structs::FileInfo {
                file_id: fid,
                path: path.to_string(),
                language: types::Language::Rust,
                content_hash: "test_hash".to_string(),
                status: types::enums::ParseStatus::Success,
            };
            store.upsert_file(&file_info).unwrap();
        }

        let provider = DefaultCandidateProvider::new(store.clone(), Some(root.clone()));
        (store, root, provider)
    }

    /// Clean up a temp project dir.
    fn cleanup_ripgrep_test(root: &PathBuf) {
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn candidates_from_ripgrep_respects_atlasignore() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let (_store, root, provider) = setup_ripgrep_test(
            &[
                ("src/lib.rs", "somecontent"),
                ("src/generated.rs", "somecontent"),
                ("build/output.rs", "somecontent"),
            ],
            Some("generated.rs\nbuild/"),
        );

        // "somecontent" exists in all three files.
        // .atlasignore excludes generated.rs and build/
        let candidates = provider.candidates_for_symbol("somecontent").unwrap();
        cleanup_ripgrep_test(&root);

        let expected_included = FileId::generate("src/lib.rs");
        let expected_excluded_generated = FileId::generate("src/generated.rs");
        let expected_excluded_build = FileId::generate("build/output.rs");
        assert!(
            candidates.contains(&expected_included),
            "src/lib.rs should be a candidate (not excluded), got: {candidates:?}"
        );
        assert!(
            !candidates.contains(&expected_excluded_generated),
            "src/generated.rs should be excluded by .atlasignore, got: {candidates:?}"
        );
        assert!(
            !candidates.contains(&expected_excluded_build),
            "build/output.rs should be excluded by .atlasignore, got: {candidates:?}"
        );
    }

    #[test]
    fn candidates_from_ripgrep_works_without_atlasignore() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let (_store, root, provider) = setup_ripgrep_test(
            &[
                ("src/main.rs", "myfunction"),
                ("tests/test.rs", "myfunction"),
            ],
            None, // no .atlasignore
        );

        let candidates = provider.candidates_for_symbol("myfunction").unwrap();
        cleanup_ripgrep_test(&root);

        assert!(
            !candidates.is_empty(),
            "should return results when .atlasignore does not exist"
        );
    }

    #[test]
    fn candidates_from_ripgrep_returns_store_file_ids() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let (store, root, provider) =
            setup_ripgrep_test(&[("src/unique_name.rs", "uniqueterm")], None);

        let candidates = provider.candidates_for_symbol("uniqueterm").unwrap();
        cleanup_ripgrep_test(&root);

        assert_eq!(
            candidates.len(),
            1,
            "expected exactly one candidate, got: {candidates:?}"
        );

        // The returned FileId should match what the store resolves
        let resolved = store
            .resolve_file_id(&root, "src/unique_name.rs")
            .unwrap()
            .expect("store should resolve the file");
        assert_eq!(
            candidates[0], resolved,
            "ripgrep result FileId should match store.resolve_file_id"
        );
    }

    #[test]
    fn run_rg_candidate_paths_stops_at_candidate_limit() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "atlas_rg_limit_test_{}_{}",
            std::process::id(),
            next_test_counter(),
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        for i in 0..25 {
            std::fs::write(
                root.join(format!("src/file_{i}.rs")),
                "fn sharedterm() {}\n",
            )
            .unwrap();
        }

        let paths = run_rg_candidate_paths(&root, "sharedterm", None, true).unwrap();
        cleanup_ripgrep_test(&root);

        assert_eq!(
            paths.len(),
            MAX_CANDIDATE_FILES,
            "candidate discovery must stop at the lazy structural limit"
        );
    }

    #[test]
    fn run_rg_candidate_paths_drains_fast_process_output() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "atlas_rg_fast_drain_test_{}_{}",
            std::process::id(),
            next_test_counter(),
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/fast.c"),
            "static void fast_symbol(void) {}\n",
        )
        .unwrap();

        let paths = run_rg_candidate_paths(&root, "fast_symbol", None, true).unwrap();
        cleanup_ripgrep_test(&root);

        assert!(
            paths.iter().any(|path| path == "src/fast.c"),
            "candidate discovery should drain stdout even when rg exits quickly: {paths:?}"
        );
    }

    #[test]
    fn candidates_from_ripgrep_inventories_unindexed_hits() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let store = test_store();
        let root = std::env::temp_dir().join(format!(
            "atlas_rg_inventory_test_{}_{}",
            std::process::id(),
            next_test_counter(),
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn late_inventory() {}\n").unwrap();

        let provider = DefaultCandidateProvider::new(store.clone(), Some(root.clone()));
        let candidates = provider.candidates_for_symbol("late_inventory").unwrap();
        cleanup_ripgrep_test(&root);

        let expected = FileId::generate("src/lib.rs");
        assert_eq!(candidates, vec![expected]);
        assert!(
            store
                .find_file_inventory_by_path("src/lib.rs")
                .unwrap()
                .is_some(),
            "unindexed rg hit should be inserted into file_inventory"
        );
    }
}
