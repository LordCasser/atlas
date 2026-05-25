//! `atlas_index` MCP tool — trigger project indexing from MCP clients.
//!
//! Accepts `analysis` mode ("structural" or "full") and runs the full
//! extraction → resolution → graph pipeline against the project root.

use std::collections::HashMap;
use std::sync::Arc;

use atlas_engine::{
    ExtractionMode, FileId, GraphBuilder, Language, LanguageRegistry,
    ParseWorkerPool, PhaseTimer, ReferenceResolver, Store, WorkerConfig,
    create_frontend,
    discovery::{DiscoveryConfig, discover_files},
};

use super::ToolRouter;

/// Result of an atlas_index invocation.
#[derive(serde::Serialize)]
struct IndexResult {
    ok: bool,
    files_discovered: usize,
    files_indexed: usize,
    files_failed: usize,
    symbols_found: usize,
    references_resolved: usize,
    errors: Vec<String>,
    duration_ms: u64,
}

impl ToolRouter {
    /// Handle `atlas_index` tool call.
    ///
    /// Parameters:
    ///   analysis: "structural" (default, no dataflow) | "full" (complete analysis)
    ///   exclude: list of glob patterns to skip (e.g. ["**/test/**", "**/*.test.ts"])
    ///
    /// Returns a JSON IndexResult with indexing statistics.
    pub(crate) fn handle_index(&self, args: &serde_json::Value) -> (String, bool) {
        let start = std::time::Instant::now();
        let analysis = args["analysis"].as_str().unwrap_or("structural");
        let mode = match analysis {
            "full" => ExtractionMode::Full,
            _ => ExtractionMode::Structural,
        };

        let exclude_patterns: Vec<String> = args["exclude"]
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
        };

        // Run the index pipeline
        match run_index(&self.store, &self.project_root, mode, &exclude_patterns) {
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

        let json = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
        (json, !result.ok)
    }
}

struct IndexStats {
    discovered: usize,
    indexed: usize,
    failed: usize,
    symbols: usize,
    resolved: usize,
}

/// Run the full index pipeline against `project_root`.
///
/// Opens a separate Store connection for writes (WAL mode allows concurrent
/// readers + one writer, so the MCP server's read connection is not blocked).
fn run_index(store: &Arc<Store>, project_root: &std::path::Path, mode: ExtractionMode, exclude_patterns: &[String]) -> anyhow::Result<IndexStats> {
    // ── Discovery ──────────────────────────────────────────────────────────
    let _disc_timer = PhaseTimer::start("discovery");
    let mut config = DiscoveryConfig::default();
    if !exclude_patterns.is_empty() {
        config.exclude_patterns = exclude_patterns.to_vec();
    }
    let discovered = discover_files(project_root, &config)?;
    if discovered.is_empty() {
        return Ok(IndexStats {
            discovered: 0, indexed: 0, failed: 0, symbols: 0, resolved: 0,
        });
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

    for rel_path in &discovered {
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
    }

    // ── Reference resolution ──────────────────────────────────────────────
    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved_refs, _stats) = match resolver.resolve_all() {
        Ok(r) => r,
        Err(e) => {
            anyhow::bail!("Reference resolution failed: {:#}", e);
        }
    };

    // ── Graph build ───────────────────────────────────────────────────────
    let builder = GraphBuilder::new(store.clone());
    let _build_stats = builder.build_all(&resolved_refs);

    Ok(IndexStats {
        discovered: discovered.len(),
        indexed,
        failed,
        symbols: total_symbols,
        resolved: _stats.resolved,
    })
}
