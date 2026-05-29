//! LazyDataflowLoader: receive a [`LazyWindow`] and ensure each
//! AnalysisUnit has its dataflow built and persisted.
//!
//! Uses a group-by-file pattern: units are grouped by [`FileId`], and each
//! file is re-extracted once via [`ExtractionMode::LazyDataflow`]. The
//! resulting facts are partitioned per unit and persisted via
//! `replace_dataflow_for_unit`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Context, Result};
use db::store_rows::ArtifactRecord;
use db::Store;
use extraction::{ExtractionMode, LanguageFrontend, create_frontend};
use types::enums::Language;
use types::ids::{BindingId, CfgNodeId, DataNodeId, FileId};
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
    /// Units are grouped by `file_id` to avoid re-reading and re-parsing
    /// the same source file multiple times.  For each file group:
    /// 1. Check artifact cache per unit (including pre-built data guard)
    /// 2. If any unit is uncached, call `build_dataflow_for_file` ONCE
    /// 3. Partition the resulting facts per uncached unit
    /// 4. Write facts via `replace_dataflow_for_unit` + `upsert_artifact`
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

        // Group units by file_id
        let mut groups: HashMap<FileId, Vec<&AnalysisUnit>> = HashMap::new();
        for unit in &window.units {
            groups.entry(unit.file_id).or_default().push(unit);
        }

        for (_file_id, units) in &groups {
            // Budget guard — check before each file group
            if start.elapsed().as_millis() > LAZY_DATAFLOW_BUDGET_MS as u128 {
                result.budget_exceeded = true;
                break;
            }

            // Step 1: Check cache for each unit, track which need building
            let mut uncached: Vec<&AnalysisUnit> = Vec::new();
            let mut cached_count = 0usize;

            for unit in units {
                let (cached, payload) = check_cache(store, unit)?;
                if cached {
                    result.budget_exceeded |= payload.budget_exceeded;
                    cached_count += 1;
                } else {
                    uncached.push(unit);
                }
            }
            result.units_cached += cached_count;

            // Step 2: If no uncached units, skip to next group
            if uncached.is_empty() {
                continue;
            }

            // Step 3: Build dataflow for the file once
            let payload = build_dataflow_for_file(store, units[0].file_id, window, project_root)?;
            result.budget_exceeded |= payload.budget_exceeded;

            // Step 4: Partition and write per uncached unit
            let current_hash = store
                .get_file(&units[0].file_id)?
                .map(|f| f.content_hash)
                .unwrap_or_default();

            for unit in &uncached {
                let unit_payload = partition_payload_for_unit(&payload, unit);

                store.replace_dataflow_for_unit(
                    unit,
                    &unit_payload.data_nodes,
                    &unit_payload.dataflow_edges,
                    &unit_payload.bindings,
                    &unit_payload.binding_uses,
                    &unit_payload.cfg_nodes,
                    &unit_payload.cfg_edges,
                )?;

                store.update_callsite_arg_data_nodes(unit, &unit_payload.data_nodes)?;

                let status = if payload.budget_exceeded {
                    "partial"
                } else {
                    "complete"
                };
                store.upsert_artifact(&ArtifactRecord {
                    file_id: unit.file_id,
                    unit_id: unit.unit_id,
                    layer: "dataflow".to_string(),
                    content_hash: current_hash.clone(),
                    status: status.to_string(),
                    node_count: Some(unit_payload.data_nodes.len() as i64),
                    edge_count: Some(unit_payload.dataflow_edges.len() as i64),
                    budget_exceeded: payload.budget_exceeded,
                    built_at: String::new(),
                })?;

                result.units_built += 1;
            }
        }

        Ok(result)
    }
}

/// Check whether a unit's dataflow artifact is already cached (including
/// pre-built data from a full index).  Does NOT build or write anything.
///
/// Returns `(cached, payload)` where `cached` is true if the artifact
/// was already up-to-date.
fn check_cache(store: &Store, unit: &AnalysisUnit) -> Result<(bool, DataflowPayload)> {
    // 1. Check artifact cache
    if let Some(artifact) = store.get_artifact(&unit.file_id, &unit.unit_id, "dataflow")? {
        let current_hash = store
            .get_file(&unit.file_id)?
            .map(|f| f.content_hash)
            .unwrap_or_default();
        if artifact.content_hash == current_hash {
            let mut payload = DataflowPayload::empty();
            payload.budget_exceeded = artifact.budget_exceeded;
            return Ok((true, payload));
        }
    }

    // 1.5. Check for pre-built dataflow from a full index
    {
        let prebuilt = store.count_data_nodes_for_unit(unit).unwrap_or(0);
        if prebuilt > 0 {
            let current_hash = store
                .get_file(&unit.file_id)?
                .map(|f| f.content_hash)
                .unwrap_or_default();
            store.upsert_artifact(&ArtifactRecord {
                file_id: unit.file_id,
                unit_id: unit.unit_id,
                layer: "dataflow".to_string(),
                content_hash: current_hash,
                status: "complete".to_string(),
                node_count: Some(prebuilt as i64),
                edge_count: None,
                budget_exceeded: false,
                built_at: String::new(),
            })?;
            return Ok((true, DataflowPayload::empty()));
        }
    }

    Ok((false, DataflowPayload::empty()))
}

/// Build dataflow for a file in one pass (shared by all units in the file group).
///
/// Reads the file once, parses once, and extracts once, returning the full
/// `DataflowPayload` for all units in the window that belong to this file.
fn build_dataflow_for_file(
    store: &Store,
    file_id: FileId,
    window: &LazyWindow,
    project_root: Option<&std::path::Path>,
) -> Result<DataflowPayload> {
    // 1. Get file info
    let file_info = store
        .get_file(&file_id)?
        .ok_or_else(|| anyhow::anyhow!("file not found in DB: {:?}", file_id))?;

    // 2. Get cached frontend
    let frontend = get_cached_frontend(file_info.language)
        .ok_or_else(|| anyhow::anyhow!("frontend not available for {:?}", file_info.language))?;

    // 3. Read source file
    let resolved_path = if let Some(root) = project_root {
        root.join(&file_info.path)
    } else {
        std::path::PathBuf::from(&file_info.path)
    };
    let source = std::fs::read_to_string(&resolved_path)
        .with_context(|| format!("failed to read source: {}", resolved_path.display()))?;

    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    // 3.5. Verify structural index is not stale
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
        file_id,
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

/// Partition a full `DataflowPayload` to only contain facts belonging to
/// the given `AnalysisUnit`.
///
/// For function-scoped units, matches `function_id == unit.symbol_id`.
/// For top-level (file-scoped) units, matches `function_id IS NULL`.
/// Edges are included when either endpoint belongs to this unit's nodes.
fn partition_payload_for_unit(payload: &DataflowPayload, unit: &AnalysisUnit) -> DataflowPayload {
    // Partition data nodes by function_id
    let data_nodes: Vec<types::DataNode> = payload
        .data_nodes
        .iter()
        .filter(|dn| dn.function_id == unit.symbol_id)
        .cloned()
        .collect();
    let data_node_ids: HashSet<DataNodeId> = data_nodes.iter().map(|dn| dn.id).collect();

    // Partition bindings by function_id
    let bindings: Vec<types::BindingDef> = payload
        .bindings
        .iter()
        .filter(|b| b.function_id == unit.symbol_id)
        .cloned()
        .collect();
    let binding_ids: HashSet<BindingId> = bindings.iter().map(|b| b.id).collect();

    // Partition cfg_nodes by function_id (CfgNode always has function_id,
    // so top-level units will always get an empty set)
    let cfg_nodes: Vec<types::CfgNode> = payload
        .cfg_nodes
        .iter()
        .filter(|cn| {
            unit.symbol_id
                .map_or(false, |sid| cn.function_id == sid)
        })
        .cloned()
        .collect();
    let cfg_node_ids: HashSet<CfgNodeId> = cfg_nodes.iter().map(|cn| cn.id).collect();

    // Partition dataflow_edges: include edges where either endpoint belongs
    // to this unit's data nodes
    let dataflow_edges: Vec<types::DataFlowEdge> = payload
        .dataflow_edges
        .iter()
        .filter(|e| data_node_ids.contains(&e.source) || data_node_ids.contains(&e.target))
        .cloned()
        .collect();

    // Partition binding_uses: include uses that reference this unit's bindings
    let binding_uses: Vec<types::BindingUse> = payload
        .binding_uses
        .iter()
        .filter(|bu| bu.binding_id.map_or(false, |bid| binding_ids.contains(&bid)))
        .cloned()
        .collect();

    // Partition cfg_edges: include edges where either endpoint belongs to
    // this unit's cfg nodes
    let cfg_edges: Vec<types::CfgEdge> = payload
        .cfg_edges
        .iter()
        .filter(|e| cfg_node_ids.contains(&e.source) || cfg_node_ids.contains(&e.target))
        .cloned()
        .collect();

    DataflowPayload {
        data_nodes,
        dataflow_edges,
        bindings,
        binding_uses,
        cfg_nodes,
        cfg_edges,
        budget_exceeded: payload.budget_exceeded,
    }
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
