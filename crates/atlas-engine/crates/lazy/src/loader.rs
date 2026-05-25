//! LazyDataflowLoader: receive a [`LazyWindow`] and ensure each
//! AnalysisUnit has its dataflow built and persisted.
//!
//! Uses a `get_or_build` pattern: for each unit, check the
//! `analysis_artifacts` table.  If the artifact is missing or stale
//! (content_hash mismatch), call `extraction::extract_file_with_mode`
//! with [`ExtractionMode::LazyDataflow`] to build only the windowed
//! dataflow, then write results via `replace_dataflow_for_unit`.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use db::Store;
use extraction::{ExtractionMode, LanguageFrontend, create_frontend};
use types::enums::Language;
use types::lazy::{AnalysisUnit, LazyWindow};

use crate::constants::LAZY_DATAFLOW_BUDGET_MS;

/// Data produced by a single lazy dataflow build.
struct DataflowPayload {
    data_nodes: Vec<types::DataNode>,
    dataflow_edges: Vec<types::DataFlowEdge>,
    bindings: Vec<types::BindingDef>,
    binding_uses: Vec<types::BindingUse>,
    cfg_nodes: Vec<types::CfgNode>,
    cfg_edges: Vec<types::CfgEdge>,
    budget_exceeded: bool,
}

impl DataflowPayload {
    fn empty() -> Self {
        Self {
            data_nodes: vec![],
            dataflow_edges: vec![],
            bindings: vec![],
            binding_uses: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            budget_exceeded: false,
        }
    }
}

/// Result of a lazy-load invocation.
#[derive(Debug, Default)]
pub(crate) struct EnsureResult {
    pub units_built: usize,
    pub units_cached: usize,
    pub budget_exceeded: bool,
}

/// Thread-safe, process-lifetime cache for LanguageFrontend instances.
static FRONTEND_CACHE: OnceLock<HashMap<Language, LanguageFrontend>> = OnceLock::new();

/// Entry point for lazy dataflow loading.
pub(crate) struct LazyDataflowLoader;

impl LazyDataflowLoader {
    /// Ensure every unit in `window` has its dataflow built.
    ///
    /// For each unit:
    /// 1. Check `analysis_artifacts` — if content_hash matches, skip.
    /// 2. Otherwise: read source, parse, extract with
    ///    `ExtractionMode::LazyDataflow{window}`, write results to DB,
    ///    backfill callsite arg data_node_ids, and record the artifact.
    ///
    /// Hard budget protection: if [`LAZY_DATAFLOW_BUDGET_MS`] is exceeded,
    /// remaining units are skipped and `EnsureResult.budget_exceeded` is set.
    pub(crate) fn ensure(
        store: &Store,
        window: &LazyWindow,
        project_root: Option<&std::path::Path>,
    ) -> Result<EnsureResult> {
        let start = Instant::now();
        let mut result = EnsureResult::default();

        for unit in &window.units {
            // Budget guard
            if start.elapsed().as_millis() > LAZY_DATAFLOW_BUDGET_MS as u128 {
                result.budget_exceeded = true;
                break;
            }

            // get_or_build
            let (cached, payload) = get_or_build(store, unit, window, project_root)?;

            result.budget_exceeded |= payload.budget_exceeded;
            if cached {
                result.units_cached += 1;
            } else {
                result.units_built += 1;
            }
        }

        Ok(result)
    }
}

/// Check artifact cache, call builder closure on miss, write results.
///
/// Returns `(cached, payload)` where `cached` is true if the artifact
/// was already up-to-date (builder was NOT called).
fn get_or_build(
    store: &Store,
    unit: &AnalysisUnit,
    window: &LazyWindow,
    project_root: Option<&std::path::Path>,
) -> Result<(bool, DataflowPayload)> {
    // 1. Check artifact cache
    if let Some(artifact) = store.get_artifact(&unit.file_id, &unit.unit_id, "dataflow")? {
        let current_hash = store
            .get_file(&unit.file_id)?
            .map(|f| f.content_hash)
            .unwrap_or_default();
        if artifact.content_hash == current_hash {
            // Cache hit — but if the artifact was built under budget pressure,
            // propagate the truncation flag so the caller can surface it.
            let mut payload = DataflowPayload::empty();
            payload.budget_exceeded = artifact.budget_exceeded;
            return Ok((true, payload));
        }
    }

    // 2. Cache miss — build
    let payload = build_dataflow_for_unit(store, unit, window, project_root)?;

    // 3. Write to DB
    let current_hash = store
        .get_file(&unit.file_id)?
        .map(|f| f.content_hash)
        .unwrap_or_default();

    store.replace_dataflow_for_unit(
        unit,
        &payload.data_nodes,
        &payload.dataflow_edges,
        &payload.bindings,
        &payload.binding_uses,
        &payload.cfg_nodes,
        &payload.cfg_edges,
    )?;

    store.update_callsite_arg_data_nodes(unit, &payload.data_nodes)?;

    // 4. Record artifact
    let status = if payload.budget_exceeded { "partial" } else { "complete" };
    store.upsert_artifact(&db::store_rows::ArtifactRecord {
        file_id: unit.file_id,
        unit_id: unit.unit_id,
        layer: "dataflow".to_string(),
        content_hash: current_hash,
        status: status.to_string(),
        node_count: Some(payload.data_nodes.len() as i64),
        edge_count: Some(payload.dataflow_edges.len() as i64),
        budget_exceeded: payload.budget_exceeded,
        built_at: String::new(), // upsert_artifact fills datetime('now')
    })?;

    Ok((false, payload))
}

/// Build dataflow for a single unit by re-extracting its file with
/// `ExtractionMode::LazyDataflow`.
fn build_dataflow_for_unit(
    store: &Store,
    unit: &AnalysisUnit,
    window: &LazyWindow,
    project_root: Option<&std::path::Path>,
) -> Result<DataflowPayload> {
    // 1. Get file info to determine language and path
    let file_info = store
        .get_file(&unit.file_id)?
        .ok_or_else(|| anyhow::anyhow!("file not found in DB: {:?}", unit.file_id))?;

    // 2. Get cached frontend
    let frontend = get_cached_frontend(file_info.language)
        .ok_or_else(|| anyhow::anyhow!("frontend not available for {:?}", file_info.language))?;

    // 3. Read source file from disk.
    // If a project_root is provided, resolve relative paths against it.
    // Otherwise try the path as-is (for absolute paths or in-memory stores).
    let resolved_path = if let Some(root) = project_root {
        root.join(&file_info.path)
    } else {
        std::path::PathBuf::from(&file_info.path)
    };
    let source = std::fs::read_to_string(&resolved_path)
        .with_context(|| format!("failed to read source: {}", resolved_path.display()))?;

    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    // 3.5. Verify structural index is not stale.
    // The planner's unit ranges came from the DB structural index.  If the
    // file has been modified on disk since then, the ranges may not match
    // the current text — reject with a clear diagnostic instead of building
    // dataflow on mismatched coordinates.
    if content_hash != file_info.content_hash {
        anyhow::bail!(
            "Structural index is stale for {} (DB hash: {}, disk hash: {}). \
             Run atlas index or atlas_sync first.",
            file_info.path,
            &file_info.content_hash[..8.min(file_info.content_hash.len())],
            &content_hash[..8.min(content_hash.len())]
        );
    }

    // 4. Extract with LazyDataflow mode
    let file_path = std::path::Path::new(&file_info.path);
    let facts = extraction::extract_file_with_mode(
        frontend,
        unit.file_id,
        file_path,
        &source,
        &content_hash,
        ExtractionMode::LazyDataflow {
            window: window.clone(),
        },
    )?;

    Ok(DataflowPayload {
        data_nodes: facts.data_nodes,
        dataflow_edges: facts.dataflow_edges,
        bindings: facts.bindings,
        binding_uses: facts.binding_uses,
        cfg_nodes: facts.cfg_nodes,
        cfg_edges: facts.cfg_edges,
        budget_exceeded: facts.budget_exceeded,
    })
}

/// Get or initialise a LanguageFrontend from the process-lifetime cache.
fn get_cached_frontend(lang: Language) -> Option<&'static LanguageFrontend> {
    let map = FRONTEND_CACHE.get_or_init(|| {
        // Populate with all compiled-in languages
        let languages = [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::Go,
            Language::CSharp,
            Language::Rust,
            Language::Php,
            Language::Ruby,
            Language::Kotlin,
            Language::Bash,
        ];
        let mut cache = HashMap::new();
        for lang in languages {
            if let Some(fe) = create_frontend(lang) {
                cache.insert(lang, fe);
            }
        }
        cache
    });
    map.get(&lang)
}
