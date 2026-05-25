//! `atlas_index` MCP tool — trigger project indexing from MCP clients.
//!
//! Accepts `analysis` mode ("structural" or "full") and runs the full
//! extraction → resolution → graph pipeline against the project root.

use std::collections::HashMap;
use std::sync::Arc;

use atlas_engine::{
    ExtractionMode, FileId, FileLock, GraphBuilder, Language, LanguageRegistry,
    ParseWorkerPool, PhaseTimer, ReferenceResolver, Store, WorkerConfig,
    create_frontend,
    discovery::{DiscoveryConfig, discover_files},
};

use super::ToolRouter;

/// Result of an atlas_index invocation.
#[derive(serde::Serialize, Clone)]
pub(crate) struct IndexResult {
    pub(crate) ok: bool,
    pub(crate) files_discovered: usize,
    pub(crate) files_indexed: usize,
    pub(crate) files_failed: usize,
    pub(crate) symbols_found: usize,
    pub(crate) references_resolved: usize,
    pub(crate) errors: Vec<String>,
    pub(crate) duration_ms: u64,
    /// Warning for large projects that may cause MCP timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
}

impl ToolRouter {
    /// Handle `atlas_index` tool call.
    ///
    /// Parameters:
    ///   analysis: "structural" (default, no dataflow) | "full" (complete analysis)
    ///   exclude: list of glob patterns to skip (e.g. ["**/test/**", "**/*.test.ts"])
    ///
    /// If [`Self::progress_sender`] is set, progress notifications are sent at each
    /// pipeline phase (discovery, extraction, resolution, graph build).
    ///
    /// Returns a JSON IndexResult with indexing statistics.
    pub(crate) fn handle_index(&self, args: &serde_json::Value) -> (String, bool) {
        let start = std::time::Instant::now();
        let analysis = args["analysis"].as_str().unwrap_or("manifest");
        let mode = match analysis {
            "structural" => ExtractionMode::Structural,
            "full" => ExtractionMode::Full,
            _ => ExtractionMode::Manifest,
        };

        let exclude_patterns: Vec<String> = args["exclude"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let include_patterns: Vec<String> = args["include"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let mut result = IndexResult {
            ok: false,
            files_discovered: 0,
            files_indexed: 0,
            files_failed: 0,
            symbols_found: 0,
            references_resolved: 0,
            errors: Vec::new(),
            duration_ms: 0,
            warning: None,
        };

        // Acquire FileLock for persistent stores to prevent races with CLI
        // or other MCP processes writing the same .atlas/atlas.db.
        let is_persistent = self.store.db_path() != std::path::Path::new(":memory:");
        let _lock_guard = if is_persistent {
            match FileLock::acquire(&self.store) {
                Ok(g) => Some(g),
                Err(e) => {
                    result.errors.push(format!(
                        "Cannot acquire exclusive lock (another atlas process may be indexing): {:#}",
                        e
                    ));
                    let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
                    return (json, true);
                }
            }
        } else {
            None
        };

        // Run the index pipeline
        let progress_sender = self.progress_sender.clone();
        match run_index(&self.store, &self.project_root, mode, &include_patterns, &exclude_patterns, progress_sender) {
            Ok(stats) => {
                result.ok = true;
                result.files_discovered = stats.discovered;
                result.files_indexed = stats.indexed;
                result.files_failed = stats.failed;
                result.symbols_found = stats.symbols;
                result.references_resolved = stats.resolved;
            }
            Err(e) => {
                result.errors.push(format!("Index failed: {:#}", e));
            }
        }

        result.duration_ms = start.elapsed().as_millis() as u64;

        // ── Large project warning ─────────────────────────────────────────
        if result.duration_ms > 30_000 {
            result.warning = Some(
                "Indexing took over 30 seconds. For large projects, consider running 'atlas index' locally before connecting via MCP to avoid timeout issues. The CLI command is: atlas index --analysis structural"
                    .into(),
            );
        }

        let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
        (json, !result.ok)
    }
}

pub(crate) struct IndexStats {
    pub(crate) discovered: usize,
    pub(crate) indexed: usize,
    pub(crate) failed: usize,
    pub(crate) symbols: usize,
    pub(crate) resolved: usize,
}

/// Run the full index pipeline against `project_root`.
///
/// Writes directly to the provided store. The caller is responsible for
/// FileLock coordination in persistent mode.
///
/// If `progress_sender` is provided, progress reports are sent at each major
/// phase: discovery (10%), extraction (10%-60%), resolution (80%), graph (95%).
pub(crate) fn run_index(
    store: &Arc<Store>,
    project_root: &std::path::Path,
    mode: ExtractionMode,
    include_patterns: &[String],
    exclude_patterns: &[String],
    progress_sender: Option<super::ProgressSender>,
) -> anyhow::Result<IndexStats> {
    // ── Discovery ──────────────────────────────────────────────────────────
    let _disc_timer = PhaseTimer::start("discovery");
    let mut config = DiscoveryConfig::default();
    if !include_patterns.is_empty() {
        config.include_patterns = include_patterns.to_vec();
    }
    if !exclude_patterns.is_empty() {
        config.exclude_patterns = exclude_patterns.to_vec();
    }
    let discovered = discover_files(project_root, &config)?;
    if discovered.is_empty() {
        return Ok(IndexStats {
            discovered: 0, indexed: 0, failed: 0, symbols: 0, resolved: 0,
        });
    }

    // ── Progress: discovery complete ─────────────────────────────────────
    let total_files = discovered.len() as f64;
    if let Some(ref sender) = progress_sender {
        let _ = sender.send((0.10, Some(1.0), Some(format!("Discovered {} files, starting extraction...", discovered.len()))));
    }

    // ── Language init ──────────────────────────────────────────────────────
    let languages: Vec<Language> = discovered
        .iter()
        .filter_map(|p| Language::from_path(p))
        .fold(Vec::new(), |mut acc, lang| {
            if !acc.contains(&lang) { acc.push(lang); }
            acc
        });

    let _registry = LanguageRegistry::new(&languages)?;
    let frontend_cache: HashMap<Language, atlas_engine::LanguageFrontend> = languages
        .iter()
        .filter_map(|&lang| create_frontend(lang).map(|fe| (lang, fe)))
        .collect();

    // ── Clean stale facts ──────────────────────────────────────────────────
    // Delete existing facts for files that will be re-indexed.
    // We collect all FileIds first, then delete them.
    let file_ids: Vec<FileId> = discovered
        .iter()
        .map(|p| FileId::generate(&p.to_string_lossy()))
        .collect();
    // Invalidate cross-file references pointing into these files
    for fid in &file_ids {
        let _ = store.invalidate_references_to_symbols_in_file(fid);
    }
    // Delete existing data for these files (CASCADE cleans related rows)
    if let Err(e) = store.delete_files_batch(&file_ids) {
        anyhow::bail!("Failed to clean stale facts: {:#}", e);
    }

    // ── Parallel extraction ───────────────────────────────────────────────
    let pool = ParseWorkerPool::new(WorkerConfig::default());
    let mut indexed = 0usize;
    let mut failed = 0usize;
    let mut total_symbols = 0usize;

    for (i, rel_path) in discovered.iter().enumerate() {
        let abs_path = project_root.join(rel_path);
        let lang = match Language::from_path(rel_path) {
            Some(l) => l,
            None => continue,
        };
        let frontend = match frontend_cache.get(&lang) {
            Some(f) => f,
            None => continue,
        };

        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => {
                failed += 1;
                continue;
            }
        };

        let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let file_id = FileId::generate(&rel_path.to_string_lossy());

        match pool.extract_one(frontend, file_id, &abs_path, &source, &content_hash, mode.clone()) {
            Ok(facts) => {
                total_symbols += facts.symbols.len();
                // Insert facts into a separate write Store connection
                // to avoid holding the MCP server's read lock.
                if let Err(e) = store.insert_file_facts(&facts) {
                    failed += 1;
                    tracing::warn!("Insert failed for {}: {:#}", rel_path.display(), e);
                } else {
                    indexed += 1;
                }
            }
            Err(e) => {
                failed += 1;
                tracing::warn!("Extraction failed for {}: {}", rel_path.display(), e.message);
            }
        }

        // ── Progress: extraction (10%-60%, report every 50 files) ────────
        if let Some(ref sender) = progress_sender {
            if i % 50 == 0 || i == discovered.len() - 1 {
                let fraction = 0.10 + 0.50 * (indexed + failed) as f64 / total_files.max(1.0);
                let msg = format!("Extracting files... {}/{} processed ({} indexed, {} failed)", 
                    indexed + failed, discovered.len(), indexed, failed);
                let _ = sender.send((fraction.min(0.60), Some(1.0), Some(msg)));
            }
        }
    }

    // ── Progress: extraction complete ────────────────────────────────────
    if let Some(ref sender) = progress_sender {
        let _ = sender.send((0.65, Some(1.0), Some(format!(
            "Extraction complete: {} indexed, {} failed ({} symbols found)",
            indexed, failed, total_symbols
        ))));
    }

    // ── Reference resolution ──────────────────────────────────────────────
    if let Some(ref sender) = progress_sender {
        let _ = sender.send((0.75, Some(1.0), Some("Resolving symbol references...".into())));
    }
    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved_refs, _stats) = match resolver.resolve_all() {
        Ok(r) => r,
        Err(e) => {
            anyhow::bail!("Reference resolution failed: {:#}", e);
        }
    };

    // ── Graph build ───────────────────────────────────────────────────────
    if let Some(ref sender) = progress_sender {
        let _ = sender.send((0.90, Some(1.0), Some("Building symbol graph...".into())));
    }
    let builder = GraphBuilder::new(store.clone());
    let _build_stats = builder.build_all(&resolved_refs);

    // ── Progress: indexing complete ──────────────────────────────────────
    if let Some(ref sender) = progress_sender {
        let _ = sender.send((1.0, Some(1.0), Some(format!(
            "Indexing complete: {} files indexed ({} failed), {} symbols, {} resolved",
            indexed, failed, total_symbols, _stats.resolved
        ))));
    }

    Ok(IndexStats {
        discovered: discovered.len(),
        indexed,
        failed,
        symbols: total_symbols,
        resolved: _stats.resolved,
    })
}
