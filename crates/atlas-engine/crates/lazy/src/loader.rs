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
use db::store_rows::UnitExtractionStateRecord;
use db::{ClaimResult, Store};
use extraction::{ExtractionMode, LanguageFrontend, create_frontend};
use types::capability::LanguageCapabilityProfile;
use types::enums::Language;
use types::ids::{BindingId, CallsiteId, CfgNodeId, DataNodeId, FileId};
use types::lazy::{AnalysisUnit, LazyWindow};
use types::structs::FactCoverage;

use crate::constants::{LAYER_DATAFLOW, LAZY_DATAFLOW_BUDGET_MS, STATUS_COMPLETE, STATUS_PARTIAL};
use crate::planner::estimate_unit_cost;

/// Data produced by a single lazy dataflow build.
struct DataflowPayload {
    data_nodes: Vec<types::DataNode>,
    dataflow_edges: Vec<types::DataFlowEdge>,
    bindings: Vec<types::BindingDef>,
    binding_uses: Vec<types::BindingUse>,
    cfg_nodes: Vec<types::CfgNode>,
    cfg_edges: Vec<types::CfgEdge>,
    budget_exceeded: bool,
    has_cfg: bool,
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
            has_cfg: false,
        }
    }
}

/// Result of a lazy-load invocation.
#[derive(Debug, Default)]
pub(crate) struct EnsureResult {
    pub units_built: usize,
    pub units_cached: usize,
    pub units_pending: usize,
    pub pending_job_ids: Vec<String>,
    pub budget_exceeded: bool,
    pub has_cfg: bool,
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
    /// 1. Check unit extraction state per unit (including pre-built data guard)
    /// 2. If any unit is uncached, call `build_dataflow_for_file` ONCE
    /// 3. Partition the resulting facts per uncached unit
    /// 4. Write facts via `replace_dataflow_for_unit` + `upsert_unit_extraction_state`
    ///
    /// Hard budget protection: if [`LAZY_DATAFLOW_BUDGET_MS`] is exceeded,
    /// remaining units are skipped and `EnsureResult.budget_exceeded` is set.
    pub(crate) fn ensure(
        store: &Store,
        window: &LazyWindow,
        project_root: Option<&std::path::Path>,
        trigger_query: Option<&str>,
    ) -> Result<EnsureResult> {
        let start = Instant::now();
        let mut result = EnsureResult::default();

        // Group units by file_id
        let mut groups: HashMap<FileId, Vec<&AnalysisUnit>> = HashMap::new();
        for unit in &window.units {
            groups.entry(unit.file_id).or_default().push(unit);
        }

        // Sort file groups by total estimated cost (cheapest first) so maximal
        // units complete within budget before expensive files consume it.
        let mut sorted_groups: Vec<(FileId, Vec<&AnalysisUnit>)> = groups.into_iter().collect();
        sorted_groups
            .sort_by_key(|(_, units)| units.iter().map(|u| estimate_unit_cost(u)).sum::<u64>());

        for (_file_id, units) in &sorted_groups {
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
                    result.has_cfg |= payload.has_cfg;
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

            let mut claimed: Vec<(&AnalysisUnit, String)> = Vec::new();
            for unit in uncached {
                match store.claim_dataflow_extraction_job(
                    unit,
                    trigger_query,
                    Some(LAZY_DATAFLOW_BUDGET_MS as i64),
                )? {
                    ClaimResult::Claimed { job_id } => claimed.push((unit, job_id)),
                    ClaimResult::AlreadyBuilding { job_id } => {
                        result.units_pending += 1;
                        result.pending_job_ids.push(job_id);
                    }
                }
            }

            if claimed.is_empty() {
                continue;
            }

            // Step 3: Build dataflow for the file once
            let payload =
                match build_dataflow_for_file(store, units[0].file_id, window, project_root) {
                    Ok(payload) => payload,
                    Err(err) => {
                        let msg = format!("{err:#}");
                        for (_, job_id) in &claimed {
                            let _ = store.fail_extraction_job(job_id, &msg);
                        }
                        return Err(err);
                    }
                };
            result.budget_exceeded |= payload.budget_exceeded;

            // ── Callsite ID remap ───────────────────────────────────────
            // LazyDataflow skips callsite extraction (mode.rs:86-89), so
            // DataNodes keep provisional byte-based callsite_ids set during
            // dataflow extraction.  Query the DB's structural callsites
            // (already written during the structural index phase) and build
            // a provisional→real map.
            let cs_id_map: std::collections::HashMap<CallsiteId, CallsiteId> =
                match store.find_callsites_by_file(&units[0].file_id) {
                    Ok(callsites) => callsites,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to find callsites for lazy remap (file {:?}): {e:#}",
                            units[0].file_id
                        );
                        Vec::new()
                    }
                }
                .iter()
                .map(|cs| {
                    (
                        CallsiteId::from_file_byte(&units[0].file_id, cs.range.start_byte),
                        cs.id,
                    )
                })
                .collect();

            // Step 4: Partition and write per uncached unit
            let file_info = store.get_file(&units[0].file_id)?.ok_or_else(|| {
                anyhow::anyhow!("file not found during lazy write: {:?}", units[0].file_id)
            })?;
            let current_hash = file_info.content_hash.clone();
            let file_lang = file_info.language;

            // Determine CFG capability from the language profile.
            // DATAFLOW is always set: an empty result for an empty function is a
            // successful outcome, not a missing capability.
            let profile = LanguageCapabilityProfile::for_language(file_lang);
            let cfg_supported = profile.features.cfg.is_supported();

            for (unit, job_id) in &claimed {
                // Interleaved budget guard: exit early if over budget
                // mid-group, so remaining units in this file group are
                // skipped without waiting for the next file-group check.
                if start.elapsed().as_millis() > LAZY_DATAFLOW_BUDGET_MS as u128 {
                    result.budget_exceeded = true;
                    break;
                }

                let mut unit_payload = partition_payload_for_unit(&payload, unit);

                // Remap provisional byte-based callsite_ids to real
                // CallsiteIds so that downstream backfill and query
                // joins (update_callsite_arg_data_nodes,
                // find_data_nodes_by_callsite) operate on real IDs.
                for dn in &mut unit_payload.data_nodes {
                    if let Some(ref provisional) = dn.callsite_id {
                        if let Some(real) = cs_id_map.get(provisional) {
                            dn.callsite_id = Some(*real);
                        }
                    }
                }

                let write_result = (|| -> Result<()> {
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
                        STATUS_PARTIAL
                    } else {
                        STATUS_COMPLETE
                    };

                    // Build capability mask: base bits are always present.
                    // DATAFLOW: set unconditionally (extraction completed — empty
                    //   result for an empty function is a success).
                    // CFG: only set if the language supports it AND actual CFG
                    //   nodes were produced for this unit.
                    let mut mask_bits = FactCoverage::MANIFEST
                        | FactCoverage::STRUCTURAL
                        | FactCoverage::CALL_EDGES
                        | FactCoverage::DATAFLOW;
                    if cfg_supported && !unit_payload.cfg_nodes.is_empty() {
                        mask_bits |= FactCoverage::CFG;
                        result.has_cfg = true;
                    }

                    store.upsert_unit_extraction_state(&UnitExtractionStateRecord {
                        file_id: unit.file_id,
                        unit_id: unit.unit_id,
                        layer: LAYER_DATAFLOW.to_string(),
                        content_hash: current_hash.clone(),
                        status: status.to_string(),
                        node_count: Some(unit_payload.data_nodes.len() as i64),
                        edge_count: Some(unit_payload.dataflow_edges.len() as i64),
                        budget_exceeded: payload.budget_exceeded,
                        capability_mask: FactCoverage::from_bits(mask_bits),
                        built_at: String::new(),
                    })?;
                    Ok(())
                })();

                if let Err(err) = write_result {
                    let _ = store.fail_extraction_job(job_id, &format!("{err:#}"));
                    return Err(err);
                }

                store.complete_extraction_job(job_id)?;
                result.units_built += 1;
            }
        }

        Ok(result)
    }
}

/// Check whether a unit's dataflow state is already cached (including
/// pre-built data from a full index).  Does NOT build or write anything.
///
/// Returns `(cached, payload)` where `cached` is true if the unit state
/// was already up-to-date.
fn check_cache(store: &Store, unit: &AnalysisUnit) -> Result<(bool, DataflowPayload)> {
    // 1. Check unit extraction state.
    if let Some(unit_state) =
        store.get_unit_extraction_state(&unit.file_id, &unit.unit_id, LAYER_DATAFLOW)?
    {
        let current_hash = store
            .get_file(&unit.file_id)?
            .map(|f| f.content_hash)
            .unwrap_or_default();
        if unit_state.content_hash == current_hash {
            let mut payload = DataflowPayload::empty();
            payload.budget_exceeded = unit_state.budget_exceeded;
            payload.has_cfg = unit_state.capability_mask.has(FactCoverage::CFG);
            return Ok((true, payload));
        }
    }

    // 1.5. Check for pre-built dataflow from a full index
    {
        let prebuilt = store.count_data_nodes_for_unit(unit).unwrap_or(0);
        if prebuilt > 0 {
            let file = store.get_file(&unit.file_id)?.ok_or_else(|| {
                anyhow::anyhow!("file not found for prebuilt check: {:?}", unit.file_id)
            })?;
            let current_hash = file.content_hash;
            let file_lang = file.language;

            // CFG capability is gated by the language profile.
            // DATAFLOW is always set: pre-existing data nodes confirm a prior
            // successful extraction.
            let profile = LanguageCapabilityProfile::for_language(file_lang);
            let cfg_supported = profile.features.cfg.is_supported();

            let mut mask_bits = FactCoverage::MANIFEST
                | FactCoverage::STRUCTURAL
                | FactCoverage::CALL_EDGES
                | FactCoverage::DATAFLOW;
            let unit_has_cfg = cfg_supported
                && unit
                    .symbol_id
                    .as_ref()
                    .map(|sym_id| {
                        store
                            .find_cfg_nodes_by_function(sym_id)
                            .map(|nodes| !nodes.is_empty())
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
            if unit_has_cfg {
                mask_bits |= FactCoverage::CFG;
            }

            store.upsert_unit_extraction_state(&UnitExtractionStateRecord {
                file_id: unit.file_id,
                unit_id: unit.unit_id,
                layer: LAYER_DATAFLOW.to_string(),
                content_hash: current_hash,
                status: STATUS_COMPLETE.to_string(),
                node_count: Some(prebuilt as i64),
                edge_count: None,
                budget_exceeded: false,
                capability_mask: FactCoverage::from_bits(mask_bits),
                built_at: String::new(),
            })?;
            let mut payload = DataflowPayload::empty();
            payload.has_cfg = unit_has_cfg;
            return Ok((true, payload));
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
        .ok_or_else(|| anyhow::anyhow!("file not found in DB: {file_id:?}"))?;

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
        return Err(types::StaleStructuralIndexError {
            file_id,
            file_path: file_info.path.clone(),
            db_hash: file_info.content_hash.clone(),
            disk_hash: content_hash,
        }
        .into()); // .into() converts to anyhow::Error while preserving downcast
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
        &(),
    )?;

    Ok(DataflowPayload {
        data_nodes: facts.data_nodes,
        dataflow_edges: facts.dataflow_edges,
        bindings: facts.bindings,
        binding_uses: facts.binding_uses,
        cfg_nodes: facts.cfg_nodes,
        cfg_edges: facts.cfg_edges,
        budget_exceeded: facts.budget_exceeded,
        has_cfg: false,
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
        .filter(|cn| unit.symbol_id == Some(cn.function_id))
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
        .filter(|bu| bu.binding_id.is_some_and(|bid| binding_ids.contains(&bid)))
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
        has_cfg: false,
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
            Language::ArkTS,
            Language::Cangjie,
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

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::Language;

    // ------------------------------------------------------------------
    // Frontend cache regression tests — ensure newly added languages
    // (ArkTS, Cangjie) are included in the lazy frontend process cache.
    // ------------------------------------------------------------------

    #[test]
    fn test_cached_frontend_includes_arkts() {
        #[cfg(feature = "arkts")]
        {
            let fe = super::get_cached_frontend(Language::ArkTS);
            assert!(
                fe.is_some(),
                "ArkTS frontend should be cached when arkts feature is enabled"
            );
            assert_eq!(fe.unwrap().language(), Language::ArkTS);
        }
        #[cfg(not(feature = "arkts"))]
        {
            let fe = super::get_cached_frontend(Language::ArkTS);
            assert!(
                fe.is_none(),
                "ArkTS frontend should NOT be cached when arkts feature is disabled"
            );
        }
    }

    #[test]
    fn test_cached_frontend_includes_cangjie() {
        #[cfg(feature = "cangjie")]
        {
            let fe = super::get_cached_frontend(Language::Cangjie);
            assert!(
                fe.is_some(),
                "Cangjie frontend should be cached when cangjie feature is enabled"
            );
            assert_eq!(fe.unwrap().language(), Language::Cangjie);
        }
        #[cfg(not(feature = "cangjie"))]
        {
            let fe = super::get_cached_frontend(Language::Cangjie);
            assert!(
                fe.is_none(),
                "Cangjie frontend should NOT be cached when cangjie feature is disabled"
            );
        }
    }

    #[test]
    fn test_cached_frontend_typescript_available() {
        #[cfg(feature = "typescript")]
        {
            let fe = super::get_cached_frontend(Language::TypeScript);
            assert!(
                fe.is_some(),
                "TypeScript frontend should be cached when typescript feature is enabled"
            );
            assert_eq!(fe.unwrap().language(), Language::TypeScript);
        }
        #[cfg(not(feature = "typescript"))]
        {
            let fe = super::get_cached_frontend(Language::TypeScript);
            assert!(
                fe.is_none(),
                "TypeScript frontend should NOT be cached when typescript feature is disabled"
            );
        }
    }

    // Helper that mirrors the mask computation in LazyDataflowLoader::ensure
    // (lines 206–212).  Extracted here to make the regression test self-
    // checking without requiring a full DB + extraction pipeline.
    fn compute_unit_mask(cfg_supported: bool, has_cfg_nodes: bool) -> FactCoverage {
        let mut bits = FactCoverage::MANIFEST
            | FactCoverage::STRUCTURAL
            | FactCoverage::CALL_EDGES
            | FactCoverage::DATAFLOW;
        if cfg_supported && has_cfg_nodes {
            bits |= FactCoverage::CFG;
        }
        FactCoverage::from_bits(bits)
    }

    // ------------------------------------------------------------------
    // Profile-level checks — CFG support per language
    // ------------------------------------------------------------------

    #[test]
    fn php_profile_cfg_unsupported() {
        let profile = LanguageCapabilityProfile::for_language(Language::Php);
        let cfg_support = profile.features.cfg.is_supported();
        assert!(!cfg_support, "PHP profile must report CFG as unsupported");
    }

    #[test]
    fn ruby_profile_cfg_supported() {
        let profile = LanguageCapabilityProfile::for_language(Language::Ruby);
        let cfg_support = profile.features.cfg.is_supported();
        assert!(cfg_support, "Ruby profile must report CFG as supported");
    }

    #[test]
    fn typescript_profile_cfg_supported() {
        let profile = LanguageCapabilityProfile::for_language(Language::TypeScript);
        let cfg_support = profile.features.cfg.is_supported();
        assert!(
            cfg_support,
            "TypeScript profile must report CFG as supported"
        );
    }

    // ------------------------------------------------------------------
    // Mask-computation logic — regression for the lazy CFG bit fix
    // ------------------------------------------------------------------

    #[test]
    fn mask_no_cfg_when_language_unsupported() {
        // Even with CFG nodes present, the CFG bit must NOT be set when the
        // language does not support CFG (the pre-fix behaviour was to
        // unconditionally set it).
        let mask = compute_unit_mask(
            /* cfg_supported */ false, /* has_cfg_nodes */ true,
        );
        assert!(
            !mask.has(FactCoverage::CFG),
            "CFG bit must NOT be set for CFG-unsupported languages"
        );
        assert!(
            mask.has(FactCoverage::DATAFLOW),
            "DATAFLOW bit must still be set"
        );
    }

    #[test]
    fn mask_no_cfg_when_no_nodes_even_if_supported() {
        // Even when the language supports CFG, the bit must NOT be set when
        // the unit produced zero CFG nodes (e.g. an empty function body).
        let mask = compute_unit_mask(
            /* cfg_supported */ true, /* has_cfg_nodes */ false,
        );
        assert!(
            !mask.has(FactCoverage::CFG),
            "CFG bit must NOT be set when no CFG nodes were produced"
        );
        assert!(
            mask.has(FactCoverage::DATAFLOW),
            "DATAFLOW bit must still be set"
        );
    }

    #[test]
    fn mask_sets_cfg_when_supported_and_nodes_present() {
        // The normal happy path: CFG-supported language + actual CFG nodes.
        let mask = compute_unit_mask(/* cfg_supported */ true, /* has_cfg_nodes */ true);
        assert!(
            mask.has(FactCoverage::CFG),
            "CFG bit must be set when language supports CFG and nodes are present"
        );
        assert!(
            mask.has(FactCoverage::DATAFLOW),
            "DATAFLOW bit must also be set"
        );
    }

    #[test]
    fn mask_always_has_dataflow_manifest_structural_call_edges() {
        // The base bits are unconditional — they represent capabilities that
        // are always produced by a successful lazy dataflow build.
        let mask = compute_unit_mask(
            /* cfg_supported */ false, /* has_cfg_nodes */ false,
        );
        assert!(mask.has(FactCoverage::MANIFEST));
        assert!(mask.has(FactCoverage::STRUCTURAL));
        assert!(mask.has(FactCoverage::CALL_EDGES));
        assert!(mask.has(FactCoverage::DATAFLOW));
    }
}
