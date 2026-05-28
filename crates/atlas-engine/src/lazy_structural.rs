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
//! - Leverages `file_index_layers` table for cache decisions.
//! - Respects the same `ExtractionMode::Structural` pipeline as `atlas index`.
//! - [`CandidateProvider`] is a trait — swap implementations for different
//!   discovery strategies (e.g. compile_commands.json, ctags, custom heuristics).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::ids::FileId;
use types::{layer, status};

/// Maximum candidate files to consider for lazy structural loading.
const MAX_CANDIDATE_FILES: usize = 20;

/// Wall-clock budget for a lazy structural invocation (milliseconds).
const LAZY_STRUCTURAL_BUDGET_MS: u64 = 30_000;

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
        // 2. Fallback: ripgrep
        self.candidates_from_ripgrep(name)
    }

    fn candidates_for_path(&self, path: &str) -> Result<Vec<FileId>> {
        Ok(vec![FileId::generate(path)])
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

    fn candidates_from_ripgrep(&self, name: &str) -> Result<Vec<FileId>> {
        let project_root = match &self.project_root {
            Some(r) => r.clone(),
            None => return Ok(Vec::new()),
        };
        let output = std::process::Command::new("rg")
            .args([
                "--files-with-matches",
                "--no-heading",
                "--word-regexp",
                "--fixed-strings",
                "--max-count=1",
                name,
            ])
            .current_dir(&project_root)
            .output();
        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return Ok(Vec::new()),
        };
        let mut file_ids = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(MAX_CANDIDATE_FILES)
        {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            file_ids.push(FileId::generate(line));
        }
        Ok(file_ids)
    }
}

// ---------------------------------------------------------------------------
// LazyStructuralService
// ---------------------------------------------------------------------------

/// Outcome of a lazy structural ensure invocation.
#[derive(Debug, Clone)]
pub struct EnsureStructuralResult {
    pub files_built: usize,
    pub files_cached: usize,
    pub budget_exceeded: bool,
}

/// Entry point for query-driven lazy structural extraction.
///
/// Holds a [`CandidateProvider`] for file discovery and a [`Store`] for
/// cache checks and re-extraction.  By default uses [`DefaultCandidateProvider`].
pub struct LazyStructuralService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    candidate_provider: Box<dyn CandidateProvider>,
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

    /// Create a service with a custom candidate provider.
    #[allow(dead_code)]
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
            });
        }
        self.ensure_structural_for_files(&candidates)
    }

    /// Ensure a specific file has full structural facts.
    pub fn ensure_structural_for_file(&self, file_id: &FileId) -> Result<EnsureStructuralResult> {
        self.ensure_structural_for_files(&[*file_id])
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
        self.ensure_structural_for_files(file_ids)
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
            .get_file_index_layer(file_id, layer::STRUCTURAL)?;
        Ok(layer.map_or(false, |(s, hash)| {
            s == status::COMPLETE && hash == *current_hash
        }))
    }

    // ── Internal ────────────────────────────────────────────────────────

    fn ensure_structural_for_files(&self, file_ids: &[FileId]) -> Result<EnsureStructuralResult> {
        let start = std::time::Instant::now();
        let mut result = EnsureStructuralResult {
            files_built: 0,
            files_cached: 0,
            budget_exceeded: false,
        };
        let mut built_file_ids: Vec<FileId> = Vec::new();

        for file_id in file_ids {
            if start.elapsed().as_millis() > LAZY_STRUCTURAL_BUDGET_MS as u128 {
                result.budget_exceeded = true;
                break;
            }
            if self.has_structural_layer(file_id)? {
                result.files_cached += 1;
                continue;
            }
            match self.reindex_file_structural(file_id) {
                Ok(()) => {
                    result.files_built += 1;
                    built_file_ids.push(*file_id);
                }
                Err(e) => {
                    tracing::warn!("Lazy structural failed for {:?}: {:#}", file_id, e);
                }
            }
        }

        if !built_file_ids.is_empty() {
            self.incremental_resolve_and_build(&built_file_ids)?;
        }

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
    fn reindex_file_structural(&self, file_id: &FileId) -> Result<()> {
        let file_info = self
            .store
            .get_file(file_id)?
            .ok_or_else(|| anyhow::anyhow!("file not found: {:?}", file_id))?;
        let frontend = create_frontend(file_info.language).ok_or_else(|| {
            anyhow::anyhow!("frontend not available for {:?}", file_info.language)
        })?;
        let resolved_path = self.resolve_file_path(&file_info.path);
        // Security: ensure path is within project root.
        if let Some(root) = &self.project_root {
            let canonical_root = root.canonicalize().with_context(|| {
                format!("failed to canonicalize project root {}", root.display())
            })?;
            let canonical_file = resolved_path.canonicalize().with_context(|| {
                format!("failed to canonicalize {}", resolved_path.display())
            })?;
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

        // Invalidate cross-file references BEFORE the atomic replace
        // (preserves reference rows so they can be re-resolved).
        self.store
            .invalidate_references_to_symbols_in_file(file_id)?;
        self.store.delete_edges_for_file_references(file_id)?;

        let facts = extract_file_with_mode(
            &frontend,
            *file_id,
            std::path::Path::new(&file_info.path),
            &source,
            &content_hash,
            ExtractionMode::Structural,
        )?;

        // Atomically delete old data and insert new facts.
        self.store.replace_file_facts(file_id, &facts)?;

        tracing::info!(
            "Lazy structural: {} ({} symbols, {} refs)",
            file_info.path,
            facts.symbol_count(),
            facts.reference_count()
        );
        Ok(())
    }

    fn incremental_resolve_and_build(&self, file_ids: &[FileId]) -> Result<()> {
        let mut resolver = resolution::ReferenceResolver::new(self.store.clone());
        let (resolved_refs, _stats) = resolver.resolve_for_files(file_ids)?;
        let builder = graph::GraphBuilder::new(self.store.clone());
        let _build_stats = builder.build_for_files(&resolved_refs, file_ids);
        Ok(())
    }

    fn resolve_file_path(&self, relative: &str) -> PathBuf {
        match &self.project_root {
            Some(root) => root.join(relative),
            None => PathBuf::from(relative),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;

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
}
