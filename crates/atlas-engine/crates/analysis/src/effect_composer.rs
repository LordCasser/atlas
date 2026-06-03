//! Effect Composer: decomposes single CfgNode statements into multiple SemanticEffect elements.
//!
//! Core algorithm:
//! 1. Walk each CfgNode that has effect_kind == Some(Call) or is a statement node.
//! 2. For Call nodes: classify the callee via OwnershipContract to get Alloc/Free effects.
//! 3. For Assign/FieldStore nodes: trace the DataFlow edges backward to find the ultimate
//!    value source (CallReturn or Param), then classify that source.
//! 4. Produce a vec of SemanticEffect with proper PlaceRef/ValueSource attribution.
//!
//! This module supersedes the former `branch_diff_df.rs` (removed in Phase 4 cleanup).
//! Uses the same range-overlap matching and DFS-based backward tracing with cycle detection
//! and max-depth limits, but produces the new multi-effect `SemanticEffect` vector instead
//! of the legacy single-`EnrichedFieldEffect` per dataflow node.

use std::collections::{HashMap, HashSet};

use types::cfg::CfgNode;
use types::dataflow::{DataFlowEdge, DataNode};
use types::effects::*;
use types::enums::{CallContext, DataFlowKind, DataNodeKind};
use types::ids::{CfgNodeId, DataNodeId, EffectId};

use super::cfg_graph::CfgGraph;
use super::scope_exit::run_scope_exit_pass;

// ---------------------------------------------------------------------------
// EffectComposition — the output of compose_effects
// ---------------------------------------------------------------------------

/// Result of composing effects for a function.
#[derive(Debug, Clone, Default)]
pub struct EffectComposition {
    /// CfgNodeId → resolved semantic effects
    pub node_effects: HashMap<CfgNodeId, Vec<SemanticEffect>>,
    /// Field path → summary of all writes/frees across the function
    pub transfer_graph: TransferGraph,
}

// ---------------------------------------------------------------------------
// TransferGraph — per-function summary of field writes and frees
// ---------------------------------------------------------------------------

/// Per-function summary: which values reach which fields.
#[derive(Debug, Clone, Default)]
pub struct TransferGraph {
    pub field_writes: HashMap<String, Vec<FieldWriteRecord>>,
    pub field_frees: HashMap<String, Vec<FieldFreeRecord>>,
}

#[derive(Debug, Clone)]
pub struct FieldWriteRecord {
    pub value_source: ValueSource,
    pub confidence: f64,
    pub node_line: u32,
}

#[derive(Debug, Clone)]
pub struct FieldFreeRecord {
    pub callee: String,
    pub node_line: u32,
}

impl TransferGraph {
    /// All fields that are written or freed in this function.
    pub fn touched_fields(&self) -> HashSet<&str> {
        let mut fields: HashSet<&str> = HashSet::new();
        for k in self.field_writes.keys() {
            fields.insert(k.as_str());
        }
        for k in self.field_frees.keys() {
            fields.insert(k.as_str());
        }
        fields
    }

    /// Get all writes to a specific field.
    pub fn writes_to(&self, field: &str) -> &[FieldWriteRecord] {
        self.field_writes
            .get(field)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get all frees of a specific field.
    pub fn frees_of(&self, field: &str) -> &[FieldFreeRecord] {
        self.field_frees
            .get(field)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Are there both writes and frees for this field? (high-interest pattern)
    pub fn has_allocate_free_pattern(&self, field: &str) -> bool {
        self.field_writes.contains_key(field) && self.field_frees.contains_key(field)
    }
}

// ---------------------------------------------------------------------------
// DataFlowIndex — pre-built indexes for fast lookups
// ---------------------------------------------------------------------------

/// Internal snapshot of the DataFlow graph with pre-built indexes.
/// Mirrors the index-building pattern in `branch_diff_df.rs`.
#[derive(Debug)]
struct DfIndex<'a> {
    nodes: HashMap<DataNodeId, &'a DataNode>,
    #[allow(dead_code)]
    source_edges: HashMap<DataNodeId, Vec<(DataFlowKind, DataNodeId)>>,
    target_edges: HashMap<DataNodeId, Vec<(DataFlowKind, DataNodeId)>>,
    range_index: HashMap<(u32, u32), Vec<&'a DataNode>>,
}

impl<'a> DfIndex<'a> {
    fn build(data_nodes: &'a [DataNode], edges: &'a [DataFlowEdge]) -> Self {
        let nodes: HashMap<DataNodeId, &DataNode> = data_nodes.iter().map(|n| (n.id, n)).collect();

        let mut source_edges: HashMap<DataNodeId, Vec<(DataFlowKind, DataNodeId)>> = HashMap::new();
        let mut target_edges: HashMap<DataNodeId, Vec<(DataFlowKind, DataNodeId)>> = HashMap::new();
        for e in edges {
            source_edges
                .entry(e.source)
                .or_default()
                .push((e.kind, e.target));
            target_edges
                .entry(e.target)
                .or_default()
                .push((e.kind, e.source));
        }

        let mut range_index: HashMap<(u32, u32), Vec<&DataNode>> = HashMap::new();
        for node in data_nodes {
            let key = (node.range.start_byte, node.range.end_byte);
            range_index.entry(key).or_default().push(node);
        }

        Self {
            nodes,
            source_edges,
            target_edges,
            range_index,
        }
    }
}

// ---------------------------------------------------------------------------
// compose_effects — main entry point
// ---------------------------------------------------------------------------

/// Compose semantic effects for an entire function's CFG.
///
/// # Arguments
/// * `cfg` - The CFG for one function (pre-built from CfgNode + CfgEdge slices).
/// * `data_nodes` - All DataNodes for the same function.
/// * `dataflow_edges` - All DataFlowEdges for the same function.
/// * `contract` - Language-specific ownership contract for classifying alloc/free.
pub fn compose_effects(
    cfg: &CfgGraph,
    data_nodes: &[DataNode],
    dataflow_edges: &[DataFlowEdge],
    contract: &dyn OwnershipContract,
) -> EffectComposition {
    if cfg.nodes.is_empty() {
        return EffectComposition {
            node_effects: HashMap::new(),
            transfer_graph: TransferGraph::default(),
        };
    }

    let dfi = DfIndex::build(data_nodes, dataflow_edges);
    let mut node_effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();

    for (node_id, node) in &cfg.nodes {
        // Skip virtual Entry/Exit nodes
        if node.stmt_range.start_byte == 0 && node.stmt_range.end_byte == 0 {
            continue;
        }

        let mut effects = Vec::new();

        // Find overlapping DataNodes to discover calls and store patterns
        let overlapping = find_overlapping_data_nodes(&node.stmt_range, &dfi);

        // Collect CallTarget DataNodes with candidate callee identities.
        // For each CallTarget we try access_path first (receiver-qualified
        // canonical path, e.g. "obj.close"), then fall back to name
        // (terminal method name, e.g. "close").  The first candidate that
        // matches any resource rule wins; subsequent candidates are skipped.
        let call_targets: Vec<(&DataNode, Vec<&str>)> = overlapping
            .iter()
            .filter(|dn| dn.kind == DataNodeKind::CallTarget)
            .filter_map(|dn| {
                let mut candidates = Vec::new();
                if let Some(ap) = dn.access_path.as_deref() {
                    if !ap.is_empty() {
                        candidates.push(ap);
                    }
                }
                if let Some(n) = dn.name.as_deref() {
                    if !n.is_empty() {
                        candidates.push(n);
                    }
                }
                if candidates.is_empty() {
                    None
                } else {
                    Some((*dn, candidates))
                }
            })
            .collect();

        for (_call_target, candidates) in &call_targets {
            for candidate in candidates {
                let mut hit_any = false;

                // Step 1a: Check if this call allocates a resource
                if let Some(return_contract) = contract.classify_return(candidate) {
                    match return_contract {
                        ReturnContract::NewOwned | ReturnContract::MaybeOwned => {
                            let target = find_return_receiver(node_id, &dfi);
                            let confidence = match return_contract {
                                ReturnContract::NewOwned => 0.85,
                                ReturnContract::MaybeOwned => 0.6,
                                _ => 0.85,
                            };
                            let eligible = contract.eligible_for_implicit_cleanup(candidate);
                            let mut eff = make_effect(
                                node_id,
                                effects.len() as u32,
                                SemanticEffectKind::Alloc {
                                    target,
                                    callee: (*candidate).to_string(),
                                },
                                confidence,
                            );
                            eff.eligible_for_implicit_cleanup = Some(eligible);
                            effects.push(eff);
                            hit_any = true;
                        }
                        _ => {}
                    }
                }

                // Step 1b: Check if this call frees a resource
                if let Some(mut cc) = contract.classify_consumption(candidate) {
                    // Go defer: set Deferred consumption style
                    if node.call_context == CallContext::GoDefer {
                        cc.style = ConsumptionStyle::Deferred;
                    }
                    let mut free_effects =
                        resolve_free_effect(node_id, candidate, &cc, &node.stmt_range, &dfi);
                    // Propagate consumption style to each free effect
                    for eff in &mut free_effects {
                        eff.consumption_style = Some(cc.style.clone());
                    }
                    effects.extend(free_effects);
                    hit_any = true;
                }

                // Step 1c: Check if this call causes resource escape
                if let Some(escape_target) = contract.classify_escape(candidate, node.call_context) {
                    let source = ValueSource::CallReturn {
                        callee: (*candidate).to_string(),
                    };
                    effects.push(make_effect(
                        node_id,
                        effects.len() as u32,
                        SemanticEffectKind::Escape {
                            value: source,
                            to: escape_target,
                        },
                        0.85,
                    ));
                    hit_any = true;
                }

                if hit_any {
                    break; // first candidate for this CallTarget that hits any rule wins
                }
            }
        }

        // Step 2: Resolve store effects via DataFlow (always run)
        let store_effects = resolve_store_effect(node_id, node, &dfi, contract);
        effects.extend(store_effects);

        // Step 2.5: Per-node React cleanup — mark all Free effects on this
        // node as Deferred when the node belongs to a React useEffect cleanup
        // arrow body (annotated by the CFG builder).
        if node.call_context == CallContext::ReactEffectCleanup {
            for eff in &mut effects {
                if matches!(eff.kind, SemanticEffectKind::Free { .. }) {
                    eff.consumption_style = Some(ConsumptionStyle::Deferred);
                    eff.description = Some("React effect cleanup return".to_string());
                }
            }
        }

        if !effects.is_empty() {
            node_effects.insert(*node_id, effects);
        }
    }

    // Step 2.6: Fallback for when CFG builder did not annotate cleanup scope
    // (e.g., arrow functions whose CFG is inlined in the parent function).
    // If ANY CFG node has ReactEffectCleanup context, per-node logic already
    // handled Deferred marking.  If NO node has it, but a CleanupReturn
    // DataNode exists, fall back to function-wide Deferred.
    let any_node_has_react_ctx = cfg
        .nodes
        .values()
        .any(|n| n.call_context == CallContext::ReactEffectCleanup);
    if !any_node_has_react_ctx {
        let has_cleanup_return = data_nodes
            .iter()
            .any(|dn| matches!(dn.kind, DataNodeKind::CleanupReturn));
        if has_cleanup_return {
            for (_node_id, node_effects) in node_effects.iter_mut() {
                for eff in node_effects.iter_mut() {
                    if matches!(eff.kind, SemanticEffectKind::Free { .. }) {
                        eff.consumption_style = Some(ConsumptionStyle::Deferred);
                        eff.description = Some("React effect cleanup return (function-wide fallback)".to_string());
                    }
                }
            }
        }
    }

    // Step 3: Run scope-exit post-pass (implicit Drop for Rust, Python with, etc.)
    // Always run — eligibility is gated per-Alloc via `eligible_for_implicit_cleanup`
    // and per-context (PythonWith / JavaTryWith / CSharpUsing).
    run_scope_exit_pass(&mut node_effects, cfg);

    // Step 4: Build TransferGraph
    let transfer_graph = build_transfer_graph(&node_effects, cfg);

    EffectComposition {
        node_effects,
        transfer_graph,
    }
}

// ---------------------------------------------------------------------------
// resolve_free_effect — trace which field/local is freed by a consumer call
// ---------------------------------------------------------------------------

fn resolve_free_effect(
    cfg_node_id: &CfgNodeId,
    callee_name: &str,
    cc: &ConsumptionContract,
    stmt_range: &types::structs::TextRange,
    dfi: &DfIndex,
) -> Vec<SemanticEffect> {
    let mut effects = Vec::new();

    // Find overlapping DataNodes to locate CallArg nodes for this call
    let overlapping = find_overlapping_data_nodes(stmt_range, dfi);
    if overlapping.is_empty() {
        return effects;
    }

    // For each CallArg in the overlapping set, trace backward to find the freed place
    for dn in &overlapping {
        if dn.kind != DataNodeKind::CallArg {
            continue;
        }

        // Check that this CallArg's callsite effectively matches this call.
        // callee_name may come from access_path or name (see candidate list
        // in compose_effects); compare against both fields.
        let callee_match = overlapping.iter().any(|n| {
            n.kind == DataNodeKind::CallTarget
                && (n.access_path.as_deref() == Some(callee_name)
                    || n.name.as_deref() == Some(callee_name))
        });
        if !callee_match {
            continue;
        }

        // Trace backward from the CallArg to find the ultimate place
        if let Some(place) = trace_back_to_place(dn.id, dfi, 10) {
            effects.push(make_effect(
                cfg_node_id,
                effects.len() as u32,
                SemanticEffectKind::Free {
                    place,
                    callee: callee_name.to_string(),
                },
                cc.confidence,
            ));
        }
    }

    // If no specific place was found, produce a generic Free effect
    if effects.is_empty() {
        effects.push(make_effect(
            cfg_node_id,
            0,
            SemanticEffectKind::Free {
                place: PlaceRef::Indeterminate,
                callee: callee_name.to_string(),
            },
            cc.confidence * 0.5,
        ));
    }

    effects
}

// ---------------------------------------------------------------------------
// resolve_store_effect — trace field writes and nullifications
// ---------------------------------------------------------------------------

fn resolve_store_effect(
    cfg_node_id: &CfgNodeId,
    _cfg_node: &CfgNode,
    dfi: &DfIndex,
    contract: &dyn OwnershipContract,
) -> Vec<SemanticEffect> {
    let mut effects = Vec::new();

    let overlapping = find_overlapping_data_nodes(&_cfg_node.stmt_range, dfi);
    if overlapping.is_empty() {
        return effects;
    }

    for dn in &overlapping {
        // Case A: Field node with incoming FieldStore
        if dn.kind == DataNodeKind::Field {
            let incoming = match dfi.target_edges.get(&dn.id) {
                Some(edges) => edges,
                None => continue,
            };

            for (kind, source_id) in incoming {
                if *kind != DataFlowKind::FieldStore {
                    continue;
                }

                let value_source = trace_to_value_source(*source_id, dfi, contract, 10);

                let field_path = dn
                    .access_path
                    .clone()
                    .or_else(|| dn.name.clone())
                    .unwrap_or_else(|| "?".to_string());

                // Determine confidence
                let confidence = match &value_source {
                    ValueSource::CallReturn { .. } => 0.9,
                    ValueSource::Param { .. } => 0.7,
                    ValueSource::Local { .. } => 0.6,
                    ValueSource::LiteralNull => 0.85,
                    ValueSource::Unknown => 0.3,
                };

                if matches!(&value_source, ValueSource::LiteralNull) {
                    effects.push(make_effect(
                        cfg_node_id,
                        effects.len() as u32,
                        SemanticEffectKind::Nullify {
                            place: PlaceRef::Field {
                                path: field_path.clone(),
                            },
                        },
                        confidence,
                    ));
                } else {
                    effects.push(make_effect(
                        cfg_node_id,
                        effects.len() as u32,
                        SemanticEffectKind::Store {
                            dst: PlaceRef::Field {
                                path: field_path.clone(),
                            },
                            src: value_source.clone(),
                        },
                        confidence,
                    ));

                    // If the value source is a CallReturn that is an alloc, also emit
                    // an Alloc effect chained through a local.
                    if let ValueSource::CallReturn { callee } = &value_source {
                        if let Some(rc) = contract.classify_return(callee) {
                            if matches!(rc, ReturnContract::NewOwned | ReturnContract::MaybeOwned) {
                                // Find intermediate local if any
                                let local_name = find_intermediate_local_name(*source_id, dfi);
                                let confidence = match rc {
                                    ReturnContract::NewOwned => 0.85,
                                    ReturnContract::MaybeOwned => 0.6,
                                    _ => 0.85,
                                };
                                let eligible = contract.eligible_for_implicit_cleanup(callee);
                                let mut eff = make_effect(
                                    cfg_node_id,
                                    effects.len() as u32,
                                    SemanticEffectKind::Alloc {
                                        target: PlaceRef::Local { name: local_name },
                                        callee: callee.clone(),
                                    },
                                    confidence,
                                );
                                eff.eligible_for_implicit_cleanup = Some(eligible);
                                effects.push(eff);
                            }
                        }
                    }
                }
            }
        }
    }

    effects
}

// ---------------------------------------------------------------------------
// build_transfer_graph — aggregate FieldWrite / FieldFree records
// ---------------------------------------------------------------------------

fn build_transfer_graph(
    node_effects: &HashMap<CfgNodeId, Vec<SemanticEffect>>,
    cfg: &CfgGraph,
) -> TransferGraph {
    let mut field_writes: HashMap<String, Vec<FieldWriteRecord>> = HashMap::new();
    let mut field_frees: HashMap<String, Vec<FieldFreeRecord>> = HashMap::new();

    for (node_id, effects) in node_effects {
        let cfg_node = match cfg.nodes.get(node_id) {
            Some(n) => n,
            None => continue,
        };
        let node_line = cfg_node.stmt_range.start_line;

        for effect in effects {
            match &effect.kind {
                SemanticEffectKind::Store {
                    dst: PlaceRef::Field { path },
                    src,
                    ..
                } => {
                    field_writes
                        .entry(path.clone())
                        .or_default()
                        .push(FieldWriteRecord {
                            value_source: src.clone(),
                            confidence: effect.confidence,
                            node_line,
                        });
                }
                SemanticEffectKind::Free {
                    place: PlaceRef::Field { path },
                    callee,
                    ..
                } => {
                    field_frees
                        .entry(path.clone())
                        .or_default()
                        .push(FieldFreeRecord {
                            callee: callee.clone(),
                            node_line,
                        });
                }
                _ => {}
            }
        }
    }

    TransferGraph {
        field_writes,
        field_frees,
    }
}

// ---------------------------------------------------------------------------
// DataFlow tracing helpers
// ---------------------------------------------------------------------------

/// Find DataNodes whose range overlaps with a CFG node's stmt_range.
fn find_overlapping_data_nodes<'a>(
    stmt_range: &types::structs::TextRange,
    dfi: &'a DfIndex,
) -> Vec<&'a DataNode> {
    let mut overlapping: Vec<&DataNode> = Vec::new();

    for ((start_byte, end_byte), nodes) in &dfi.range_index {
        if start_byte < &stmt_range.end_byte && end_byte > &stmt_range.start_byte {
            overlapping.extend(nodes.iter().copied());
        }
    }

    // Avoid deduplication issues: keep unique by DataNodeId
    let mut seen = HashSet::new();
    overlapping.retain(|n| seen.insert(n.id));

    overlapping
}

/// Trace backward from a DataNode to find the ultimate place (Field or Local)
/// that a value originates from.  Used by free-effect resolution.
///
/// Common pattern: `free(ptr)` where ptr traces back through
/// Assign ← FieldLoad ← Field.
fn trace_back_to_place(start: DataNodeId, dfi: &DfIndex, max_depth: usize) -> Option<PlaceRef> {
    let mut current = start;
    let mut visited = HashSet::new();

    for _ in 0..max_depth {
        if !visited.insert(current) {
            return None; // cycle detected
        }

        let dn = dfi.nodes.get(&current)?;

        match dn.kind {
            DataNodeKind::Field => {
                let path = dn
                    .access_path
                    .clone()
                    .or_else(|| dn.name.clone())
                    .unwrap_or_else(|| "?".to_string());
                return Some(PlaceRef::Field { path });
            }
            DataNodeKind::Local | DataNodeKind::VariableUse => {
                let incoming = dfi.target_edges.get(&current)?;
                // Follow Assign or FieldLoad edges backward
                let prev = incoming
                    .iter()
                    .find(|(k, _)| *k == DataFlowKind::Assign || *k == DataFlowKind::FieldLoad)
                    .map(|(_, id)| *id)?;
                current = prev;
                continue;
            }
            DataNodeKind::CallReturn => {
                // Try to step through to see if the call return traces further
                let incoming = dfi.target_edges.get(&current)?;
                if let Some((_, next)) = incoming.first() {
                    current = *next;
                    continue;
                }
                return None;
            }
            DataNodeKind::Literal => {
                // Cannot trace through a literal to a place
                return None;
            }
            DataNodeKind::Parameter => {
                let name = dn.name.clone().unwrap_or_else(|| "?".to_string());
                return Some(PlaceRef::Local { name });
            }
            _ => {
                // For unknown types, try any incoming edge
                let incoming = dfi.target_edges.get(&current)?;
                if let Some((_, next)) = incoming.first() {
                    current = *next;
                    continue;
                }
                return None;
            }
        }
    }

    None
}

/// Trace backward from a source DataNode to find the ultimate value source.
/// Follows Assign edges until reaching CallReturn, Parameter, Literal, or max depth.
fn trace_to_value_source(
    node_id: DataNodeId,
    dfi: &DfIndex,
    _contract: &dyn OwnershipContract,
    max_depth: usize,
) -> ValueSource {
    let mut current = node_id;
    let mut visited = HashSet::new();

    for _ in 0..max_depth {
        if !visited.insert(current) {
            return ValueSource::Unknown; // cycle detected
        }

        let dn = match dfi.nodes.get(&current) {
            Some(n) => n,
            None => return ValueSource::Unknown,
        };

        match dn.kind {
            DataNodeKind::CallReturn => {
                return ValueSource::CallReturn {
                    callee: dn
                        .access_path
                        .clone()
                        .or_else(|| dn.name.clone())
                        .unwrap_or_else(|| "?".to_string()),
                };
            }
            DataNodeKind::Parameter => {
                return ValueSource::Param {
                    name: dn.name.clone().unwrap_or_else(|| "?".to_string()),
                };
            }
            DataNodeKind::Literal => {
                return ValueSource::LiteralNull;
            }
            DataNodeKind::Local | DataNodeKind::VariableUse => {
                let incoming = match dfi.target_edges.get(&current) {
                    Some(edges) => edges,
                    None => return ValueSource::Unknown,
                };
                let assign_src = incoming
                    .iter()
                    .find(|(k, _)| *k == DataFlowKind::Assign)
                    .map(|(_, id)| *id);
                match assign_src {
                    Some(next) => {
                        current = next;
                        continue;
                    }
                    None => return ValueSource::Unknown,
                }
            }
            _ => {
                let incoming = match dfi.target_edges.get(&current) {
                    Some(edges) => edges,
                    None => return ValueSource::Unknown,
                };
                if let Some((_, next)) = incoming.first() {
                    current = *next;
                    continue;
                }
                return ValueSource::Unknown;
            }
        }
    }

    ValueSource::Unknown
}

/// Find the name of an intermediate local variable between a CallReturn and a FieldStore.
/// e.g., in `p = malloc(); data->field = p`, walking from the FieldStore source
/// through Assign edges reaches the Local `p`.
fn find_intermediate_local_name(source_id: DataNodeId, dfi: &DfIndex) -> String {
    // Walk one step backward from the FieldStore source
    let dn = match dfi.nodes.get(&source_id) {
        Some(n) => n,
        None => return "?".to_string(),
    };

    if matches!(dn.kind, DataNodeKind::Local | DataNodeKind::VariableUse) {
        return dn.name.clone().unwrap_or_else(|| "?".to_string());
    }

    // Try one more step
    if let Some(edges) = dfi.target_edges.get(&source_id) {
        if let Some((DataFlowKind::Assign, prev)) =
            edges.iter().find(|(k, _)| *k == DataFlowKind::Assign)
        {
            if let Some(prev_dn) = dfi.nodes.get(prev) {
                if matches!(
                    prev_dn.kind,
                    DataNodeKind::Local | DataNodeKind::VariableUse
                ) {
                    return prev_dn.name.clone().unwrap_or_else(|| "?".to_string());
                }
            }
        }
    }

    "?".to_string()
}

/// Find the local variable that receives a call return value.
/// Given the CfgNodeId of a Call node, find the DataNode that represents
/// the local variable target of the call's return value assignment.
fn find_return_receiver(_cfg_node_id: &CfgNodeId, _dfi: &DfIndex) -> PlaceRef {
    // The return value of a call like `p = malloc(N)` flows:
    // CallReturn → (Assign) → Local(p)
    // We look for a Local DataNode that has an incoming Assign edge
    // from a CallReturn.  Since we don't have a direct CallReturn node
    // reference here, we scan for matching CallReturn in overlapping nodes.
    //
    // For simplicity, return Indeterminate — the caller can refine this
    // in later phases.
    PlaceRef::Indeterminate
}

// ---------------------------------------------------------------------------
// make_effect — helper to construct a SemanticEffect with deterministic ID
// ---------------------------------------------------------------------------

pub(crate) fn make_effect(
    cfg_node_id: &CfgNodeId,
    order: u32,
    kind: SemanticEffectKind,
    confidence: f64,
) -> SemanticEffect {
    let kind_name = effect_kind_name(&kind);
    let id = EffectId::generate(cfg_node_id, order, kind_name);
    SemanticEffect {
        id,
        cfg_node_id: *cfg_node_id,
        order,
        kind,
        confidence,
        consumption_style: None,
        description: None,
        eligible_for_implicit_cleanup: None,
    }
}

/// Short string name for a SemanticEffectKind — used in EffectId generation.
pub(crate) fn effect_kind_name(kind: &SemanticEffectKind) -> &'static str {
    match kind {
        SemanticEffectKind::Alloc { .. } => "Alloc",
        SemanticEffectKind::Free { .. } => "Free",
        SemanticEffectKind::Store { .. } => "Store",
        SemanticEffectKind::Assign { .. } => "Assign",
        SemanticEffectKind::Call { .. } => "Call",
        SemanticEffectKind::Nullify { .. } => "Nullify",
        SemanticEffectKind::Return { .. } => "Return",
        SemanticEffectKind::Escape { .. } => "Escape",
    }
}
