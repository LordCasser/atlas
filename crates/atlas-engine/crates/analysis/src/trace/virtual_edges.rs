//! Virtual edges for inter-procedural dataflow tracing.
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
//!
//! The [`SummaryEdgeProvider`] uses [`super::summary::FunctionSummary`] to
//! bridge these gaps.  When no summary exists yet it falls back to
//! direct-join heuristics (match callsite callee symbol → callee params;
//! match call-arg DataNode → callee param by index).

use db::TraceStore;
use types::dataflow::DataFlowEdge;
use types::enums::{DataFlowKind, DataNodeKind};
use types::ids::{DataNodeId, SymbolId};
use types::structs::Callsite;

// ---------------------------------------------------------------------------
// TraceEdge — a cross-boundary dataflow connection
// ---------------------------------------------------------------------------

/// A virtual edge that connects data nodes across function boundaries.
///
/// Unlike [`DataFlowEdge`] which stays within a single function, `TraceEdge`
/// bridges caller-callee or callee-caller transitions needed for
/// inter-procedural backward tracing.
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

/// Provider of virtual inter-procedural trace edges.
///
/// Implementations bridge the gap between intra-procedural dataflow (which
/// stays within a single function) and cross-function tracing.
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
// SummaryEdgeProvider — bridges using FunctionSummary
// ---------------------------------------------------------------------------

/// Bridges call boundaries using [`FunctionSummary`] and direct DB joins.
///
/// ## Strategy (in priority order)
///
/// 1. **Parameter node** → find callers via `callsites_by_callee`, match
///    each caller's call-arg DataNode to this parameter by arg_index.
/// 2. **CallReturn / Expr nodes with callsite_id** → find the callee
///    function, look up its summary, connect each ReturnFlow source back
///    to this call-result node.
/// 3. **Direct callsite join** — when summary is unavailable, match
///    caller call-arg DataNodes to callee parameters using the existing
///    `callsite_id` on DataNodes and the callsite's callee symbol.
pub struct SummaryEdgeProvider;

impl TraceEdgeProvider for SummaryEdgeProvider {
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let target_node = match store.get_data_node(target_id)? {
            Some(n) => n,
            None => return Ok(vec![]),
        };

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
            return Ok(bridge_edges);
        }

        // ── Phase 2: fallback to existing runtime BFS logic ──
        let mut edges: Vec<TraceEdge> = Vec::new();

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

    #[test]
    fn provider_returns_empty_on_missing_data() -> anyhow::Result<()> {
        use types::ids::FileId;

        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let provider = SummaryEdgeProvider;

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
}
