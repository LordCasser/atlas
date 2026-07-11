//! Virtual edges for runtime dataflow tracing.
//!
//! When the backward slicer hits a function boundary (parameter node at the
//! top of a function, or call-return node with an associated [`Callsite`]),
//! it needs to "jump" across the call boundary to continue tracing into the
//! caller or callee.  These cross-boundary jumps are modelled as virtual
//! [`TraceEdge`]s provided by a [`TraceEdgeProvider`].
//!
//! ## Bridge types
//!
//! || Direction || From || To || When ||
//! || Backward (caller arg → callee param) || CallArg DataNode in caller || Parameter DataNode in callee || Slicer reaches a Parameter node with known callers ||
//! || Backward (callee return → caller result) || Return DataNode in callee || Expr/CallResult DataNode in caller || Slicer reaches a call-result node with known callee ||
//! || Backward (framework state write → reactive field read) || AppStorage value argument || `@StorageLink` / `@StorageProp` field access || Keys match ||
//!
//! The [`RuntimeEdgeProvider`] uses [`super::summary::FunctionSummary`] to
//! bridge call boundaries and ArkTS framework-managed state. When no summary
//! exists yet it falls back to direct callsite joins.

use db::TraceStore;
use types::dataflow::DataFlowEdge;
use types::enums::{DataFlowKind, DataNodeKind, Language, ReferenceKind, SymbolKind};
use types::ids::{DataNodeId, FileId, SymbolId};
use types::structs::Callsite;

// ---------------------------------------------------------------------------
// TraceEdge — a cross-boundary dataflow connection
// ---------------------------------------------------------------------------

/// A virtual edge that connects data nodes across semantic boundaries.
///
/// Unlike [`DataFlowEdge`] which stays within a single function, `TraceEdge`
/// bridges caller-callee transitions or framework-managed state channels.
#[derive(Debug, Clone)]
pub struct TraceEdge {
    /// Source of the data flow (caller-side for backward tracing).
    pub source_id: DataNodeId,
    /// Target of the data flow (callee-side for backward tracing).
    pub target_id: DataNodeId,
    /// Edge kind — typically `ArgToParam` or `ReturnToCall`.
    pub kind: DataFlowKind,
    /// Confidence (0.0–1.0).  Virtual edges have lower confidence than
    /// intra-procedural dataflow edges.
    pub confidence: f64,
    /// Human-readable provenance describing how this edge was inferred.
    pub provenance: String,
}

// ---------------------------------------------------------------------------
// TraceEdgeProvider trait
// ---------------------------------------------------------------------------

/// Provider of virtual runtime trace edges.
///
/// Implementations bridge the gap between intra-procedural dataflow and
/// semantic transitions that do not have a source-level assignment edge.
pub trait TraceEdgeProvider: Send + Sync {
    /// Return virtual edges whose **target** is the given node.  For backward
    /// tracing, these are edges *into* this node from across a call boundary.
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>>;
}

// ---------------------------------------------------------------------------
// RuntimeEdgeProvider — bridges using FunctionSummary
// ---------------------------------------------------------------------------

/// Bridges call boundaries and framework state using query-time DB joins.
///
/// ## Strategy (in priority order)
///
/// 1. **ArkTS reactive field** → match its decorator key to AppStorage writes.
/// 2. **Parameter node** → find callers via `callsites_by_callee`, match
///    each caller's call-arg DataNode to this parameter by arg_index.
/// 3. **CallReturn / Expr nodes with callsite_id** → find the callee
///    function, look up its summary, connect each ReturnFlow source back
///    to this call-result node.
/// 4. **Direct callsite join** — when summary is unavailable, match
///    caller call-arg DataNodes to callee parameters using the existing
///    `callsite_id` on DataNodes and the callsite's callee symbol.
pub struct RuntimeEdgeProvider;

impl TraceEdgeProvider for RuntimeEdgeProvider {
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let target_node = match store.get_data_node(target_id)? {
            Some(n) => n,
            None => return Ok(vec![]),
        };
        let mut runtime_edges = arkts_state_incoming(&target_node, store)?;

        // ── Phase 1: try CrossFunctionBridge (persisted summaries) ──
        let bridge_edges = match target_node.kind {
            DataNodeKind::Parameter => {
                crate::cross_function::CrossFunctionBridge::incoming_for_param(target_id, store)
                    .unwrap_or_default()
            }
            DataNodeKind::CallReturn | DataNodeKind::Expr => {
                crate::cross_function::CrossFunctionBridge::incoming_for_call_result(
                    target_id, store,
                )
                .unwrap_or_default()
            }
            _ => vec![],
        };

        if !bridge_edges.is_empty() {
            runtime_edges.extend(bridge_edges);
            return Ok(runtime_edges);
        }

        // ── Phase 2: runtime BFS join (Focus primary; Full when no summary) ──
        // Focus never runs summary phase, so this path is the designed cross-
        // function bridge for query-time materialize — not a legacy shim.
        let mut edges = runtime_edges;

        match target_node.kind {
            // ── Parameter: find direct + indirect callers ──
            DataNodeKind::Parameter => {
                let function_id = match &target_node.function_id {
                    Some(fid) => *fid,
                    None => return Ok(vec![]),
                };

                // Layer 1: direct callers
                let direct_callers = store.find_resolved_callsites_by_callee(&function_id)?;
                let param_index =
                    crate::cross_function::find_param_index(store, &function_id, target_id)?;

                for rc in &direct_callers {
                    let cs = &rc.callsite;
                    for (arg_idx, arg) in cs.args.iter().enumerate() {
                        let arg_dn_id = match &arg.data_node_id {
                            Some(dn_id) => dn_id,
                            None => continue,
                        };
                        if let Some(param_idx) = param_index {
                            if arg_idx == param_idx {
                                edges.push(TraceEdge {
                                    source_id: *arg_dn_id,
                                    target_id: *target_id,
                                    kind: DataFlowKind::ArgToParam,
                                    confidence: 0.67,
                                    provenance: format!(
                                        "direct caller arg[{}] at callsite {} → callee param[{}]",
                                        arg_idx,
                                        hex::encode(cs.id.as_bytes()),
                                        param_idx,
                                    ),
                                });
                            }
                        }
                    }
                }

                // Layer 3: nested call bridge.
                // For each direct caller arg at this parameter position,
                // if the arg is a call result (CallReturn/Expr with callsite_id),
                // bridge from the inner callee's return sources to this param.
                for rc in &direct_callers {
                    let cs = &rc.callsite;
                    if let Some(param_idx) = param_index {
                        for (arg_idx, arg) in cs.args.iter().enumerate() {
                            if arg_idx != param_idx {
                                continue;
                            }
                            let arg_dn_id = match &arg.data_node_id {
                                Some(dn_id) => dn_id,
                                None => continue,
                            };
                            // Check if this argument is a call result
                            if let Ok(Some(arg_dn)) = store.get_data_node(arg_dn_id) {
                                if arg_dn.kind == DataNodeKind::CallReturn
                                    || arg_dn.kind == DataNodeKind::Expr
                                {
                                    if let Some(inner_csid) = arg_dn.callsite_id {
                                        if let Ok(inner_rcs) =
                                            store.find_resolved_callsites_by_id(&inner_csid)
                                        {
                                            if let Some(inner_rc) = inner_rcs.first() {
                                                let inner_callee = &inner_rc.callee;
                                                if let Ok(inner_summary) =
                                                    crate::summary::SummaryBuilder::build(
                                                        store,
                                                        inner_callee,
                                                        None,
                                                    )
                                                {
                                                    for rf in &inner_summary.return_flows {
                                                        for src_id in &rf.sources {
                                                            edges.push(TraceEdge {
                                                                source_id: *src_id,
                                                                target_id: *target_id,
                                                                kind: DataFlowKind::ReturnToCall,
                                                                confidence: 0.55,
                                                                provenance: format!(
                                                                    "nested call return {} → outer param (via callsite {})",
                                                                    hex::encode(rf.return_id.as_bytes()),
                                                                    hex::encode(inner_csid.as_bytes()),
                                                                ),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Layer 2: indirect callers (recursive, up to depth 3)
                const MAX_INDIRECT_DEPTH: usize = 3;
                let indirect = find_indirect_callers(store, &function_id, MAX_INDIRECT_DEPTH);
                for (depth, _caller_sym_id, cs) in &indirect {
                    // Match args by position using the ORIGINAL callee's
                    // parameter index, not the indirect caller's param set.
                    if let Some(p_idx) = param_index {
                        for (arg_idx, arg) in cs.args.iter().enumerate() {
                            let arg_dn_id = match &arg.data_node_id {
                                Some(dn_id) => dn_id,
                                None => continue,
                            };
                            if arg_idx == p_idx {
                                let depth_penalty = 0.85_f64.powi(*depth as i32);
                                edges.push(TraceEdge {
                                    source_id: *arg_dn_id,
                                    target_id: *target_id,
                                    kind: DataFlowKind::ArgToParam,
                                    confidence: 0.67 * depth_penalty,
                                    provenance: format!(
                                        "indirect(depth={depth}) caller arg[{}] at callsite {} → param[{}]",
                                        arg_idx,
                                        hex::encode(cs.id.as_bytes()),
                                        p_idx,
                                    ),
                                });
                            }
                        }
                    }
                }
            }

            // ── CallReturn-like: find the callee and connect its returns ──
            DataNodeKind::CallReturn | DataNodeKind::Expr => {
                let callsite_id = match &target_node.callsite_id {
                    Some(csid) => csid,
                    None => return Ok(vec![]),
                };

                // Find the callsite → get the callee symbol
                let callee_sym_id =
                    match crate::cross_function::resolve_callsite_to_callee(store, callsite_id)? {
                        Some(sym) => sym,
                        None => return Ok(vec![]),
                    };

                // Try summary-based bridge first.
                // Pass None for function_range: SummaryBuilder uses all DataNodes
                // in the file and relies on graph connectivity for scoping.
                // (function_id is not reliably set on DataNodes, and callers
                // here don't have source-level function body ranges.)
                if let Ok(summary) =
                    crate::summary::SummaryBuilder::build(store, &callee_sym_id, None)
                {
                    for rf in &summary.return_flows {
                        for src_id in &rf.sources {
                            edges.push(TraceEdge {
                                source_id: *src_id,
                                target_id: *target_id,
                                kind: DataFlowKind::ReturnToCall,
                                confidence: rf.confidence * 0.85, // cross-boundary penalty
                                provenance: format!(
                                    "callee return {} → call site {} (summary bridge)",
                                    hex::encode(rf.return_id.as_bytes()),
                                    hex::encode(callsite_id.as_bytes()),
                                ),
                            });
                        }
                    }
                }
            }

            _ => {}
        }

        Ok(edges)
    }
}

fn arkts_state_incoming(
    target: &types::dataflow::DataNode,
    store: &dyn TraceStore,
) -> anyhow::Result<Vec<TraceEdge>> {
    if !matches!(
        target.kind,
        DataNodeKind::Field | DataNodeKind::CallArg | DataNodeKind::Expr
    ) {
        return Ok(vec![]);
    }
    let Some(function_id) = target.function_id else {
        return Ok(vec![]);
    };
    if store
        .find_symbol_by_id(&function_id)?
        .is_none_or(|function| function.language != Language::ArkTS)
    {
        return Ok(vec![]);
    }

    let field_reads = if target.kind == DataNodeKind::Field {
        vec![target.clone()]
    } else {
        store
            .find_data_nodes_by_file(&target.file_id)?
            .into_iter()
            .filter(|node| {
                node.kind == DataNodeKind::Field
                    && node.function_id == target.function_id
                    && node.range.start_byte >= target.range.start_byte
                    && node.range.end_byte <= target.range.end_byte
            })
            .collect()
    };

    let mut edges = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for field_read in field_reads {
        for edge in arkts_state_for_field(&field_read, target.id, store)? {
            if seen.insert(edge.source_id) {
                edges.push(edge);
            }
        }
    }
    Ok(edges)
}

/// Find materialized writer functions for reactive AppStorage keys declared in a file.
///
/// Focus uses this after its state-channel closure has added writer files, so
/// their function-local dataflow can be built before the runtime edge query.
pub fn arkts_state_writer_functions_for_file(
    file_id: &FileId,
    store: &dyn TraceStore,
) -> anyhow::Result<Vec<SymbolId>> {
    let references = store.find_references_by_file(file_id)?;
    let reactive_keys: std::collections::HashSet<_> = store
        .find_callsites_by_file(file_id)?
        .into_iter()
        .filter_map(|callsite| {
            let reference_id = callsite.reference_id?;
            let reference = references
                .iter()
                .find(|reference| reference.id == reference_id)?;
            (reference.kind == ReferenceKind::Call
                && matches!(reference.name.as_str(), "StorageLink" | "StorageProp"))
            .then(|| callsite.args.first())
            .flatten()
            .map(|arg| canonical_state_key(&arg.value))
        })
        .collect();
    if reactive_keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut functions = Vec::new();
    for setter in ["setOrCreate", "set"] {
        for callsite in
            store.find_callsites_by_name_and_receiver(setter, "AppStorage", Language::ArkTS)?
        {
            if callsite.args.len() >= 2
                && callsite
                    .args
                    .first()
                    .map(|arg| canonical_state_key(&arg.value))
                    .is_some_and(|key| reactive_keys.contains(&key))
            {
                functions.push(callsite.caller);
            }
        }
    }
    functions.sort_by_key(SymbolId::to_hex);
    functions.dedup();
    Ok(functions)
}

fn arkts_state_for_field(
    field_read: &types::dataflow::DataNode,
    target_id: DataNodeId,
    store: &dyn TraceStore,
) -> anyhow::Result<Vec<TraceEdge>> {
    let Some(field_name) = field_read.name.as_deref() else {
        return Ok(vec![]);
    };
    if field_read
        .access_path
        .as_deref()
        .and_then(|path| path.strip_prefix("this."))
        != Some(field_name)
    {
        return Ok(vec![]);
    }

    let symbols = store.find_symbols_by_file(&field_read.file_id)?;
    let Some(owner) = field_read
        .function_id
        .and_then(|function_id| symbols.iter().find(|symbol| symbol.id == function_id))
        .and_then(|function| function.container)
    else {
        return Ok(vec![]);
    };
    let field = symbols.iter().find(|symbol| {
        symbol.kind == SymbolKind::Field
            && symbol.name == field_name
            && symbol.container == Some(owner)
    });
    let Some(field) = field.filter(|field| field.language == Language::ArkTS) else {
        return Ok(vec![]);
    };
    let Some(container_id) = field.container else {
        return Ok(vec![]);
    };

    let previous_field_start = symbols
        .iter()
        .filter(|symbol| {
            symbol.kind == SymbolKind::Field
                && symbol.container == Some(container_id)
                && symbol.range.start_byte < field.range.start_byte
        })
        .map(|symbol| symbol.range.start_byte)
        .max()
        .unwrap_or(0);
    let references = store.find_references_by_file(&field_read.file_id)?;
    let callsites = store.find_callsites_by_file(&field_read.file_id)?;
    let annotation = callsites
        .iter()
        .filter(|callsite| {
            callsite.caller == container_id
                && callsite.range.start_byte > previous_field_start
                && callsite.range.end_byte <= field.range.start_byte
        })
        .filter_map(|callsite| {
            let reference_id = callsite.reference_id?;
            let reference = references
                .iter()
                .find(|reference| reference.id == reference_id)?;
            (reference.kind == ReferenceKind::Call
                && matches!(reference.name.as_str(), "StorageLink" | "StorageProp"))
            .then_some(callsite)
        })
        .max_by_key(|callsite| callsite.range.start_byte);
    let Some(annotation) = annotation else {
        return Ok(vec![]);
    };
    let Some((key, key_is_literal)) = annotation
        .args
        .first()
        .map(|arg| canonical_state_key(&arg.value))
    else {
        return Ok(vec![]);
    };

    let mut seen = std::collections::HashSet::new();
    let mut edges = Vec::new();
    for setter in ["setOrCreate", "set"] {
        for callsite in
            store.find_callsites_by_name_and_receiver(setter, "AppStorage", Language::ArkTS)?
        {
            if callsite.args.len() < 2 {
                continue;
            }
            let (setter_key, setter_key_is_literal) = canonical_state_key(&callsite.args[0].value);
            if setter_key != key || setter_key_is_literal != key_is_literal {
                continue;
            }
            let Some(source_id) = callsite.args[1].data_node_id else {
                continue;
            };
            if !seen.insert(source_id) {
                continue;
            }
            edges.push(TraceEdge {
                source_id,
                target_id,
                kind: DataFlowKind::StateFlow,
                confidence: if key_is_literal { 0.72 } else { 0.60 },
                provenance: format!(
                    "ArkTS AppStorage.{setter}({key}) → reactive field {}",
                    field.qualified_name
                ),
            });
        }
    }
    Ok(edges)
}

fn canonical_state_key(value: &str) -> (String, bool) {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if matches!(
            (bytes[0], bytes[trimmed.len() - 1]),
            (b'\'', b'\'') | (b'"', b'"')
        ) {
            return (trimmed[1..trimmed.len() - 1].to_string(), true);
        }
    }
    (
        trimmed.chars().filter(|ch| !ch.is_whitespace()).collect(),
        false,
    )
}

// ---------------------------------------------------------------------------
// Helper — convert TraceEdge → DataFlowEdge for slicer compatibility
// ---------------------------------------------------------------------------

impl TraceEdge {
    /// Convert this virtual edge into a synthetic [`DataFlowEdge`] so the
    /// slicer can process it alongside real intra-procedural edges.
    pub fn to_dataflow_edge(&self) -> DataFlowEdge {
        DataFlowEdge {
            id: types::ids::DataFlowEdgeId::generate(
                &self.source_id,
                &self.target_id,
                self.kind.as_str(),
            ),
            source: self.source_id,
            target: self.target_id,
            kind: self.kind,
            location: types::structs::TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
            confidence: self.confidence,
        }
    }
}

/// Recursively find indirect callers of a function through the call graph.
///
/// BFS from the given function through all callers.  Returns tuples of
/// (depth, caller_symbol_id, callsite).  Depth 1 = direct caller, 2 = caller
/// of caller, etc.  Bounded by `max_depth`.
fn find_indirect_callers(
    store: &dyn TraceStore,
    function_id: &SymbolId,
    max_depth: usize,
) -> Vec<(usize, SymbolId, Callsite)> {
    let mut results = Vec::new();
    let mut visited: std::collections::HashSet<SymbolId> = std::collections::HashSet::new();
    visited.insert(*function_id);

    // BFS queue: (depth, function_id)
    let mut queue: std::collections::VecDeque<(usize, SymbolId)> =
        std::collections::VecDeque::new();
    queue.push_back((0, *function_id));

    while let Some((depth, current_fid)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let callers = match store.find_resolved_callsites_by_callee(&current_fid) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for rc in callers {
            let cs = &rc.callsite;
            let caller_sym = cs.caller;
            if visited.contains(&caller_sym) {
                continue;
            }
            visited.insert(caller_sym);
            results.push((depth + 1, caller_sym, cs.clone()));
            queue.push_back((depth + 1, caller_sym));
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use types::enums::{
        Confidence, DataNodeKind, Provenance, ReferenceKind, ResolutionStrategy, SymbolKind,
    };
    use types::ids::{CallsiteId, FileId, SymbolId};
    use types::structs::{ArgumentFact, Callsite, ReferenceUse, ResolvedTarget, TextRange};

    #[test]
    fn arkts_state_keys_match_literal_quote_styles_and_exact_expressions() {
        assert_eq!(canonical_state_key(" 'webUrl' "), ("webUrl".into(), true));
        assert_eq!(canonical_state_key("\"webUrl\""), ("webUrl".into(), true));
        assert_eq!(
            canonical_state_key("StorageKey . COLOR_MODE"),
            ("StorageKey.COLOR_MODE".into(), false)
        );
        assert_eq!(
            canonical_state_key("'page context'"),
            ("page context".into(), true)
        );
        assert_ne!(
            canonical_state_key("'StorageKey.COLOR_MODE'"),
            canonical_state_key("StorageKey.COLOR_MODE")
        );
    }

    #[test]
    fn provider_returns_empty_on_missing_data() -> anyhow::Result<()> {
        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let provider = RuntimeEdgeProvider;

        // Parameter without function_id or callers — DB has nothing
        let file_id = FileId::generate("test.ts");
        let param_id = DataNodeId::generate(&file_id, None, "param", None, None, 0);
        let edges = provider.virtual_incoming(&param_id, &store)?;
        assert!(edges.is_empty(), "non-existent param should yield no edges");

        // CallReturn without callsite_id
        let cr_id = DataNodeId::generate(&file_id, None, "call_return", None, None, 0);
        let edges = provider.virtual_incoming(&cr_id, &store)?;
        assert!(
            edges.is_empty(),
            "non-existent call return should yield no edges"
        );

        Ok(())
    }

    fn insert_fn(store: &Store, file_id: FileId, name: &str) -> SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };
        let sym = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", name, "function", None),
            kind: SymbolKind::Function,
            name: name.into(),
            qualified_name: name.into(),
            symbol_path: vec![name.into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym.clone()]).unwrap();
        sym.id
    }

    /// Focus mode (no FunctionSummary): Phase 2 runtime BFS must still emit
    /// ArgToParam edges — locks Task 6 "do not delete Phase2" contract.
    #[test]
    fn focus_mode_phase2_arg_to_param_without_summary() -> anyhow::Result<()> {
        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let file_id = FileId::generate("focus_phase2.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "focus_phase2.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let callee_id = insert_fn(&store, file_id, "callee_fp2");
        let caller_id = insert_fn(&store, file_id, "caller_fp2");
        let range = TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };

        let arg_node_id = DataNodeId::generate(
            &file_id,
            Some(&caller_id),
            "call_arg",
            Some("arg0"),
            None,
            20,
        );
        let param_id =
            DataNodeId::generate(&file_id, Some(&callee_id), "parameter", Some("y"), None, 10);

        let callee_param = types::dataflow::DataNode::parameter(
            param_id,
            file_id,
            Some(callee_id),
            None,
            "y",
            range,
        );
        let caller_arg = types::dataflow::DataNode {
            id: arg_node_id,
            file_id,
            function_id: Some(caller_id),
            kind: DataNodeKind::CallArg,
            binding_id: None,
            callsite_id: None,
            name: Some("arg0".into()),
            access_path: None,
            arg_index: Some(0),
            range,
        };
        {
            let unit_callee = types::lazy::AnalysisUnit::from_function(file_id, callee_id, range);
            store.replace_dataflow_for_unit(
                &unit_callee,
                &[callee_param],
                &[],
                &[],
                &[],
                &[],
                &[],
            )?;
            let unit_caller = types::lazy::AnalysisUnit::from_function(file_id, caller_id, range);
            store.replace_dataflow_for_unit(
                &unit_caller,
                &[caller_arg],
                &[],
                &[],
                &[],
                &[],
                &[],
            )?;
        }

        let ref_id = types::ids::ReferenceId::generate(
            &file_id,
            Some(&caller_id),
            20,
            25,
            "callee_fp2",
            ReferenceKind::Call,
        );
        let cs_id = CallsiteId::generate(&ref_id, Some(&caller_id), 20);
        store.insert_callsites(&[Callsite {
            id: cs_id,
            reference_id: Some(ref_id),
            caller: caller_id,
            receiver: None,
            args: vec![ArgumentFact {
                index: 0,
                name: None,
                value: "x".into(),
                range: None,
                data_node_id: Some(arg_node_id),
            }],
            range,
            callee_range: None,
        }])?;
        store.insert_references(&[ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol: Some(caller_id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "callee_fp2".into(),
            name: "callee_fp2".into(),
            receiver: None,
            arity: Some(1),
            range,
            binding_id: None,
            resolved: Some(ResolvedTarget {
                symbol_id: callee_id,
                confidence: Confidence::certain(),
                strategy: ResolutionStrategy::ExactMatch,
                provenance: Provenance::TreeSitter,
            }),
        }])?;

        // No FunctionSummary inserted → Phase 1 empty; Phase 2 must still bridge.
        let provider = RuntimeEdgeProvider;
        let edges = provider.virtual_incoming(&param_id, &store)?;
        assert!(
            !edges.is_empty(),
            "Focus Phase 2 must produce ArgToParam without summary"
        );
        assert!(
            edges.iter().any(|e| {
                e.kind == DataFlowKind::ArgToParam
                    && e.source_id == arg_node_id
                    && e.target_id == param_id
            }),
            "expected ArgToParam from call arg → param, got: {edges:?}"
        );
        Ok(())
    }
}
