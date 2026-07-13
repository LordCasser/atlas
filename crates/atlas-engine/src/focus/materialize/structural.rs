//! Focus structural materialize — on-demand structural extraction.
//!
//! Part of the **Focus** query-time solution (not a separate product). When a
//! query needs symbols beyond the manifest layer, this service triggers full
//! structural extraction for candidate files, then incremental resolution and
//! graph edges — same `ExtractionMode::Structural` pipeline as Index.
//!
//! # Components
//!
//! ```text
//! FocusRuntime / Engine (Focus materialize)
//!     │
//!     v
//! LazyStructuralService  (implementation type name; Focus-owned)
//!     ├─ CandidateProvider  (pluggable: FTS5 + ripgrep by default)
//!     └─ StructuralLoader   (re-extract + re-resolve + rebuild edges)
//! ```

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use db::{ClaimResult, Store};
use extraction::{CancelCheck, ExtractionMode, create_frontend, extract_file_with_mode};
use types::ids::FileId;
use types::structs::AnswerQuality;
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
    /// Find files that likely contain a definition of `name`.
    fn candidates_for_symbol(&self, name: &str) -> Result<Vec<FileId>>;

    /// Find files that may reference `name`.
    fn candidates_for_references(&self, name: &str) -> Result<Vec<FileId>>;

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

    fn candidates_for_references(&self, name: &str) -> Result<Vec<FileId>> {
        self.candidates_from_ripgrep(name, None)
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
    /// AnswerQuality reflecting data quality after this lazy operation.
    pub quality: AnswerQuality,
    /// Files that are being built by another job (ClaimResult::AlreadyBuilding).
    pub files_pending: usize,
    /// IDs of extraction jobs that are currently in-flight (AlreadyBuilding).
    pub pending_job_ids: Vec<String>,
    /// Candidate files intentionally left for background focus warming.
    pub deferred_file_ids: Vec<FileId>,
    /// Files that could not be materialized, with a stable diagnostic.
    pub failed_files: Vec<(FileId, String)>,
}

/// Focus-owned on-demand structural materialization.
///
/// Holds a [`CandidateProvider`] for file discovery and a [`Store`] for
/// cache checks and re-extraction.  By default uses [`DefaultCandidateProvider`].
pub struct LazyStructuralService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    pub(crate) candidate_provider: Box<dyn CandidateProvider>,
}

impl Clone for LazyStructuralService {
    /// Clone shares store/root; rebuilds the default candidate provider.
    /// Custom test providers are not preserved (scheduler/thread clones use defaults).
    fn clone(&self) -> Self {
        Self::new(self.store.clone(), self.project_root.clone())
    }
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

    /// Underlying store (tests / wiring audits).
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Project root for path resolution (tests / diagnostics).
    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    pub(crate) fn candidate_files_for_symbol(&self, name: &str) -> Result<Vec<FileId>> {
        self.candidate_provider.candidates_for_symbol(name)
    }

    pub(crate) fn candidate_files_referencing(&self, name: &str) -> Result<Vec<FileId>> {
        self.candidate_provider.candidates_for_references(name)
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
                quality: AnswerQuality::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: vec![],
                failed_files: vec![],
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
                quality: AnswerQuality::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: vec![],
                failed_files: vec![],
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
        let candidates = self.candidates_for_symbol_in_scope(name, scope)?;
        if candidates.is_empty() || max_files == 0 {
            return Ok(EnsureStructuralResult {
                files_built: 0,
                files_cached: 0,
                budget_exceeded: !candidates.is_empty(),
                built_file_ids: vec![],
                cached_file_ids: vec![],
                quality: AnswerQuality::worst(),
                files_pending: 0,
                pending_job_ids: vec![],
                deferred_file_ids: candidates,
                failed_files: vec![],
            });
        }

        let mut cached_file_ids = Vec::new();
        let mut sync_file_ids = Vec::new();
        let mut deferred_file_ids = Vec::new();
        for candidate in candidates {
            if self.has_structural_layer(&candidate).unwrap_or(false) {
                cached_file_ids.push(candidate);
            } else if sync_file_ids.len() < max_files {
                sync_file_ids.push(candidate);
            } else {
                deferred_file_ids.push(candidate);
            }
        }

        let truncated = !deferred_file_ids.is_empty();
        let mut result = self.ensure_structural_for_files(&sync_file_ids, None)?;
        for file_id in cached_file_ids {
            if !result.cached_file_ids.contains(&file_id) {
                result.files_cached += 1;
                result.cached_file_ids.push(file_id);
            }
        }
        result.budget_exceeded |= truncated;
        result.deferred_file_ids = deferred_file_ids;
        result.quality = crate::precision::structural_precision(
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
        let Some(file) = self.store.get_file(file_id)? else {
            return Ok(false);
        };
        let current_hash = &file.content_hash;
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
        if !self.has_complete_type_ranges(&file)? {
            tracing::warn!(
                %file_id,
                "structural layer has stale type ranges; scheduling lazy rebuild"
            );
            return Ok(false);
        }
        Ok(true)
    }

    fn has_complete_type_ranges(&self, file: &FileInfo) -> Result<bool> {
        let Some(project_root) = &self.project_root else {
            return Ok(true);
        };

        let type_symbols: Vec<_> = self
            .store
            .find_symbols_by_file(&file.file_id)?
            .into_iter()
            .filter(|symbol| {
                matches!(
                    symbol.kind,
                    types::SymbolKind::Class
                        | types::SymbolKind::Struct
                        | types::SymbolKind::Interface
                        | types::SymbolKind::Trait
                        | types::SymbolKind::Enum
                ) && symbol.range.start_line >= symbol.range.end_line
            })
            .collect();
        if type_symbols.is_empty() {
            return Ok(true);
        }
        if type_symbols
            .iter()
            .any(|symbol| symbol.range.start_line > symbol.range.end_line)
        {
            return Ok(false);
        }

        let relative_path = Path::new(&file.path);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Ok(false);
        }
        let source = std::fs::read_to_string(project_root.join(relative_path))
            .with_context(|| format!("failed to validate type ranges for {}", file.path))?;
        let lines: Vec<_> = source.lines().collect();

        for symbol in type_symbols {
            let Some(line) = lines.get(symbol.range.start_line as usize) else {
                return Ok(false);
            };
            let open_count = line.bytes().filter(|byte| *byte == b'{').count();
            let close_count = line.bytes().filter(|byte| *byte == b'}').count();
            if open_count > close_count {
                return Ok(false);
            }
            if open_count == 0 && !line.trim_end().ends_with(';') {
                let next_code_line = lines
                    .iter()
                    .skip(symbol.range.start_line as usize + 1)
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty());
                if next_code_line.is_some_and(|line| line.starts_with('{')) {
                    return Ok(false);
                }
            }
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
        self.ensure_resolution_symbols_for_file_ids_impl(file_ids, true)
    }

    /// Focus-owned variant: materialize dependency symbols without mutating
    /// the repository-wide resolved graph. Closure resolution runs afterward.
    pub(crate) fn ensure_resolution_symbols_for_file_ids_in_closure(
        &self,
        file_ids: &[FileId],
    ) -> Result<EnsureStructuralResult> {
        self.ensure_resolution_symbols_for_file_ids_impl(file_ids, false)
    }

    fn ensure_resolution_symbols_for_file_ids_impl(
        &self,
        file_ids: &[FileId],
        build_global_graph: bool,
    ) -> Result<EnsureStructuralResult> {
        let start = std::time::Instant::now();
        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            cached_file_ids: vec![],
            quality: AnswerQuality::worst(),
            files_pending: 0,
            pending_job_ids: vec![],
            deferred_file_ids: vec![],
            failed_files: vec![],
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
                    let reason = format!("{e:#}");
                    tracing::warn!("Lazy resolution_symbols failed for {:?}: {reason}", file_id);
                    result.failed_files.push((*file_id, reason));
                }
            }
        }

        if build_global_graph && !result.built_file_ids.is_empty() {
            self.incremental_resolve_and_build(&result.built_file_ids)?;
        }

        result.quality = crate::precision::structural_precision(
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

        // Post-extract hooks (EXPORT_SYMBOL etc.) run inside extract_file_with_mode.
        // ResolutionSymbols persistence: exported flag on symbols is written;
        // initcall edges / syscall diagnostics are not (no raw_edges write).
        let facts = extract_file_with_mode(
            &frontend,
            *file_id,
            std::path::Path::new(&file_info.path),
            &source,
            &content_hash,
            ExtractionMode::ResolutionSymbols,
            &(),
        )?;

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
        // Cross-process: reject if CLI holds exclusive FileLock (no wait).
        filesync::FileLock::reject_if_held_by_other(self.store.as_ref())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let start = std::time::Instant::now();
        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            cached_file_ids: vec![],
            quality: AnswerQuality::worst(),
            files_pending: 0,
            pending_job_ids: vec![],
            deferred_file_ids: vec![],
            failed_files: vec![],
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
            if self.store.get_file(file_id)?.is_none()
                && self.store.find_file_inventory_by_id(file_id)?.is_none()
            {
                result.failed_files.push((
                    *file_id,
                    format!("file not found in files or inventory: {file_id:?}"),
                ));
                continue;
            }
            if self.store.get_file(file_id)?.is_none() {
                let file_info = self.file_info_for_lazy(file_id)?;
                self.store.upsert_file(&file_info)?;
            }
            let job_id = match self.store.claim_file_extraction_job(
                file_id,
                layer::STRUCTURAL,
                None,
                None,
                None,
            )? {
                ClaimResult::Claimed { job_id } => job_id,
                ClaimResult::AlreadyBuilding { job_id } => {
                    result.files_pending += 1;
                    result.pending_job_ids.push(job_id);
                    continue;
                }
            };
            match self.reindex_file_structural(file_id, token) {
                Ok(ReindexOutcome::Built) => {
                    self.store.complete_extraction_job(&job_id)?;
                    result.files_built += 1;
                    result.built_file_ids.push(*file_id);
                }
                Ok(ReindexOutcome::Cancelled) => {
                    self.store
                        .fail_extraction_job(&job_id, "structural extraction budget exceeded")?;
                    result.budget_exceeded = true;
                    break;
                }
                Err(e) => {
                    let reason = format!("{e:#}");
                    self.store.fail_extraction_job(&job_id, &reason)?;
                    tracing::warn!("Lazy structural failed for {:?}: {reason}", file_id);
                    result.failed_files.push((*file_id, reason));
                }
            }
        }

        if build_global_graph && !result.built_file_ids.is_empty() {
            self.incremental_resolve_and_build(&result.built_file_ids)?;
        }

        // Compute precision tier from build results
        result.quality = crate::precision::structural_precision(
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
        // Post-extract hooks run inside extract_file_with_mode (shared with index).
        let facts = if let Some(t) = token {
            extract_file_with_mode(
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
                &(),
            )?
        };

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
    let language = Language::from_path(Path::new(rel_path)).unwrap_or_default();
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
        "*.tsx", "*.js", "*.jsx", "*.ets", "*.sts", "*.cs", "*.php", "*.rb", "*.kt", "*.kts",
        "*.cj",
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
pub fn rebuild_structural_for_file(
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

    // 7. Extract structural layer (post-extract hooks run inside extract_file_with_mode)
    let facts = extract_file_with_mode(
        &frontend,
        *file_id,
        std::path::Path::new(&file_info.path),
        &source,
        &content_hash,
        ExtractionMode::Structural,
        &(),
    )?;

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
    use types::structs::FactCoverage;

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
                FactCoverage::default(),
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
                FactCoverage::default(),
            )
            .unwrap();

        assert!(!svc.has_structural_layer(&fid).unwrap());
    }

    #[test]
    fn stale_multiline_c_type_range_forces_structural_rebuild() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "stale.c";
        let source = "struct stale {\n    int value;\n};\n";
        std::fs::write(root.path().join(path), source).unwrap();

        let fid = FileId::generate(path);
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let frontend = create_frontend(Language::C).unwrap();
        let mut facts = extract_file_with_mode(
            &frontend,
            fid,
            Path::new(path),
            source,
            &hash,
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let stale = facts
            .symbols
            .iter_mut()
            .find(|symbol| symbol.name == "stale")
            .unwrap();
        stale.range.end_line = stale.range.start_line;
        stale.range.end_byte = source.find('\n').unwrap() as u32;
        store.insert_file_facts(&facts).unwrap();
        store
            .upsert_file_extraction_state(
                &fid,
                layer::STRUCTURAL,
                &hash,
                status::COMPLETE,
                FactCoverage::default(),
            )
            .unwrap();

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        assert!(!svc.has_structural_layer(&fid).unwrap());

        let result = svc.ensure_structural_for_file(&fid, None).unwrap();
        assert_eq!(result.files_built, 1);
        let rebuilt = store
            .find_symbols_by_file(&fid)
            .unwrap()
            .into_iter()
            .find(|symbol| symbol.name == "stale")
            .unwrap();
        assert!(rebuilt.range.end_line > rebuilt.range.start_line);
    }

    #[cfg(feature = "arkts")]
    #[test]
    fn inverted_arkts_type_lines_force_structural_rebuild() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "MainPage.ets";
        let source = "\n@Component({ freezeWhenInactive: true })\nstruct MainPage {\n  build() { Text('ready') }\n}\n";
        std::fs::write(root.path().join(path), source).unwrap();

        let fid = FileId::generate(path);
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let frontend = create_frontend(Language::ArkTS).unwrap();
        let mut facts = extract_file_with_mode(
            &frontend,
            fid,
            Path::new(path),
            source,
            &hash,
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let stale = facts
            .symbols
            .iter_mut()
            .find(|symbol| symbol.name == "MainPage")
            .unwrap();
        assert!(stale.range.start_line > 0);
        stale.range.end_line = 0;
        store.insert_file_facts(&facts).unwrap();
        store
            .upsert_file_extraction_state(
                &fid,
                layer::STRUCTURAL,
                &hash,
                status::COMPLETE,
                FactCoverage::default(),
            )
            .unwrap();

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        assert!(!svc.has_structural_layer(&fid).unwrap());

        let result = svc.ensure_structural_for_file(&fid, None).unwrap();
        assert_eq!(result.files_built, 1);
        let rebuilt = store
            .find_symbols_by_file(&fid)
            .unwrap()
            .into_iter()
            .find(|symbol| symbol.name == "MainPage")
            .unwrap();
        assert!(rebuilt.range.end_line > rebuilt.range.start_line);
        assert_eq!(rebuilt.range.end_byte as usize, source.trim_end().len());
    }

    #[cfg(feature = "rust")]
    #[test]
    fn stale_multiline_rust_type_range_forces_structural_rebuild() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "stale.rs";
        let source = "struct Stale {\n    value: i32,\n}\n";
        std::fs::write(root.path().join(path), source).unwrap();

        let fid = FileId::generate(path);
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let frontend = create_frontend(Language::Rust).unwrap();
        let mut facts = extract_file_with_mode(
            &frontend,
            fid,
            Path::new(path),
            source,
            &hash,
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let stale = facts
            .symbols
            .iter_mut()
            .find(|symbol| symbol.name == "Stale")
            .unwrap();
        stale.range.end_line = stale.range.start_line;
        stale.range.end_byte = source.find('\n').unwrap() as u32;
        store.insert_file_facts(&facts).unwrap();
        store
            .upsert_file_extraction_state(
                &fid,
                layer::STRUCTURAL,
                &hash,
                status::COMPLETE,
                FactCoverage::default(),
            )
            .unwrap();

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        assert!(!svc.has_structural_layer(&fid).unwrap());

        let result = svc.ensure_structural_for_file(&fid, None).unwrap();
        assert_eq!(result.files_built, 1);
        let rebuilt = store
            .find_symbols_by_file(&fid)
            .unwrap()
            .into_iter()
            .find(|symbol| symbol.name == "Stale")
            .unwrap();
        assert!(rebuilt.range.end_line > rebuilt.range.start_line);
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
        assert!(
            store
                .find_active_file_extraction_job(&fid, layer::STRUCTURAL)
                .unwrap()
                .is_none(),
            "successful structural ensure must not leave an active extraction job"
        );
        assert!(svc.has_structural_layer(&fid).unwrap());
        let refs = store.find_references_by_file(&fid).unwrap();
        assert!(
            refs.iter().any(|r| r.name == "helper"),
            "structural ensure must parse references when no structural extraction_state exists"
        );
    }

    #[test]
    fn structural_ensure_materializes_inventory_only_file_before_claiming_job() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "inventory_only.c";
        let source = "int inventory_only(void) { return 1; }\n";
        let full_path = root.path().join(path);
        std::fs::write(&full_path, source).unwrap();

        let fid = FileId::generate(path);
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
                Language::C.as_str(),
                mtime,
                metadata.len() as i64,
                inode,
                dev,
            )
            .unwrap();
        assert!(
            store.get_file(&fid).unwrap().is_none(),
            "fixture must start as inventory-only"
        );

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        let result = svc.ensure_structural_for_file(&fid, None).unwrap();

        assert_eq!(result.files_built, 1);
        assert!(
            store.get_file(&fid).unwrap().is_some(),
            "lazy structural must materialize a files row before recording extraction jobs"
        );
        assert!(
            store
                .find_active_file_extraction_job(&fid, layer::STRUCTURAL)
                .unwrap()
                .is_none(),
            "successful structural ensure must not leave an active extraction job"
        );
        assert!(svc.has_structural_layer(&fid).unwrap());
    }

    #[test]
    fn structural_ensure_reports_existing_active_job_without_rebuilding() {
        let store = test_store();
        let root = tempfile::tempdir().unwrap();
        let path = "pending.c";
        let source = "int pending(void) { return 1; }\n";
        std::fs::write(root.path().join(path), source).unwrap();

        let fid = FileId::generate(path);
        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        store
            .upsert_file(&FileInfo {
                file_id: fid,
                path: path.to_string(),
                language: Language::C,
                content_hash,
                status: ParseStatus::Success,
            })
            .unwrap();
        let claim = store
            .claim_file_extraction_job(&fid, layer::STRUCTURAL, Some("query"), None, Some(1000))
            .unwrap();
        let job_id = match claim {
            ClaimResult::Claimed { job_id } => job_id,
            ClaimResult::AlreadyBuilding { .. } => panic!("first claim should own the job"),
        };

        let svc = LazyStructuralService::new(store.clone(), Some(root.path().to_path_buf()));
        let result = svc.ensure_structural_for_file(&fid, None).unwrap();

        assert_eq!(result.files_built, 0);
        assert_eq!(result.files_cached, 0);
        assert_eq!(result.files_pending, 1);
        assert_eq!(result.pending_job_ids, vec![job_id.clone()]);
        assert!(
            !svc.has_structural_layer(&fid).unwrap(),
            "a caller that does not own the active job must not rebuild the file"
        );
        let active = store
            .find_active_file_extraction_job(&fid, layer::STRUCTURAL)
            .unwrap()
            .expect("existing job should remain active");
        assert_eq!(active.job_id, job_id);
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
    fn candidates_from_ripgrep_covers_all_nonstandard_language_extensions() {
        if !rg_available() {
            eprintln!("skipping test: rg not available");
            return;
        }

        let files = [
            ("src/main.ets", "frameworkProbe()"),
            ("src/main.sts", "frameworkProbe()"),
            ("src/Main.cs", "frameworkProbe()"),
            ("src/main.cj", "frameworkProbe()"),
        ];
        let (_store, root, provider) = setup_ripgrep_test(&files, None);

        let candidates = provider.candidates_for_symbol("frameworkProbe").unwrap();
        cleanup_ripgrep_test(&root);

        for (path, _) in files {
            assert!(
                candidates.contains(&FileId::generate(path)),
                "{path} should participate in candidate discovery: {candidates:?}"
            );
        }
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
